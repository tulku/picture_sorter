use super::*;
use crate::{
    archive::{ArchiveRecord, CommitEnvelope, ProjectionPlan},
    domain::*,
};
use rusqlite::{OptionalExtension, params};

impl Catalog {
    pub(crate) fn revalidate_queued_resolutions(
        &self,
        records: &mut Vec<ArchiveRecord>,
    ) -> AppResult<()> {
        let mut obsolete = std::collections::HashSet::new();
        for record in records.iter() {
            if let ArchiveRecord::ConflictResolved {
                id,
                expected_revision,
                version,
            } = record
            {
                let conflict = self.conflict(*id)?;
                if conflict.resolved {
                    obsolete.insert(*id);
                } else if conflict.revision != *expected_revision {
                    return Err(AppError::new(
                        crate::error::ErrorCode::Conflict,
                        "queued resolution revision changed",
                    ));
                } else if conflict.kind == "nas_projection" {
                    let sidecar = self.version(*version)?.sidecar;
                    if self.heads(sidecar)? != vec![*version] {
                        obsolete.insert(*id);
                    }
                }
            }
        }
        records.retain(|record| match record {
            ArchiveRecord::ConflictResolved { id, .. }
            | ArchiveRecord::AmbiguityRecorded { id, .. } => !obsolete.contains(id),
            _ => true,
        });
        Ok(())
    }

    pub(crate) fn archived_ancestor_body(
        &self,
        sidecar: SidecarId,
        head: VersionId,
        body: &VersionBody,
    ) -> AppResult<bool> {
        if !matches!(body, VersionBody::Content { .. }) {
            return Ok(false);
        }
        Ok(self.db.query_row("WITH RECURSIVE ancestors(id) AS (VALUES(?1) UNION SELECT p.parent FROM version_parents p JOIN ancestors a ON p.version=a.id) SELECT EXISTS(SELECT 1 FROM ancestors a JOIN versions v ON v.id=a.id JOIN archived_versions av ON av.version=v.id WHERE v.sidecar=?2 AND v.body=?3)", params![head.to_string(),sidecar.to_string(),serde_json::to_string(body)?],|r|r.get(0))?)
    }

    pub(crate) fn committed_projection_successor(
        &self,
        archive: ArchiveId,
        sidecar: SidecarId,
        head: VersionId,
        observed: &VersionBody,
    ) -> AppResult<Option<(CommitEnvelope, ProjectionPlan)>> {
        if self.heads(sidecar)? != vec![head]
            || !self.archived_ancestor_body(sidecar, head, observed)?
        {
            return Ok(None);
        }
        let (path, owner): (String, String) = self.db.query_row(
            "SELECT path,owner FROM sidecars WHERE id=?",
            [sidecar.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if owner == GENERATED_OUTPUT_OWNER {
            return Ok(None);
        }
        let mut query = self.db.prepare("WITH RECURSIVE ancestors(id) AS (VALUES(?1) UNION SELECT p.parent FROM version_parents p JOIN ancestors a ON p.version=a.id) SELECT DISTINCT v.id,c.envelope FROM ancestors a JOIN versions v ON v.id=a.id JOIN archived_versions av ON av.version=v.id JOIN version_parents p ON p.version=v.id JOIN versions previous ON previous.id=p.parent JOIN projection_commits pc ON pc.sidecar=v.sidecar AND pc.version=v.id JOIN commits c ON c.id=pc.commit_id WHERE v.sidecar=?2 AND previous.sidecar=?2 AND previous.body=?3 AND c.applied=1 LIMIT 129")?;
        let rows = query
            .query_map(
                params![
                    head.to_string(),
                    sidecar.to_string(),
                    serde_json::to_string(observed)?
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() > 128 {
            return Ok(None);
        }
        let mut result = None;
        for (version, envelope) in rows {
            let version: VersionId = version.parse()?;
            let commit = CommitEnvelope::decode(&envelope, archive)?;
            for record in commit.records()? {
                if let ArchiveRecord::ProjectionPlanned { plan } = record
                    && plan.sidecar == sidecar
                    && plan.version == version
                    && plan.expected == *observed
                    && plan.body != *observed
                    && plan.path.as_str() == path
                    && plan.owner == owner
                    && self.version(version)?.body == plan.body
                {
                    if result.is_some() {
                        return Ok(None);
                    }
                    result = Some((commit.clone(), plan));
                }
            }
        }
        Ok(result)
    }

    pub(crate) fn queue_stale_projection_resolutions(
        &self,
        sidecar: SidecarId,
        head: VersionId,
    ) -> AppResult<()> {
        if self.heads(sidecar)? != vec![head] {
            return Ok(());
        }
        let mut query = self.db.prepare("SELECT id FROM conflicts WHERE kind='nas_projection' AND resolved=0 AND json_extract(details,'$.sidecar')=? AND NOT EXISTS(SELECT 1 FROM pending_shared_records p,json_each(p.records) e WHERE p.committed=0 AND json_extract(e.value,'$.type')='conflict_resolved' AND json_extract(e.value,'$.id')=conflicts.id) ORDER BY id LIMIT 128")?;
        let ids = query
            .query_map([sidecar.to_string()], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut records = Vec::new();
        for id in ids {
            let conflict = self.conflict(id.parse()?)?;
            if !conflict.details["preserved"].is_null() {
                continue;
            }
            let Ok(version) =
                serde_json::from_value::<VersionId>(conflict.details["version"].clone())
            else {
                continue;
            };
            let Ok(observed) =
                serde_json::from_value::<VersionBody>(conflict.details["observed"].clone())
            else {
                continue;
            };
            if !self.is_ancestor(version, head)?
                || !self.archived_ancestor_body(sidecar, head, &observed)?
            {
                continue;
            }
            let shared: bool = self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM published_conflicts WHERE id=?)",
                [&id],
                |r| r.get(0),
            )?;
            if !shared {
                if let Some(group) = conflict.group {
                    records.push(ArchiveRecord::GroupCreated { id: group });
                }
                records.push(ArchiveRecord::AmbiguityRecorded {
                    id: conflict.id,
                    kind: conflict.kind,
                    group: conflict.group,
                    details: conflict.details,
                });
            }
            records.push(ArchiveRecord::ConflictResolved {
                id: conflict.id,
                expected_revision: conflict.revision,
                version: head,
            });
        }
        if !records.is_empty() {
            self.db.execute(
                "INSERT INTO pending_shared_records(operation,records) VALUES(?,?)",
                params![
                    OperationId::new().to_string(),
                    serde_json::to_string(&records)?
                ],
            )?;
        }
        Ok(())
    }

    pub(crate) fn conflict_path(&self, conflict: &ConflictRecord) -> AppResult<Option<String>> {
        if let Some(sidecar) = conflict.details["sidecar"].as_str() {
            let path = self
                .db
                .query_row("SELECT path FROM sidecars WHERE id=?", [sidecar], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?;
            if path.is_some() {
                return Ok(path);
            }
        }
        if let Some(path) = conflict.details["path"]
            .as_str()
            .or_else(|| conflict.details["destination"].as_str())
        {
            return Ok(Some(path.to_owned()));
        }
        if let Some(group) = conflict.group {
            return Ok(self
                .db
                .query_row(
                    "SELECT path FROM members WHERE group_id=? ORDER BY path LIMIT 1",
                    [group.to_string()],
                    |r| r.get(0),
                )
                .optional()?);
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(bytes: &[u8]) -> VersionBody {
        VersionBody::Content {
            digest: Digest::of(bytes),
            size: bytes.len() as u64,
        }
    }

    #[test]
    fn stale_reports_are_journaled_once_without_clearing_unknown_or_raced_edits() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), crate::fs::RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let archive = ArchiveId::new();
        let writer = WriterId::new();
        let group = GroupId::new();
        let sidecar = SidecarId::new();
        let first = VersionId::new();
        let head = VersionId::new();
        let branch = VersionId::new();
        let first_body = body(b"first");
        let first_commit = CommitEnvelope::new(
            archive,
            writer,
            1,
            vec![],
            vec![
                ArchiveRecord::GroupCreated { id: group },
                ArchiveRecord::SidecarCreated {
                    id: sidecar,
                    group: Some(group),
                    path: ArchivePath::parse("photo.ORF.rrdata").unwrap(),
                    owner: "rapidraw-base".into(),
                },
                ArchiveRecord::VersionAdded {
                    version: Version {
                        id: first,
                        sidecar,
                        parents: vec![],
                        body: first_body.clone(),
                        origin: VersionOrigin::Observed,
                    },
                    operation: OperationId::new(),
                },
                ArchiveRecord::HeadsChanged {
                    sidecar,
                    expected: vec![],
                    heads: vec![first],
                },
            ],
        )
        .unwrap();
        catalog.apply_commit(&first_commit).unwrap();
        let second = CommitEnvelope::new(
            archive,
            writer,
            2,
            vec![first_commit.commit_id],
            vec![
                ArchiveRecord::VersionAdded {
                    version: Version {
                        id: head,
                        sidecar,
                        parents: vec![first],
                        body: body(b"latest"),
                        origin: VersionOrigin::Observed,
                    },
                    operation: OperationId::new(),
                },
                ArchiveRecord::VersionAdded {
                    version: Version {
                        id: branch,
                        sidecar,
                        parents: vec![first],
                        body: body(b"other branch"),
                        origin: VersionOrigin::Observed,
                    },
                    operation: OperationId::new(),
                },
                ArchiveRecord::HeadsChanged {
                    sidecar,
                    expected: vec![first],
                    heads: vec![head],
                },
                ArchiveRecord::ProjectionPlanned {
                    plan: ProjectionPlan {
                        sidecar,
                        path: ArchivePath::parse("photo.ORF.rrdata").unwrap(),
                        version: head,
                        expected: first_body.clone(),
                        body: body(b"latest"),
                        owner: "rapidraw-base".into(),
                    },
                },
            ],
        )
        .unwrap();
        catalog.apply_commit(&second).unwrap();
        assert!(
            catalog
                .committed_projection_successor(archive, sidecar, head, &first_body)
                .unwrap()
                .is_some()
        );
        assert!(
            catalog
                .committed_projection_successor(archive, sidecar, head, &body(b"unknown"))
                .unwrap()
                .is_none()
        );
        let mut ids = Vec::new();
        for (version, observed, preserved) in [
            (head, first_body.clone(), serde_json::Value::Null),
            (head, body(b"unknown"), serde_json::Value::Null),
            (branch, first_body.clone(), serde_json::Value::Null),
            (
                head,
                first_body.clone(),
                serde_json::json!(".photo-sorter/staging/preserved"),
            ),
        ] {
            ids.push(catalog.record_ambiguity("nas_projection",Some(group),serde_json::json!({"sidecar":sidecar,"version":version,"observed":observed,"preserved":preserved})).unwrap());
        }
        catalog
            .queue_stale_projection_resolutions(sidecar, head)
            .unwrap();
        catalog
            .queue_stale_projection_resolutions(sidecar, head)
            .unwrap();
        let count: u64 = catalog
            .db
            .query_row("SELECT count(*) FROM pending_shared_records", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(catalog.conflicts(false).unwrap().len(), 4);
        let text: String = catalog
            .db
            .query_row("SELECT records FROM pending_shared_records", [], |r| {
                r.get(0)
            })
            .unwrap();
        let records: Vec<ArchiveRecord> = serde_json::from_str(&text).unwrap();
        let resolutions = records
            .iter()
            .filter_map(|r| {
                if let ArchiveRecord::ConflictResolved { id, .. } = r {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(resolutions, vec![ids[0]]);
        let mut queued = records.clone();
        let commit =
            CommitEnvelope::new(archive, writer, 3, vec![second.commit_id], records).unwrap();
        catalog.apply_commit(&commit).unwrap();
        catalog.revalidate_queued_resolutions(&mut queued).unwrap();
        assert!(!queued.iter().any(|r| matches!(
            r,
            ArchiveRecord::ConflictResolved { .. } | ArchiveRecord::AmbiguityRecorded { .. }
        )));
        let replay =
            CommitEnvelope::new(archive, WriterId::new(), 1, vec![commit.commit_id], queued)
                .unwrap();
        catalog.apply_commit(&replay).unwrap();
        assert!(catalog.conflict(ids[0]).unwrap().resolved);
        for id in &ids[1..] {
            assert!(!catalog.conflict(*id).unwrap().resolved);
        }
        let config = crate::config::Config::new("/local".into(), "/state".into(), "/nas".into());
        let display = catalog
            .describe_conflict(&catalog.conflict(ids[1]).unwrap(), &config)
            .unwrap();
        assert_eq!(display["path"], "photo.ORF.rrdata");
        assert_eq!(display["backup_behind"], false);
    }
}
