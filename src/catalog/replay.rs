use super::*;
use crate::{archive::*, domain::*};
use rusqlite::{OptionalExtension, params};
type SharedAssetBinding = (
    Option<String>,
    String,
    Option<String>,
    String,
    String,
    String,
);
#[derive(Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    AlreadyApplied,
    Deferred,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum PublicationReadiness {
    Ready,
    Blocked(Vec<String>),
}
use serde::{Deserialize, Serialize};
impl Catalog {
    pub fn commit_applied(&self, id: CommitId) -> AppResult<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM commits WHERE id=? AND applied=1)",
            [id.to_string()],
            |r| r.get(0),
        )?)
    }
    pub fn block_record(&mut self, id: &str, reason: &str) -> AppResult<()> {
        self.db.execute("INSERT INTO blocked_records(id,reason) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET reason=excluded.reason",params![id,reason])?;
        Ok(())
    }
    pub fn observe_commit(&mut self, bytes: &[u8], archive: ArchiveId) -> AppResult<ApplyOutcome> {
        match CommitEnvelope::decode(bytes, archive) {
            Ok(c) => self.apply_commit(&c),
            Err(e) => {
                self.block_record(Digest::of(bytes).as_str(), &e.to_string())?;
                Err(e)
            }
        }
    }
    pub fn apply_commit(&mut self, commit: &CommitEnvelope) -> AppResult<ApplyOutcome> {
        self.apply_commit_with_faults(commit, &NoFaults)
    }
    pub fn apply_commit_with_faults(
        &mut self,
        commit: &CommitEnvelope,
        faults: &dyn FaultInjector,
    ) -> AppResult<ApplyOutcome> {
        let result = self.apply_validated(commit);
        if let Err(e) = &result {
            self.block_record(&commit.commit_id.to_string(), &e.to_string())?;
        }
        if result.is_ok() {
            self.drain_pending(128)?;
            faults.hit(FaultPoint::LocalAck)?;
        }
        result
    }
    pub fn drain_pending(&mut self, limit: usize) -> AppResult<usize> {
        if limit == 0 || limit > 128 {
            return Err(AppError::invalid("replay batch must be 1..128"));
        }
        let mut applied = 0;
        while applied < limit {
            let bytes:Option<Vec<u8>>=self.db.query_row("SELECT c.envelope FROM commits c WHERE c.applied=0 AND NOT EXISTS(SELECT 1 FROM blocked_records b WHERE b.id=c.id) AND NOT EXISTS(SELECT 1 FROM dependencies d LEFT JOIN commits dep ON dep.id=d.dependency WHERE d.commit_id=c.id AND (dep.id IS NULL OR dep.applied=0)) LIMIT 1",[],|r|r.get(0)).optional()?;
            let Some(bytes) = bytes else { break };
            let commit: CommitEnvelope = serde_json::from_slice(&bytes)?;
            if let Err(error) = self.apply_validated(&commit) {
                self.block_record(&commit.commit_id.to_string(), &error.to_string())?;
                return Err(error);
            }
            applied += 1;
        }
        Ok(applied)
    }
    fn apply_validated(&mut self, commit: &CommitEnvelope) -> AppResult<ApplyOutcome> {
        let encoded = commit.encode()?;
        let bound: Option<String> = self
            .db
            .query_row("SELECT id FROM archives LIMIT 1", [], |r| r.get(0))
            .optional()?;
        if bound.is_some_and(|s| s != commit.archive_id.to_string()) {
            return Err(AppError::integrity("catalog bound to another archive"));
        }
        let previous: Option<(Vec<u8>, bool)> = self
            .db
            .query_row(
                "SELECT envelope,applied FROM commits WHERE id=?",
                [commit.commit_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((bytes, applied)) = previous {
            if bytes != encoded {
                return Err(AppError::integrity(
                    "commit ID reused with different envelope",
                ));
            }
            if applied {
                return Ok(ApplyOutcome::AlreadyApplied);
            }
        }
        self.db.execute(
            "INSERT INTO archives(id,writer) VALUES(?,?) ON CONFLICT(id) DO NOTHING",
            params![commit.archive_id.to_string(), commit.writer_id.to_string()],
        )?;
        self.db.execute(
            "INSERT INTO commits(id,envelope) VALUES(?,?) ON CONFLICT(id) DO NOTHING",
            params![commit.commit_id.to_string(), encoded],
        )?;
        for dependency in &commit.dependencies {
            self.db.execute(
                "INSERT INTO dependencies(commit_id,dependency) VALUES(?,?) ON CONFLICT DO NOTHING",
                params![commit.commit_id.to_string(), dependency.to_string()],
            )?;
        }
        let missing:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM dependencies d LEFT JOIN commits c ON c.id=d.dependency WHERE d.commit_id=? AND (c.id IS NULL OR c.applied=0))",[commit.commit_id.to_string()],|r|r.get(0))?;
        if missing {
            return Ok(ApplyOutcome::Deferred);
        }
        let records = commit.records()?;
        let tx = self.db.transaction()?;
        for record in records {
            apply_record(
                &tx,
                &commit.archive_id.to_string(),
                &commit.commit_id.to_string(),
                record,
            )?;
        }
        tx.execute(
            "UPDATE commits SET applied=1,reason=NULL WHERE id=?",
            [commit.commit_id.to_string()],
        )?;
        tx.commit()?;
        Ok(ApplyOutcome::Applied)
    }
    pub fn publication_readiness(&self) -> AppResult<PublicationReadiness> {
        self.shared_readiness(true)
    }
    pub fn journal_readiness(&self) -> AppResult<PublicationReadiness> {
        self.shared_readiness(false)
    }
    fn shared_readiness(&self, include_rebuild_block: bool) -> AppResult<PublicationReadiness> {
        let mut reasons = Vec::new();
        let mut q=self.db.prepare("SELECT reason FROM blocked_records WHERE ? OR id<>'rebuild-incomplete' UNION ALL SELECT 'missing dependency or unapplied record: '||id FROM commits WHERE applied=0")?;
        for r in q.query_map([include_rebuild_block], |r| r.get::<_, String>(0))? {
            reasons.push(r?);
        }
        Ok(if reasons.is_empty() {
            PublicationReadiness::Ready
        } else {
            PublicationReadiness::Blocked(reasons)
        })
    }
    pub fn version(&self, id: VersionId) -> AppResult<Version> {
        let (sidecar, body, origin): (String, String, String) = self.db.query_row(
            "SELECT sidecar,body,origin FROM versions WHERE id=?",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut q = self
            .db
            .prepare("SELECT parent FROM version_parents WHERE version=? ORDER BY parent")?;
        let parents = q
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .map(|s| decode(format!("\"{}\"", s?)))
            .collect::<AppResult<Vec<_>>>()?;
        Ok(Version {
            id,
            sidecar: decode(format!("\"{sidecar}\""))?,
            parents,
            body: decode(body)?,
            origin: decode(origin)?,
        })
    }
    pub fn heads(&self, sidecar: SidecarId) -> AppResult<Vec<VersionId>> {
        heads(&self.db, sidecar)
    }
}
pub(crate) fn heads(db: &Connection, sidecar: SidecarId) -> AppResult<Vec<VersionId>> {
    let mut q = db.prepare("SELECT version FROM heads WHERE sidecar=? ORDER BY version")?;
    q.query_map([sidecar.to_string()], |r| r.get::<_, String>(0))?
        .map(|s| decode(format!("\"{}\"", s?)))
        .collect()
}
fn apply_record(
    db: &Connection,
    archive: &str,
    commit_id: &str,
    record: ArchiveRecord,
) -> AppResult<()> {
    match record {
        ArchiveRecord::CaptureRecorded { capture } => {
            crate::import::record_capture_evidence(db, &capture)?
        }
        ArchiveRecord::SequenceRecorded { sequence } => {
            crate::import::record_sequence_evidence(db, &sequence)?
        }
        ArchiveRecord::AmbiguityRecorded {
            id,
            kind,
            group,
            details,
        } => {
            let group = group
                .map(|g| super::identity::canonical(db, g))
                .transpose()?;
            let previous: Option<(String, Option<String>, String)> = db
                .query_row(
                    "SELECT kind,group_id,details FROM conflicts WHERE id=?",
                    [id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let expected = (
                kind.clone(),
                group.map(|g| g.to_string()),
                encode(&details)?,
            );
            if previous.as_ref().is_some_and(|p| p != &expected) {
                return Err(AppError::integrity(
                    "ambiguity ID reused with different evidence",
                ));
            }
            db.execute("INSERT INTO conflicts(id,kind,group_id,details) VALUES(?,?,?,?) ON CONFLICT DO NOTHING",params![id.to_string(),kind,expected.1,expected.2])?;
            db.execute(
                "INSERT INTO published_conflicts(id) VALUES(?) ON CONFLICT DO NOTHING",
                [id.to_string()],
            )?;
            if let Some(group) = group {
                db.execute(
                    "UPDATE residency SET conflict=1,protected=1 WHERE group_id=?",
                    [group.to_string()],
                )?;
            }
        }
        ArchiveRecord::AmbiguityResolved { request } => {
            super::conflicts::apply_resolution(db, request)?
        }
        ArchiveRecord::MetadataCached { result } => {
            let result = result.bounded()?;
            if result.extractor != crate::import::EXTRACTOR_VERSION {
                return Err(AppError::integrity(
                    "unsupported normalized metadata contract",
                ));
            }
            db.execute("INSERT INTO metadata_cache(digest,extractor,metadata) VALUES(?,?,?) ON CONFLICT(digest,extractor) DO NOTHING",params![result.digest.as_str(),result.extractor,encode(&result)?])?;
        }
        ArchiveRecord::ProjectionPlanned { plan } => {
            db.execute("INSERT INTO projection_commits(sidecar,version,commit_id) VALUES(?,?,?) ON CONFLICT DO NOTHING", params![plan.sidecar.to_string(),plan.version.to_string(),commit_id])?;
            db.execute("INSERT INTO nas_projections(sidecar,version,applied) VALUES(?,?,0) ON CONFLICT(sidecar) DO UPDATE SET version=excluded.version,applied=0",params![plan.sidecar.to_string(),plan.version.to_string()])?;
            let sidecar = plan.sidecar.to_string();
            let (location, owner): (String, String) = db.query_row(
                "SELECT path,owner FROM sidecars WHERE id=?",
                [&sidecar],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let actual: String = db.query_row(
                "SELECT body FROM versions WHERE id=? AND sidecar=?",
                params![plan.version.to_string(), sidecar],
                |r| r.get(0),
            )?;
            if location != plan.path.as_str()
                || owner != plan.owner
                || decode::<VersionBody>(actual)? != plan.body
                || heads(db, plan.sidecar)? != vec![plan.version]
            {
                return Err(AppError::integrity(
                    "projection does not match unambiguous sidecar head",
                ));
            }
            db.execute("INSERT INTO projections(sidecar,committed,pending) VALUES(?,?,1) ON CONFLICT(sidecar) DO UPDATE SET committed=excluded.committed,pending=1",params![sidecar,encode(&plan)?])?;
        }
        ArchiveRecord::GroupCreated { id } => {
            db.execute("INSERT INTO groups(id,published) VALUES(?,1) ON CONFLICT(id) DO UPDATE SET published=1",[id.to_string()])?;
            db.execute(
                "INSERT INTO residency(group_id) VALUES(?) ON CONFLICT DO NOTHING",
                [id.to_string()],
            )?;
        }
        ArchiveRecord::GroupsMerged { groups, evidence } => {
            super::identity::merge(db, &groups, &evidence)?;
        }
        ArchiveRecord::MemberAttached {
            group,
            member,
            evidence,
        } => {
            evidence.validate()?;
            member.validate()?;
            let group = super::identity::canonical(db, group)?;
            let old: Option<(String, String)> = db
                .query_row(
                    "SELECT group_id,digest FROM members WHERE archive=? AND path=?",
                    params![archive, member.path.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((id, digest)) = old {
                if digest != member.digest.as_str() {
                    return Err(AppError::integrity("conflicting placement in journal"));
                }
                let previous = decode(format!("\"{id}\""))?;
                if super::identity::canonical(db, previous)? != group {
                    super::identity::merge(db, &[previous, group], &evidence)?;
                }
            } else {
                db.execute(
                    "INSERT INTO blobs(digest,size) VALUES(?,?) ON CONFLICT DO NOTHING",
                    params![member.digest.as_str(), member.size],
                )?;
                let size: u64 = db.query_row(
                    "SELECT size FROM blobs WHERE digest=?",
                    [member.digest.as_str()],
                    |r| r.get(0),
                )?;
                if size != member.size {
                    return Err(AppError::integrity("blob size mismatch"));
                }
                db.execute(
                    "INSERT INTO members(archive,path,group_id,digest,role) VALUES(?,?,?,?,?)",
                    params![
                        archive,
                        member.path.as_str(),
                        group.to_string(),
                        member.digest.as_str(),
                        encode(&member.role)?
                    ],
                )?;
                db.execute(
                    "UPDATE groups SET revision=revision+1 WHERE id=?",
                    [group.to_string()],
                )?;
                db.execute(
                    "UPDATE residency SET ready_revision=NULL,metadata_current=0 WHERE group_id=?",
                    [group.to_string()],
                )?;
            }
            let canonical = super::identity::canonical(db, group)?;
            db.execute("INSERT INTO placements(id,group_id,path,digest,archived) VALUES(?,?,?,?,1) ON CONFLICT(path,group_id) DO UPDATE SET archived=1",params![OperationId::new().to_string(),canonical.to_string(),member.path.as_str(),member.digest.as_str()])?;
        }
        ArchiveRecord::SidecarCreated {
            id,
            group,
            path,
            owner,
        } => {
            validate_sidecar_path(&path)?;
            let group = group
                .map(|g| super::identity::canonical(db, g))
                .transpose()?;
            let old: Option<(Option<String>, String, String)> = db
                .query_row(
                    "SELECT group_id,path,owner FROM sidecars WHERE id=?",
                    [id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let expected = (
                group.map(|g| g.to_string()),
                path.as_str().to_owned(),
                owner.clone(),
            );
            if old.as_ref().is_some_and(|o| o != &expected) {
                return Err(AppError::integrity("sidecar identity reused"));
            }
            db.execute("INSERT INTO sidecars(id,group_id,path,owner) VALUES(?,?,?,?) ON CONFLICT DO NOTHING",params![id.to_string(),expected.0,path.as_str(),owner])?;
            db.execute(
                "INSERT INTO published_sidecars(id) VALUES(?) ON CONFLICT DO NOTHING",
                [id.to_string()],
            )?;
        }
        ArchiveRecord::VersionAdded { version, operation } => {
            let old: Option<(String, String, String, String)> = db
                .query_row(
                    "SELECT sidecar,body,origin,operation FROM versions WHERE id=?",
                    [version.id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            if let Some((sidecar, body, origin, old_operation)) = old {
                let mut q = db.prepare(
                    "SELECT parent FROM version_parents WHERE version=? ORDER BY parent",
                )?;
                let recorded = q
                    .query_map([version.id.to_string()], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                let mut expected = version
                    .parents
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                expected.sort();
                if sidecar != version.sidecar.to_string()
                    || decode::<VersionBody>(body)? != version.body
                    || decode::<VersionOrigin>(origin)? != version.origin
                    || old_operation != operation.to_string()
                    || recorded != expected
                {
                    return Err(AppError::integrity(
                        "version ID reused with different identity, bytes or ancestry",
                    ));
                }
                db.execute(
                    "INSERT INTO archived_versions(version) VALUES(?) ON CONFLICT DO NOTHING",
                    [version.id.to_string()],
                )?;
                return Ok(());
            }
            let mut seen = std::collections::BTreeSet::new();
            for parent in &version.parents {
                if !seen.insert(parent) || *parent == version.id {
                    return Err(AppError::integrity("duplicate or cyclic version parent"));
                }
                let owner: Option<String> = db
                    .query_row(
                        "SELECT sidecar FROM versions WHERE id=?",
                        [parent.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if owner != Some(version.sidecar.to_string()) {
                    return Err(AppError::integrity("missing or cross-sidecar parent"));
                }
            }
            db.execute(
                "INSERT INTO versions(id,sidecar,body,origin,operation) VALUES(?,?,?,?,?)",
                params![
                    version.id.to_string(),
                    version.sidecar.to_string(),
                    encode(&version.body)?,
                    encode(&version.origin)?,
                    operation.to_string()
                ],
            )?;
            db.execute(
                "INSERT INTO archived_versions(version) VALUES(?) ON CONFLICT DO NOTHING",
                [version.id.to_string()],
            )?;
            for parent in version.parents {
                db.execute(
                    "INSERT INTO version_parents(version,parent) VALUES(?,?)",
                    params![version.id.to_string(), parent.to_string()],
                )?;
            }
        }
        ArchiveRecord::HeadsChanged {
            sidecar,
            mut expected,
            heads: mut next,
        } => {
            expected.sort();
            next.sort();
            if expected != heads(db, sidecar)? {
                return Err(AppError::integrity("head precondition mismatch"));
            }
            if next.windows(2).any(|w| w[0] == w[1]) {
                return Err(AppError::integrity("duplicate head"));
            }
            for id in &next {
                let owner: String = db.query_row(
                    "SELECT sidecar FROM versions WHERE id=?",
                    [id.to_string()],
                    |r| r.get(0),
                )?;
                if owner != sidecar.to_string() {
                    return Err(AppError::integrity("head owner mismatch"));
                }
            }
            db.execute("DELETE FROM heads WHERE sidecar=?", [sidecar.to_string()])?;
            for id in next {
                db.execute(
                    "INSERT INTO heads(sidecar,version) VALUES(?,?)",
                    params![sidecar.to_string(), id.to_string()],
                )?;
            }
            db.execute("INSERT INTO projections(sidecar,pending) VALUES(?,1) ON CONFLICT(sidecar) DO UPDATE SET pending=1",[sidecar.to_string()])?;
            db.execute("UPDATE residency SET metadata_current=0,protected=1 WHERE group_id IN(SELECT group_id FROM sidecars WHERE id=?)", [sidecar.to_string()])?;
        }
        ArchiveRecord::ConflictRecorded {
            id,
            group,
            kind,
            heads,
        } => {
            db.execute(
                "INSERT INTO conflicts(id,kind,group_id,details) VALUES(?,?,?,?)",
                params![
                    id.to_string(),
                    kind,
                    group.map(|g| g.to_string()),
                    encode(&heads)?
                ],
            )?;
            db.execute(
                "INSERT INTO published_conflicts(id) VALUES(?) ON CONFLICT DO NOTHING",
                [id.to_string()],
            )?;
            if let Some(group) = group {
                db.execute(
                    "UPDATE residency SET conflict=1 WHERE group_id=?",
                    [super::identity::canonical(db, group)?.to_string()],
                )?;
            }
        }
        ArchiveRecord::ConflictResolved {
            id,
            expected_revision,
            version,
        } => {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM versions WHERE id=?)",
                [version.to_string()],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(AppError::integrity("resolution references unknown version"));
            }
            if db.execute("UPDATE conflicts SET resolved=1,revision=revision+1 WHERE id=? AND revision=? AND resolved=0",params![id.to_string(),expected_revision])?!=1{return Err(AppError::integrity("resolution precondition mismatch"));}
            db.execute("UPDATE residency SET conflict=EXISTS(SELECT 1 FROM conflicts WHERE group_id=residency.group_id AND resolved=0) WHERE group_id IN(SELECT group_id FROM conflicts WHERE id=?)",[id.to_string()])?;
        }
        ArchiveRecord::MappingRecorded {
            remote_checksum,
            external_path,
            server,
            asset,
            library,
            path,
            digest,
            group,
        } => {
            let group = super::identity::canonical(db, group)?;
            let matches:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM members WHERE archive=? AND path=? AND digest=? AND group_id=? AND role=?)",params![archive,path.as_str(),digest.as_str(),group.to_string(),encode(&MediaRole::Jpeg)?],|r|r.get(0))?;
            if !matches {
                return Err(AppError::integrity("mapping identity mismatch"));
            }
            let old:Option<SharedAssetBinding>=db.query_row("SELECT library,path,digest,group_id,remote_checksum,external_path FROM mappings WHERE server=? AND asset=?",params![server,asset.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
            let expected = (
                library.map(|l| l.to_string()),
                path.as_str().to_owned(),
                Some(digest.as_str().to_owned()),
                group.to_string(),
                remote_checksum.clone(),
                external_path.clone(),
            );
            if old.is_some_and(|old| old != expected) {
                return Err(AppError::integrity(
                    "shared asset binding differs from retained identity",
                ));
            }
            db.execute("INSERT INTO mappings(server,asset,library,path,digest,group_id,remote_checksum,external_path) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT DO NOTHING",params![server,asset.to_string(),expected.0,path.as_str(),digest.as_str(),group.to_string(),remote_checksum,external_path])?;
        }
        ArchiveRecord::RatingObserved {
            operation,
            group,
            local,
            remote,
        } => {
            let group = super::identity::canonical(db, group)?;
            db.execute(
                "INSERT INTO rating_observations(id,group_id,local,remote) VALUES(?,?,?,?)",
                params![
                    operation.to_string(),
                    group.to_string(),
                    encode(&local)?,
                    encode(&remote)?
                ],
            )?;
        }
    }
    Ok(())
}
