use crate::{
    catalog::*,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
};
use rusqlite::{OptionalExtension, params};
#[derive(Debug, Clone)]
pub enum SnapshotPayload {
    Stored(super::StoredSnapshot),
    Content(Vec<u8>),
    File {
        file: std::sync::Arc<std::fs::File>,
        digest: Digest,
        size: u64,
    },
    Reset,
}
#[derive(Debug, Clone)]
pub struct SnapshotIntent {
    pub sidecar: SidecarId,
    pub parents: Vec<VersionId>,
    pub operation: OperationId,
    pub payload: SnapshotPayload,
    pub origin: VersionOrigin,
}
pub struct VersionStore<'a> {
    catalog: &'a mut Catalog,
    root: &'a RootHandle,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionDecision {
    Unchanged,
    PublishLocal(Vec<VersionId>),
    AdoptRemote(Vec<VersionId>),
    Conflict(Vec<VersionId>),
}
pub fn reconcile_heads(
    base: &[VersionId],
    local: &[VersionId],
    remote: &[VersionId],
) -> VersionDecision {
    let ordered = |ids: &[VersionId]| {
        ids.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
    };
    let b = ordered(base);
    let l = ordered(local);
    let r = ordered(remote);
    if l == r {
        VersionDecision::Unchanged
    } else if r == b {
        VersionDecision::PublishLocal(l.into_iter().collect())
    } else if l == b {
        VersionDecision::AdoptRemote(r.into_iter().collect())
    } else {
        VersionDecision::Conflict(l.union(&r).copied().collect())
    }
}
impl<'a> VersionStore<'a> {
    pub fn new(catalog: &'a mut Catalog, root: &'a RootHandle) -> Self {
        Self { catalog, root }
    }
    pub fn record(&mut self, intent: SnapshotIntent) -> AppResult<VersionId> {
        self.record_with_head(intent, true, &NoFaults)
    }
    pub(crate) fn record_detached(&mut self, intent: SnapshotIntent) -> AppResult<VersionId> {
        self.record_with_head(intent, false, &NoFaults)
    }
    pub fn record_with_faults(
        &mut self,
        intent: SnapshotIntent,
        faults: &dyn FaultInjector,
    ) -> AppResult<VersionId> {
        self.record_with_head(intent, true, faults)
    }
    fn record_with_head(
        &mut self,
        intent: SnapshotIntent,
        advance_head: bool,
        faults: &dyn FaultInjector,
    ) -> AppResult<VersionId> {
        self.catalog
            .db
            .execute_batch(crate::catalog::schema::EDIT_SCHEMA)?;
        let body = match &intent.payload {
            SnapshotPayload::Stored(stored) => stored.body(),
            SnapshotPayload::Content(bytes) => VersionBody::Content {
                digest: Digest::of(bytes),
                size: bytes.len() as u64,
            },
            SnapshotPayload::File { digest, size, .. } => VersionBody::Content {
                digest: digest.clone(),
                size: *size,
            },
            SnapshotPayload::Reset => VersionBody::Reset,
        };
        let mut parents = intent.parents.clone();
        parents.sort();
        if parents.windows(2).any(|p| p[0] == p[1]) {
            return Err(AppError::invalid("duplicate version parent"));
        }
        for parent in &parents {
            if self.catalog.version(*parent)?.sidecar != intent.sidecar {
                return Err(AppError::integrity("cross-sidecar parent"));
            }
        }
        let old:Option<(String,String,String,String,String)>=self.catalog.db.query_row("SELECT version,sidecar,parents,body,origin FROM snapshot_intents WHERE operation=?",[intent.operation.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        let version = if let Some((
            version,
            sidecar,
            previous_parents,
            previous_body,
            previous_origin,
        )) = old
        {
            if sidecar != intent.sidecar.to_string()
                || previous_parents != serde_json::to_string(&parents)?
                || previous_body != serde_json::to_string(&body)?
                || previous_origin != serde_json::to_string(&intent.origin)?
            {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "snapshot operation reused with different content or ancestry",
                ));
            }
            serde_json::from_str(&format!("\"{version}\""))?
        } else {
            let version = if intent.origin == VersionOrigin::Derived {
                VersionId(intent.operation.0)
            } else {
                VersionId::new()
            };
            self.catalog.db.execute("INSERT INTO snapshot_intents(operation,version,sidecar,parents,body,origin) VALUES(?,?,?,?,?,?)",params![intent.operation.to_string(),version.to_string(),intent.sidecar.to_string(),serde_json::to_string(&parents)?,serde_json::to_string(&body)?,serde_json::to_string(&intent.origin)?])?;
            version
        };
        let recorded: bool = self.catalog.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM versions WHERE id=?)",
            [version.to_string()],
            |r| r.get(0),
        )?;
        if recorded {
            return Ok(version);
        }
        if let SnapshotPayload::Stored(stored) = &intent.payload {
            stored.register(self.root, self.catalog)?;
        }
        if let SnapshotPayload::Content(bytes) = &intent.payload {
            let digest = Digest::of(bytes);
            self.root
                .mkdir_all(&ArchivePath::parse(".photo-sorter/snapshots")?)?;
            let target = ArchivePath::parse(&format!(
                ".photo-sorter/snapshots/{}",
                &digest.as_str()[7..]
            ))?;
            match self.root.write_new(&target, bytes) {
                Ok(()) => {}
                Err(e) => {
                    if e.code != ErrorCode::Io
                        || self.root.read(&target, bytes.len() as u64)? != *bytes
                    {
                        return Err(e);
                    }
                }
            }
            self.catalog.db.execute(
                "INSERT INTO snapshots(digest,path,size) VALUES(?,?,?) ON CONFLICT DO NOTHING",
                params![digest.as_str(), target.as_str(), bytes.len() as u64],
            )?;
        }
        if let SnapshotPayload::File { file, digest, size } = &intent.payload {
            self.root
                .mkdir_all(&ArchivePath::parse(".photo-sorter/snapshots")?)?;
            let target = ArchivePath::parse(&format!(
                ".photo-sorter/snapshots/{}",
                &digest.as_str()[7..]
            ))?;
            let stage = ArchivePath::parse(&format!(
                ".photo-sorter/snapshots/stage-{}",
                intent.operation
            ))?;
            let mut dest = self.root.create_new(&stage)?;
            let mut offset = 0;
            let mut buffer = [0u8; 128 * 1024];
            let mut hash = blake3::Hasher::new();
            use std::{io::Write, os::unix::fs::FileExt};
            loop {
                let n = file.read_at(&mut buffer, offset)?;
                if n == 0 {
                    break;
                }
                dest.write_all(&buffer[..n])?;
                hash.update(&buffer[..n]);
                offset += n as u64;
            }
            dest.sync_all()?;
            if offset != *size
                || Digest::parse(&format!("blake3:{}", hash.finalize().to_hex()))? != *digest
            {
                return Err(AppError::integrity(
                    "snapshot source changed during streaming copy; staged bytes retained",
                ));
            }
            if publish_noreplace(self.root, &stage, &target)? == PublishOutcome::Exists {
                let mut existing = self.root.open_relative(&target, libc::O_RDONLY, 0)?;
                if crate::transfer::digest_file(&mut existing)? != (digest.clone(), *size) {
                    return Err(AppError::integrity("immutable snapshot is corrupt"));
                }
            }
            self.catalog.db.execute(
                "INSERT INTO snapshots(digest,path,size) VALUES(?,?,?) ON CONFLICT DO NOTHING",
                params![digest.as_str(), target.as_str(), size],
            )?;
        }
        faults.hit(FaultPoint::IntentSaved)?;
        let tx = self.catalog.db.transaction()?;
        tx.execute("INSERT INTO versions(id,sidecar,body,origin,operation) VALUES(?,?,?,?,?) ON CONFLICT(id) DO NOTHING",params![version.to_string(),intent.sidecar.to_string(),serde_json::to_string(&body)?,serde_json::to_string(&intent.origin)?,intent.operation.to_string()])?;
        for parent in parents {
            tx.execute(
                "INSERT INTO version_parents(version,parent) VALUES(?,?) ON CONFLICT DO NOTHING",
                params![version.to_string(), parent.to_string()],
            )?;
        }
        if intent.origin == VersionOrigin::Observed && advance_head {
            let previous: Option<String> = tx
                .query_row(
                    "SELECT version FROM local_heads WHERE sidecar=?",
                    [intent.sidecar.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            if previous.as_ref() != Some(&version.to_string()) {
                tx.execute("INSERT INTO local_heads(sidecar,version) VALUES(?,?) ON CONFLICT(sidecar) DO UPDATE SET version=excluded.version,dirty=1",params![intent.sidecar.to_string(),version.to_string()])?;
            }
        }
        tx.execute(
            "UPDATE sidecars SET protected=1 WHERE id=?",
            [intent.sidecar.to_string()],
        )?;
        tx.commit()?;
        Ok(version)
    }
}
impl Catalog {
    pub fn register_sidecar(
        &mut self,
        id: SidecarId,
        group: Option<GroupId>,
        path: ArchivePath,
        owner: &str,
    ) -> AppResult<()> {
        validate_sidecar_path(&path)?;
        let group = group.map(|g| self.canonical_group(g)).transpose()?;
        let old: Option<(Option<String>, String, String)> = self
            .db
            .query_row(
                "SELECT group_id,path,owner FROM sidecars WHERE id=?",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let expected = (
            group.map(|g| g.to_string()),
            path.as_str().to_owned(),
            owner.to_owned(),
        );
        if old.as_ref().is_some_and(|p| p != &expected) {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "sidecar identity already bound",
            ));
        }
        self.db.execute(
            "INSERT INTO sidecars(id,group_id,path,owner) VALUES(?,?,?,?) ON CONFLICT DO NOTHING",
            params![id.to_string(), expected.0, path.as_str(), owner],
        )?;
        Ok(())
    }
    pub fn acknowledge_snapshot(
        &mut self,
        sidecar: SidecarId,
        version: VersionId,
    ) -> AppResult<bool> {
        Ok(self.db.execute(
            "UPDATE local_heads SET dirty=0 WHERE sidecar=? AND version=?",
            params![sidecar.to_string(), version.to_string()],
        )? == 1)
    }
    pub fn snapshot_dirty(&self, sidecar: SidecarId) -> AppResult<bool> {
        Ok(self.db.query_row(
            "SELECT dirty FROM local_heads WHERE sidecar=?",
            [sidecar.to_string()],
            |r| r.get(0),
        )?)
    }
    pub fn resolve_conflict(
        &mut self,
        id: ConflictId,
        expected: &[VersionId],
        chosen: VersionId,
    ) -> AppResult<VersionId> {
        let version = self.version(chosen)?;
        let mut heads = self.heads(version.sidecar)?;
        let mut expected = expected.to_vec();
        heads.sort();
        expected.sort();
        if heads != expected || !heads.contains(&chosen) {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "resolution must name all current heads and choose one",
            ));
        }
        let (revision, resolved): (u64, bool) = self.db.query_row(
            "SELECT revision,resolved FROM conflicts WHERE id=?",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if resolved {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "conflict already resolved",
            ));
        }
        let _ = revision;
        let details: String = self.db.query_row(
            "SELECT details FROM conflicts WHERE id=?",
            [id.to_string()],
            |r| r.get(0),
        )?;
        let mut conflict_heads: Vec<VersionId> = serde_json::from_str(&details)?;
        conflict_heads.sort();
        if conflict_heads != expected {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "conflict belongs to different version heads",
            ));
        }
        let new = VersionId::new();
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT INTO versions(id,sidecar,body,origin,operation) VALUES(?,?,?,?,?)",
            params![
                new.to_string(),
                version.sidecar.to_string(),
                serde_json::to_string(&version.body)?,
                serde_json::to_string(&VersionOrigin::Derived)?,
                OperationId::new().to_string()
            ],
        )?;
        for parent in &heads {
            tx.execute(
                "INSERT INTO version_parents(version,parent) VALUES(?,?)",
                params![new.to_string(), parent.to_string()],
            )?;
        }
        tx.execute(
            "INSERT INTO pending_resolutions(conflict,version,expected_heads) VALUES(?,?,?)",
            params![
                id.to_string(),
                new.to_string(),
                serde_json::to_string(&heads)?
            ],
        )?;
        tx.commit()?;
        Ok(new)
    }
}
impl Catalog {
    pub fn local_head(&self, sidecar: SidecarId) -> AppResult<Option<VersionId>> {
        let id: Option<String> = self
            .db
            .query_row(
                "SELECT version FROM local_heads WHERE sidecar=?",
                [sidecar.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        id.map(|id| serde_json::from_str(&format!("\"{id}\"")).map_err(Into::into))
            .transpose()
    }
}

impl Catalog {
    pub fn managed_sidecar_id(&self, path: &ArchivePath) -> AppResult<SidecarId> {
        validate_sidecar_path(path)?;
        let mut query = self
            .db
            .prepare("SELECT id FROM sidecars WHERE path=? ORDER BY id LIMIT 2")?;
        let ids = query
            .query_map([path.as_str()], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if ids.len() > 1 {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "multiple sidecar identities claim this path; explicit association is required",
            ));
        }
        if let Some(id) = ids.first() {
            return Ok(serde_json::from_str(&format!("\"{id}\""))?);
        }
        let archive: Option<String> = self
            .db
            .query_row("SELECT id FROM archives LIMIT 1", [], |r| r.get(0))
            .optional()?;
        let Some(archive) = archive else {
            return Ok(SidecarId::new());
        };
        let digest = blake3::hash(format!("sidecar-v1:{archive}:{}", path.as_str()).as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        Ok(SidecarId(uuid::Uuid::from_bytes(bytes)))
    }
}
