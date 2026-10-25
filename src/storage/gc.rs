use crate::{
    catalog::Catalog,
    domain::*,
    error::{AppError, AppResult},
    fs::RootHandle,
};
#[derive(Default, Debug)]
pub struct GcReport {
    pub removed: u64,
    pub bytes: u64,
}
pub struct SnapshotGc<'a> {
    pub state: &'a RootHandle,
}
impl SnapshotGc<'_> {
    pub fn collect(&self, catalog: &mut Catalog) -> AppResult<GcReport> {
        self.state.require_local()?;
        catalog.db.execute("UPDATE snapshots AS s SET refs=(SELECT count(*) FROM local_heads l JOIN versions v ON v.id=l.version WHERE json_extract(v.body,'$.Content.digest')=s.digest)+(SELECT count(*) FROM heads h JOIN versions v ON v.id=h.version WHERE json_extract(v.body,'$.Content.digest')=s.digest)+(SELECT count(*) FROM projections p LEFT JOIN versions v ON v.id=p.applied WHERE json_extract(p.committed,'$.body.Content.digest')=s.digest OR (CASE WHEN json_valid(p.applied) THEN json_extract(p.applied,'$.body.Content.digest') END)=s.digest OR json_extract(v.body,'$.Content.digest')=s.digest)+(SELECT count(*) FROM observations o WHERE o.digest=s.digest)",[])?;
        let mut result = GcReport::default();
        let mut q=catalog.db.prepare("SELECT digest,path,size FROM snapshots s WHERE archived=1 AND refs=0 AND NOT EXISTS(SELECT 1 FROM jobs WHERE json_extract(state,'$') NOT IN('Complete','Cancelled')) AND NOT EXISTS(SELECT 1 FROM conflicts WHERE resolved=0) AND NOT EXISTS(SELECT 1 FROM projections WHERE pending=1) AND NOT EXISTS(SELECT 1 FROM capture_requests WHERE done=0) AND NOT EXISTS(SELECT 1 FROM local_heads l JOIN versions v ON v.id=l.version WHERE json_extract(v.body,'$.Content.digest')=s.digest) LIMIT 128")?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);
        for (digest, path, size) in rows {
            let path = ArchivePath::parse(&path)?;
            if !path.as_str().starts_with(".photo-sorter/snapshots/") {
                return Err(AppError::integrity("invalid local snapshot ownership"));
            }
            let mut file = self.state.open_relative(&path, libc::O_RDONLY, 0)?;
            if crate::transfer::digest_file(&mut file)? != (Digest::parse(&digest)?, size) {
                return Err(AppError::integrity("unknown snapshot bytes retained"));
            }
            catalog.db.execute(
                "UPDATE snapshots SET cleanup_intent=1 WHERE digest=?",
                [&digest],
            )?;
            self.state.unlink(&path, false)?;
            catalog
                .db
                .execute("DELETE FROM snapshots WHERE digest=?", [&digest])?;
            result.removed += 1;
            result.bytes += size;
        }
        Ok(result)
    }
}
#[cfg(test)]
mod reference_tests {
    use super::*;
    use crate::{edit::*, fs::*};
    #[test]
    fn archived_ancestor_is_collectable_but_current_snapshot_remains_referenced() {
        let temporary = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temporary.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let sidecar = SidecarId::new();
        catalog
            .register_sidecar(
                sidecar,
                None,
                ArchivePath::parse("a.rrdata").unwrap(),
                "rapidraw-base",
            )
            .unwrap();
        let first = VersionStore::new(&mut catalog, &root)
            .record(SnapshotIntent {
                sidecar,
                parents: vec![],
                operation: OperationId::new(),
                payload: SnapshotPayload::Content(b"old".to_vec()),
                origin: VersionOrigin::Observed,
            })
            .unwrap();
        VersionStore::new(&mut catalog, &root)
            .record(SnapshotIntent {
                sidecar,
                parents: vec![first],
                operation: OperationId::new(),
                payload: SnapshotPayload::Content(b"current".to_vec()),
                origin: VersionOrigin::Observed,
            })
            .unwrap();
        catalog
            .db
            .execute("UPDATE snapshots SET archived=1", [])
            .unwrap();
        let result = SnapshotGc { state: &root }.collect(&mut catalog).unwrap();
        assert_eq!(result.removed, 1);
        assert!(
            !temporary
                .path()
                .join(snapshot_path(&Digest::of(b"old")).unwrap().as_str())
                .exists()
        );
        assert!(
            temporary
                .path()
                .join(snapshot_path(&Digest::of(b"current")).unwrap().as_str())
                .exists()
        );
        let refs: u64 = catalog
            .db
            .query_row(
                "SELECT refs FROM snapshots WHERE digest=?",
                [Digest::of(b"current").as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(refs > 0);
    }
}
