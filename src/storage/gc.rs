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
    pub fn refresh_references(catalog: &mut Catalog) -> AppResult<()> {
        catalog.db.execute(r#"UPDATE snapshots AS s SET refs=(SELECT count(*) FROM local_heads l JOIN versions v ON v.id=l.version WHERE json_extract(v.body,'$.Content.digest')=s.digest)+(SELECT count(*) FROM heads h JOIN versions v ON v.id=h.version WHERE json_extract(v.body,'$.Content.digest')=s.digest)+(SELECT count(*) FROM projections p LEFT JOIN versions v ON v.id=p.applied WHERE json_extract(p.committed,'$.body.Content.digest')=s.digest OR (CASE WHEN json_valid(p.applied) THEN json_extract(p.applied,'$.body.Content.digest') END)=s.digest OR json_extract(v.body,'$.Content.digest')=s.digest)+(SELECT count(*) FROM observations o WHERE o.digest=s.digest)+(SELECT count(*) FROM versions v WHERE json_extract(v.body,'$.Content.digest')=s.digest AND (NOT EXISTS(SELECT 1 FROM archived_versions a WHERE a.version=v.id) OR EXISTS(SELECT 1 FROM rating_baselines b WHERE b.source_version=v.id OR b.pending_version=v.id) OR EXISTS(SELECT 1 FROM favorite_baselines b WHERE b.source_version=v.id OR b.pending_version=v.id) OR EXISTS(SELECT 1 FROM favorite_intents f WHERE f.state IN('prepared','sent') AND json_extract(f.body,'$.source')=v.id) OR EXISTS(SELECT 1 FROM merged_favorite_evidence f WHERE json_extract(f.body,'$.source_version')=v.id OR json_extract(f.body,'$.pending_version')=v.id) OR EXISTS(SELECT 1 FROM jobs j WHERE j.state NOT IN('"Complete"','"Cancelled"') AND instr(j.request,v.id)>0)))+(SELECT count(*) FROM snapshot_intents i WHERE json_extract(i.body,'$.Content.digest')=s.digest AND NOT EXISTS(SELECT 1 FROM versions v WHERE v.id=i.version))"#,[])?;
        Ok(())
    }
    pub fn candidates(catalog: &mut Catalog) -> AppResult<Vec<(String, String, u64)>> {
        Self::refresh_references(catalog)?;
        let mut query=catalog.db.prepare("SELECT digest,path,size FROM snapshots WHERE archived=1 AND refs=0 AND NOT EXISTS(SELECT 1 FROM cleanup_receipts c WHERE c.kind='snapshot' AND c.path=snapshots.path AND c.digest=snapshots.digest AND c.state='retained') AND NOT EXISTS(SELECT 1 FROM capture_requests WHERE done=0) ORDER BY digest LIMIT 128")?;
        Ok(query
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?)
    }
    pub fn collect(&self, catalog: &mut Catalog) -> AppResult<GcReport> {
        self.state.require_local()?;
        let mut result = GcReport::default();
        let rows = Self::candidates(catalog)?;
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
    use crate::{
        catalog::{DurableRequest, JobKind},
        edit::*,
        fs::*,
    };
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
        catalog
            .db
            .execute("INSERT INTO archived_versions SELECT id FROM versions", [])
            .unwrap();
        catalog
            .enqueue(DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::Clean,
            })
            .unwrap();
        catalog
            .record_ambiguity("unrelated", None, serde_json::json!({"preserved":true}))
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
