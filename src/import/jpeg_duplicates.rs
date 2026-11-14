use super::*;
use crate::{
    catalog::*,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::{RootHandle, RootPolicy},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JpegCandidate {
    pub group: GroupId,
    pub path: ArchivePath,
    pub digest: Digest,
    pub size: u64,
}

#[cfg(test)]
mod performance_tests {
    use super::*;
    use crate::config::Config;

    unsafe extern "C" fn consume_budget(context: *mut std::ffi::c_void) -> libc::c_int {
        let remaining = unsafe { &mut *context.cast::<u32>() };
        if *remaining == 0 {
            1
        } else {
            *remaining -= 1;
            0
        }
    }

    #[test]
    fn canonical_image_lookup_work_is_bounded_by_matching_fingerprints() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let state = temp.path().join("state");
        let local = temp.path().join("local");
        for path in [&source, &state, &local] {
            std::fs::create_dir(path).unwrap();
        }
        let root = RootHandle::open(&state, RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let archive = ArchiveId::new();
        let group = catalog.create_group(true).unwrap();
        let card = include_bytes!("../../tests/fixtures/jpeg/red.jpg");
        let mut nas = card[..2].to_vec();
        nas.extend_from_slice(b"\xff\xfe\x00\x09catalog");
        nas.extend_from_slice(&card[2..]);
        let card_inspection = inspect_jpeg(card.as_slice()).unwrap();
        let nas_inspection = inspect_jpeg(nas.as_slice()).unwrap();
        catalog.cache_jpeg_inspection(&card_inspection).unwrap();
        catalog.cache_jpeg_inspection(&nas_inspection).unwrap();
        catalog
            .attach_member(
                group,
                OriginalMember {
                    digest: nas_inspection.digest.clone(),
                    size: nas_inspection.size,
                    path: ArchivePath::parse("JPEG/2026/05/15/A.JPG").unwrap(),
                    role: MediaRole::Jpeg,
                },
                PairEvidence::UserSelected {
                    reason: "canonical archive fixture".into(),
                },
            )
            .unwrap();
        let tx = catalog.db.transaction().unwrap();
        tx.execute("WITH RECURSIVE n(value) AS(VALUES(1) UNION ALL SELECT value+1 FROM n WHERE value<10000) INSERT INTO blobs(digest,size) SELECT 'blake3:'||printf('%064x',value),1 FROM n",[]).unwrap();
        tx.execute("INSERT INTO members(archive,path,group_id,digest,role) SELECT ?,printf('JPEG/unrelated/%s.JPG',digest),?,digest,? FROM blobs WHERE size=1",params![archive.to_string(),group.to_string(),serde_json::to_string(&MediaRole::Jpeg).unwrap()]).unwrap();
        tx.execute(
            "UPDATE members SET archive=? WHERE archive='unbound'",
            [archive.to_string()],
        )
        .unwrap();
        tx.execute("INSERT INTO placements(id,group_id,path,digest,archived) SELECT path,group_id,path,digest,1 FROM members",[]).unwrap();
        tx.execute("INSERT INTO jpeg_fingerprints(digest,algorithm,fingerprint) SELECT digest,?,? FROM blobs WHERE size=1",params![JPEG_FINGERPRINT_VERSION,Digest::of(b"unrelated image").as_str()]).unwrap();
        tx.commit().unwrap();
        std::fs::write(source.join("A.JPG"), card).unwrap();
        let mut inventory =
            Inventory::open(RootHandle::open(&source, RootPolicy::Local).unwrap()).unwrap();
        let entry = inventory.next_batch(1).unwrap().remove(0);
        let mut config = Config::new(local, state, temp.path().join("archive"));
        config.archive_id = Some(archive);
        let engine = ImportCoordinator::new(config, catalog);
        let mut remaining = 20u32;
        let handle = unsafe { engine.catalog.db.handle() };
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                handle,
                1000,
                Some(consume_budget),
                std::ptr::from_mut(&mut remaining).cast(),
            );
        }
        let result = engine.canonical_archived_original(&entry, &card_inspection.digest, false);
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(handle, 0, None, std::ptr::null_mut());
        }
        assert!(
            matches!(result.as_ref(),Ok(CanonicalSelection::Image(candidate,_)) if candidate.path.as_str()=="JPEG/2026/05/15/A.JPG"),
            "canonical lookup exceeded its catalogue-work budget or lost its match: {:?}",
            result.as_ref().err()
        );
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JpegCheck {
    pub candidate: JpegCandidate,
    pub source_image: Digest,
    pub source_stamp: String,
    pub group: GroupId,
    pub member: OriginalMember,
    pub facts: PairFacts,
}
pub(crate) enum JpegSelection {
    None,
    Compare(Box<JpegCheck>),
    Ambiguous,
}
pub(crate) enum CanonicalSelection {
    None,
    Exact(JpegCandidate),
    Image(JpegCandidate, Digest),
    Ambiguous,
}

pub(crate) fn jpeg_file_stamp(metadata: &std::fs::Metadata) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}",
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}
impl ImportCoordinator {
    pub(crate) fn canonical_archived_original(
        &self,
        entry: &SourceEntry,
        digest: &Digest,
        strict: bool,
    ) -> AppResult<CanonicalSelection> {
        let Some(archive) = self.config.archive_id else {
            return Ok(CanonicalSelection::None);
        };
        let SourceRole::Original(role) = entry.role else {
            return Ok(CanonicalSelection::None);
        };
        let image = if role == MediaRole::Jpeg {
            self.catalog.jpeg_fingerprint(digest)?.flatten()
        } else {
            None
        };
        if role == MediaRole::Jpeg {
            let mut query = self.catalog.db.prepare("SELECT DISTINCT m.group_id,m.path,m.digest,b.size,f.digest,f.fingerprint FROM retired_import_plans r JOIN members m ON m.path=json_extract(r.previous,'$.destination') JOIN blobs b ON b.digest=m.digest LEFT JOIN jpeg_fingerprints f ON f.digest=m.digest AND f.algorithm=? WHERE json_extract(r.previous,'$.digest')=? AND m.archive=? AND m.role=? AND m.digest<>? AND EXISTS(SELECT 1 FROM placements p WHERE p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest AND p.archived=1) LIMIT 129")?;
            let preferred = query
                .query_map(
                    params![
                        JPEG_FINGERPRINT_VERSION,
                        digest.as_str(),
                        archive.to_string(),
                        serde_json::to_string(&role)?,
                        digest.as_str()
                    ],
                    |row| {
                        Ok((
                            (
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, u64>(3)?,
                            ),
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            if preferred.len() > 128 {
                return Ok(CanonicalSelection::Ambiguous);
            }
            if !preferred.is_empty() {
                if strict
                    || image.is_none()
                    || preferred.iter().any(|(_, cached, _)| cached.is_none())
                {
                    return Ok(CanonicalSelection::None);
                }
                let image = image.as_ref().unwrap();
                let mut matches = preferred
                    .into_iter()
                    .filter_map(|(candidate, _, fingerprint)| {
                        (fingerprint.as_deref() == Some(image.as_str())).then_some(candidate)
                    })
                    .collect::<Vec<_>>();
                return Ok(match matches.len() {
                    0 => CanonicalSelection::None,
                    1 => CanonicalSelection::Image(
                        self.archived_candidate(matches.pop().unwrap())?,
                        image.clone(),
                    ),
                    _ => CanonicalSelection::Ambiguous,
                });
            }
        }
        let mut query = self.catalog.db.prepare(
            "SELECT m.group_id,m.path,m.digest,b.size FROM members m JOIN blobs b ON b.digest=m.digest WHERE m.archive=? AND m.role=? AND m.digest=? AND EXISTS(SELECT 1 FROM placements p WHERE p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest AND p.archived=1) LIMIT 2")?;
        let mut candidates = query
            .query_map(
                params![
                    archive.to_string(),
                    serde_json::to_string(&role)?,
                    digest.as_str()
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, u64>(3)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let exact = !candidates.is_empty();
        if !exact
            && !strict
            && let Some(image) = &image
        {
            let mut query = self.catalog.db.prepare(
                "SELECT m.group_id,m.path,m.digest,b.size FROM jpeg_fingerprints f CROSS JOIN members m ON m.digest=f.digest CROSS JOIN blobs b ON b.digest=m.digest WHERE f.algorithm=? AND f.fingerprint=? AND m.archive=? AND m.role=? AND EXISTS(SELECT 1 FROM placements p WHERE p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest AND p.archived=1) LIMIT 2")?;
            candidates = query
                .query_map(
                    params![
                        JPEG_FINGERPRINT_VERSION,
                        image.as_str(),
                        archive.to_string(),
                        serde_json::to_string(&role)?
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, u64>(3)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
        }
        if candidates.len() > 1 {
            if exact {
                let metadata = self.catalog.metadata(digest)?.unwrap_or_else(|| {
                    MetadataResult::from_json(digest.clone(), serde_json::Value::Null)
                });
                let mut capture = CaptureRecord::new(
                    OriginalMember {
                        digest: digest.clone(),
                        size: entry.size,
                        path: entry.path.clone(),
                        role,
                    },
                    metadata,
                );
                capture.source_mtime = Some(entry.modified);
                let ordinary = plan_placements(
                    &[PlannedGroup {
                        id: GroupId::new(),
                        records: vec![capture],
                        sequence: None,
                    }],
                    &self.catalog,
                    &self.config.import_timezone,
                )?
                .remove(0)
                .member
                .path;
                let candidate=self.catalog.db.query_row("SELECT m.group_id,m.path,m.digest,b.size FROM members m JOIN blobs b ON b.digest=m.digest WHERE m.archive=? AND m.path=? AND m.role=? AND m.digest=? AND EXISTS(SELECT 1 FROM placements p WHERE p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest AND p.archived=1)",params![archive.to_string(),ordinary.as_str(),serde_json::to_string(&role)?,digest.as_str()],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,u64>(3)?))).optional()?;
                if let Some(candidate) = candidate {
                    return Ok(CanonicalSelection::Exact(
                        self.archived_candidate(candidate)?,
                    ));
                }
            }
            return Ok(CanonicalSelection::Ambiguous);
        }
        let Some(candidate) = candidates.pop() else {
            return Ok(CanonicalSelection::None);
        };
        let candidate = self.archived_candidate(candidate)?;
        Ok(if exact {
            CanonicalSelection::Exact(candidate)
        } else {
            CanonicalSelection::Image(candidate, image.unwrap())
        })
    }
    fn archived_candidate(
        &self,
        (group, path, digest, size): (String, String, String, u64),
    ) -> AppResult<JpegCandidate> {
        Ok(JpegCandidate {
            group: self
                .catalog
                .canonical_group(serde_json::from_value(group.into())?)?,
            path: ArchivePath::parse(&path)?,
            digest: Digest::parse(&digest)?,
            size,
        })
    }
    pub(crate) fn account_canonical_original(
        &mut self,
        job: JobId,
        entry: &SourceEntry,
        digest: &Digest,
        selection: CanonicalSelection,
    ) -> AppResult<bool> {
        let (candidate, image, conflict) = match selection {
            CanonicalSelection::None => return Ok(false),
            CanonicalSelection::Exact(candidate) => (Some(candidate), None, false),
            CanonicalSelection::Image(candidate, image) => (Some(candidate), Some(image), false),
            CanonicalSelection::Ambiguous => (None, None, true),
        };
        let child = JobId::new();
        let destination = candidate
            .as_ref()
            .map(|value| &value.path)
            .unwrap_or(&entry.path);
        let group = candidate.as_ref().map(|value| value.group);
        if conflict {
            self.catalog.record_ambiguity("original_collision",None,serde_json::json!({"digest":digest,"source":entry.path,"reason":"multiple canonical archived placements"}))?;
        }
        let tx = self.catalog.db.transaction()?;
        tx.execute("INSERT INTO import_items(id,parent,source_root,source_path,digest,size,role,group_id,destination,state,local_ready,archived,warning) VALUES(?,?,?,?,?,?,?,?,?,?,0,?,0)",params![child.to_string(),job.to_string(),entry.root.path().to_str().ok_or_else(||AppError::invalid("UTF-8 source root required"))?,entry.path.as_str(),digest.as_str(),entry.size,serde_json::to_string(&entry.role)?,group.map(|id|id.to_string()),destination.as_str(),if conflict {"conflict"} else if image.is_some() {"image_duplicate"} else {"accounted"},!conflict && image.is_none()])?;
        if let (Some(candidate), Some(image)) = (&candidate, &image) {
            tx.execute("INSERT INTO jpeg_duplicates(item,archive,canonical_group,canonical_path,canonical_digest,canonical_size,image) VALUES(?,?,?,?,?,?,?)",params![child.to_string(),self.config.archive_id.unwrap().to_string(),candidate.group.to_string(),candidate.path.as_str(),candidate.digest.as_str(),candidate.size,image.as_str()])?;
        }
        tx.execute(
            "UPDATE import_stats SET selected=selected+1,recognized=recognized+? WHERE job=?",
            params![u64::from(!conflict), job.to_string()],
        )?;
        tx.commit()?;
        Ok(true)
    }
    pub(crate) fn prepare_jpeg_check(
        &self,
        entry: &SourceEntry,
        member: &OriginalMember,
        original: &ArchivePath,
        group: GroupId,
        facts: &PairFacts,
    ) -> AppResult<JpegSelection> {
        let Some(archive) = self.config.archive_id else {
            return Ok(JpegSelection::None);
        };
        let Some(Some(source_image)) = self.catalog.jpeg_fingerprint(&member.digest)? else {
            return Ok(JpegSelection::None);
        };
        let mut paths = BTreeSet::from([original.as_str().to_owned()]);
        let mut query = self.catalog.db.prepare("SELECT DISTINCT json_extract(r.previous,'$.destination') FROM retired_import_plans r JOIN import_items i ON i.id=r.replacement WHERE json_extract(r.previous,'$.digest')=? AND i.group_id=?")?;
        for path in query.query_map(params![member.digest.as_str(), group.to_string()], |row| {
            row.get::<_, String>(0)
        })? {
            paths.insert(path?);
        }
        if !facts.camera.is_empty() && !facts.capture.is_empty() {
            let filename = entry
                .path
                .as_str()
                .rsplit('/')
                .next()
                .ok_or_else(|| AppError::invalid("JPEG source filename missing"))?;
            let suffix = format!("/{filename}");
            let mut query = self.catalog.db.prepare("SELECT m.path FROM members m JOIN metadata_cache c ON c.digest=m.digest WHERE m.archive=? AND m.role=? AND c.extractor=? AND json_extract(c.metadata,'$.camera')=? AND json_extract(c.metadata,'$.capture')=? AND (m.path=? OR substr(m.path,-?)=?) AND EXISTS(SELECT 1 FROM placements p WHERE p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest AND p.archived=1) LIMIT 2")?;
            for path in query.query_map(
                params![
                    archive.to_string(),
                    serde_json::to_string(&MediaRole::Jpeg)?,
                    EXTRACTOR_VERSION,
                    facts.camera,
                    facts.capture,
                    filename,
                    suffix.chars().count(),
                    suffix
                ],
                |row| row.get::<_, String>(0),
            )? {
                paths.insert(path?);
            }
        }
        let mut candidates = Vec::new();
        let planned_path = member.path.as_str().to_owned();
        for path in paths
            .iter()
            .chain(std::iter::once(&planned_path))
            .collect::<BTreeSet<_>>()
        {
            if path.as_str() == member.path.as_str()
                && paths.iter().any(|other| other.as_str() != path.as_str())
            {
                continue;
            }
            let row: Option<(String,String,u64)> = self.catalog.db.query_row("SELECT m.group_id,m.digest,b.size FROM members m JOIN blobs b ON b.digest=m.digest WHERE m.archive=? AND m.path=? AND m.role=? AND EXISTS(SELECT 1 FROM placements p WHERE p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest AND p.archived=1)",params![archive.to_string(),path,serde_json::to_string(&MediaRole::Jpeg)?],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
            if let Some((id, digest, size)) = row {
                candidates.push(JpegCandidate {
                    group: self
                        .catalog
                        .canonical_group(serde_json::from_value(id.into())?)?,
                    path: ArchivePath::parse(path)?,
                    digest: Digest::parse(&digest)?,
                    size,
                });
            }
        }
        if candidates.len() > 1 {
            return Ok(JpegSelection::Ambiguous);
        }
        let Some(candidate) = candidates.pop() else {
            return Ok(JpegSelection::None);
        };
        if candidate.path == member.path && candidate.digest == member.digest {
            return Ok(JpegSelection::None);
        }
        Ok(JpegSelection::Compare(Box::new(JpegCheck {
            candidate,
            source_image,
            source_stamp: format!(
                "{}:{}:{}:{}",
                entry.fingerprint, entry.inode, entry.ctime.0, entry.ctime.1
            ),
            group,
            member: member.clone(),
            facts: facts.clone(),
        })))
    }
    pub(crate) fn pending_jpeg_check(&self, item: JobId) -> AppResult<Option<JpegCheck>> {
        let text:Option<String>=self.catalog.db.query_row("SELECT c.payload FROM jpeg_checks c JOIN import_items i ON i.id=c.item WHERE c.item=? AND i.state='jpeg_pending'",[item.to_string()],|row|row.get(0)).optional()?;
        text.map(|text| Ok(serde_json::from_str(&text)?))
            .transpose()
    }
    pub(crate) fn finish_jpeg_check(
        &mut self,
        item: &super::runner::ImportItem,
        inspection: &JpegInspection,
    ) -> AppResult<()> {
        let result = self.apply_jpeg_check(item, inspection);
        if result.as_ref().is_err_and(|error| {
            matches!(
                error.code,
                ErrorCode::Conflict | ErrorCode::Integrity | ErrorCode::SourceChanged
            )
        }) {
            let tx = self.catalog.db.transaction()?;
            tx.execute(
                "UPDATE import_items SET state='conflict' WHERE id=? AND state='jpeg_pending'",
                [item.id.to_string()],
            )?;
            tx.execute(
                "DELETE FROM jpeg_checks WHERE item=?",
                [item.id.to_string()],
            )?;
            tx.commit()?;
        }
        result
    }
    fn apply_jpeg_check(
        &mut self,
        item: &super::runner::ImportItem,
        inspection: &JpegInspection,
    ) -> AppResult<()> {
        let check = self
            .pending_jpeg_check(item.id)?
            .ok_or_else(|| AppError::integrity("JPEG comparison lost its pending item"))?;
        if inspection.digest != check.candidate.digest || inspection.size != check.candidate.size {
            return Err(AppError::integrity(
                "JPEG comparison differs from archived byte identity",
            ));
        }
        let archive = self
            .config
            .archive_id
            .ok_or_else(|| AppError::integrity("JPEG comparison lost archive identity"))?;
        let valid:bool=self.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM members m JOIN placements p ON p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest WHERE m.archive=? AND m.group_id=? AND m.path=? AND m.digest=? AND p.archived=1)",params![archive.to_string(),check.candidate.group.to_string(),check.candidate.path.as_str(),check.candidate.digest.as_str()],|row|row.get(0))?;
        if !valid {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "JPEG archive candidate changed during comparison",
            ));
        }
        self.catalog.cache_jpeg_inspection(inspection)?;
        let source = RootHandle::open(&item.source.root, RootPolicy::Local)?;
        let file = source.open_relative(&item.source.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        if !file.metadata()?.is_file() || jpeg_file_stamp(&file.metadata()?) != check.source_stamp {
            return Err(AppError::new(
                ErrorCode::SourceChanged,
                "JPEG source changed during duplicate comparison",
            ));
        }
        source.validate_identity()?;
        if inspection.image.as_ref() == Some(&check.source_image) {
            let tx = self.catalog.db.transaction()?;
            tx.execute("INSERT INTO jpeg_duplicates(item,archive,canonical_group,canonical_path,canonical_digest,canonical_size,image) VALUES(?,?,?,?,?,?,?)",params![item.id.to_string(),archive.to_string(),check.candidate.group.to_string(),check.candidate.path.as_str(),check.candidate.digest.as_str(),check.candidate.size,check.source_image.as_str()])?;
            let changed=tx.execute("UPDATE import_items SET state='image_duplicate',archived=0,local_ready=0 WHERE id=? AND state='jpeg_pending'",[item.id.to_string()])?;
            if changed != 1 {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "JPEG duplicate acknowledgement is obsolete",
                ));
            }
            if !check.facts.camera.is_empty() && !check.facts.capture.is_empty() {
                tx.execute("INSERT INTO group_facts(context,stem,camera,capture,group_id) VALUES(?,?,?,?,?) ON CONFLICT DO NOTHING",params![check.facts.context,check.facts.stem,check.facts.camera,check.facts.capture,check.candidate.group.to_string()])?;
            }
            tx.execute("UPDATE import_stats SET recognized=recognized+1 WHERE job=(SELECT parent FROM import_items WHERE id=?)",[item.id.to_string()])?;
            tx.execute(
                "DELETE FROM jpeg_checks WHERE item=?",
                [item.id.to_string()],
            )?;
            tx.commit()?;
        } else {
            if !check.facts.camera.is_empty() && !check.facts.capture.is_empty() {
                self.catalog.db.execute("INSERT INTO group_facts(context,stem,camera,capture,group_id) VALUES(?,?,?,?,?) ON CONFLICT DO NOTHING", params![check.facts.context,check.facts.stem,check.facts.camera,check.facts.capture,check.group.to_string()])?;
            }
            let occupied: Option<String> = self
                .catalog
                .db
                .query_row(
                    "SELECT digest FROM members WHERE archive=? AND path=?",
                    params![archive.to_string(), check.member.path.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            if occupied
                .as_deref()
                .is_some_and(|digest| digest != check.member.digest.as_str())
            {
                self.catalog.record_ambiguity(
                    "original_collision",
                    Some(check.group),
                    serde_json::to_value(&check.member)?,
                )?;
                self.catalog.db.execute(
                    "UPDATE import_items SET state='conflict' WHERE id=?",
                    [item.id.to_string()],
                )?;
            } else {
                self.catalog.attach_member(
                    check.group,
                    check.member.clone(),
                    PairEvidence::ObservedPlacement {
                        path: check.member.path.clone(),
                        digest: check.member.digest.clone(),
                    },
                )?;
                let archived:bool=self.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM placements WHERE group_id=? AND path=? AND digest=? AND archived=1)",params![check.group.to_string(),check.member.path.as_str(),check.member.digest.as_str()],|row|row.get(0))?;
                let tx = self.catalog.db.transaction()?;
                tx.execute(
                    "UPDATE import_items SET state=?,archived=? WHERE id=?",
                    params![
                        if archived { "accounted" } else { "pending" },
                        archived,
                        item.id.to_string()
                    ],
                )?;
                if archived {
                    tx.execute("UPDATE import_stats SET recognized=recognized+1 WHERE job=(SELECT parent FROM import_items WHERE id=?)", [item.id.to_string()])?;
                }
                tx.commit()?;
            }
            self.catalog.db.execute(
                "DELETE FROM jpeg_checks WHERE item=?",
                [item.id.to_string()],
            )?;
        }
        Ok(())
    }
}
