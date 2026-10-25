use super::*;
use crate::{archive::*, domain::*};
use rusqlite::{OptionalExtension, params};
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
        let result = self.apply_validated(commit);
        if let Err(e) = &result {
            self.block_record(&commit.commit_id.to_string(), &e.to_string())?;
        }
        result
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
            apply_record(&tx, &commit.archive_id.to_string(), record)?;
        }
        tx.execute(
            "UPDATE commits SET applied=1,reason=NULL WHERE id=?",
            [commit.commit_id.to_string()],
        )?;
        tx.commit()?;
        loop {
            let mut query=self.db.prepare("SELECT c.envelope FROM commits c WHERE c.applied=0 AND NOT EXISTS(SELECT 1 FROM dependencies d LEFT JOIN commits dep ON dep.id=d.dependency WHERE d.commit_id=c.id AND (dep.id IS NULL OR dep.applied=0)) LIMIT 1")?;
            let bytes: Option<Vec<u8>> = query.query_row([], |r| r.get(0)).optional()?;
            drop(query);
            if let Some(bytes) = bytes {
                let next = CommitEnvelope::decode(&bytes, commit.archive_id)?;
                self.apply_validated(&next)?;
            } else {
                break;
            }
        }
        Ok(ApplyOutcome::Applied)
    }
    pub fn publication_readiness(&self) -> AppResult<PublicationReadiness> {
        let mut reasons = Vec::new();
        let mut q=self.db.prepare("SELECT reason FROM blocked_records UNION ALL SELECT 'missing dependency or unapplied record: '||id FROM commits WHERE applied=0")?;
        for r in q.query_map([], |r| r.get::<_, String>(0))? {
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
fn heads(db: &Connection, sidecar: SidecarId) -> AppResult<Vec<VersionId>> {
    let mut q = db.prepare("SELECT version FROM heads WHERE sidecar=? ORDER BY version")?;
    q.query_map([sidecar.to_string()], |r| r.get::<_, String>(0))?
        .map(|s| decode(format!("\"{}\"", s?)))
        .collect()
}
fn apply_record(db: &Connection, archive: &str, record: ArchiveRecord) -> AppResult<()> {
    match record {
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
        }
        ArchiveRecord::SidecarCreated {
            id,
            group,
            path,
            owner,
        } => {
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
        }
        ArchiveRecord::VersionAdded { version, operation } => {
            let old: Option<(String, String, String, String)> = db
                .query_row(
                    "SELECT sidecar,body,origin,operation FROM versions WHERE id=?",
                    [version.id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            if old.is_some() {
                return Err(AppError::integrity(
                    "version ID already exists in another record",
                ));
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
        }
        ArchiveRecord::MappingRecorded {
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
            db.execute("INSERT INTO mappings(server,asset,library,path,digest,group_id) VALUES(?,?,?,?,?,?)",params![server,asset.to_string(),library.map(|l|l.to_string()),path.as_str(),digest.as_str(),group.to_string()])?;
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
