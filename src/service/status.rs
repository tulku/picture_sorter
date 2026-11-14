use crate::{catalog::*, domain::*, error::AppResult, import::ImportCoordinator};
use serde::Serialize;
#[derive(Debug, Serialize)]
pub struct SpaceSnapshot {
    pub budget: u64,
    pub reserve: u64,
    pub filesystem_free: Option<u64>,
    pub state_filesystem_free: Option<u64>,
    pub occupancy_observed_at: Option<Timestamp>,
    pub occupancy_reason: Option<String>,
    pub snapshots: Option<u64>,
    pub source_companions: Option<u64>,
    pub retained_displaced: Option<u64>,
    pub catalog_overhead: Option<u64>,
    pub exclusive_total: Option<u64>,
    pub resident: u64,
    pub staging: Option<u64>,
    pub metadata: Option<u64>,
    pub pinned: u64,
    pub protected: u64,
    pub evictable: u64,
    pub remaining_reservations: u64,
    pub subset_note: String,
}
#[derive(Debug, Serialize)]
pub struct StatusSnapshot {
    pub activity: serde_json::Value,
    pub session_metrics: crate::metrics::Metrics,
    pub pagination: serde_json::Value,
    pub schema_version: u32,
    pub protocol_version: u32,
    pub profile_id: ProfileId,
    pub writer_id: WriterId,
    pub archive_id: Option<ArchiveId>,
    pub archive: serde_json::Value,
    pub capabilities: serde_json::Value,
    pub space: SpaceSnapshot,
    pub jobs: Vec<JobRecord>,
    pub groups: Vec<serde_json::Value>,
    pub conflicts: Vec<ConflictRecord>,
    pub last_successful_operations: Vec<JobId>,
    pub watcher: serde_json::Value,
    pub worker: serde_json::Value,
}
impl StatusSnapshot {
    pub(crate) fn collect_page(
        engine: &ImportCoordinator,
        occupancy: &super::space::SpaceMeter,
        worker: serde_json::Value,
        archive: serde_json::Value,
        query: &super::protocol::StatusQuery,
    ) -> AppResult<Self> {
        if query.limit == 0 || query.limit > 128 {
            return Err(crate::error::AppError::invalid(
                "status page must be 1..128",
            ));
        }
        let catalog = &engine.catalog;
        let jobs = catalog.jobs_page(query.jobs_after, query.limit, false)?;
        let conflicts = catalog.conflicts_page(query.conflicts_after, query.limit, false)?;
        let group_records = catalog.group_batch(query.groups_after, query.limit)?;
        let job_cursor = jobs.last().map(|j| j.id);
        let group_cursor = group_records.last().map(|g| g.id);
        let conflict_cursor = conflicts.last().map(|c| c.id);
        let jobs_more = if let Some(id) = job_cursor {
            !catalog.jobs_page(Some(id), 1, false)?.is_empty()
        } else {
            false
        };
        let groups_more = if let Some(id) = group_cursor {
            !catalog.group_batch(Some(id), 1)?.is_empty()
        } else {
            false
        };
        let conflicts_more = if let Some(id) = conflict_cursor {
            !catalog.conflicts_page(Some(id), 1, false)?.is_empty()
        } else {
            false
        };
        let pagination = serde_json::json!({"jobs":{"next":job_cursor,"has_more":jobs_more},"groups":{"next":group_cursor,"has_more":groups_more},"conflicts":{"next":conflict_cursor,"has_more":conflicts_more}});
        let resident:u64=catalog.db.query_row("SELECT coalesce(sum(b.size),0) FROM placements p JOIN blobs b ON b.digest=p.digest WHERE local_ready=1",[],|r|r.get(0))?;
        let subset = |condition: &str| -> AppResult<u64> {
            Ok(catalog.db.query_row(&format!("SELECT coalesce(sum(b.size),0) FROM placements p JOIN blobs b ON b.digest=p.digest JOIN residency r ON r.group_id=p.group_id WHERE p.local_ready=1 AND {condition}"),[],|r|r.get(0))?)
        };
        let measured = occupancy.measurement.as_ref();
        let exclusive_total = measured
            .map(|m| {
                m.local
                    .total
                    .checked_add(m.state.total)
                    .ok_or_else(|| crate::error::AppError::invalid("occupancy overflow"))
            })
            .transpose()?;
        let groups = group_records
            .into_iter()
            .map(|group| {
                let readiness = catalog.readiness(group.id)?;
                let protection = catalog.protection(group.id)?;
                let mut query=catalog.db.prepare("SELECT path,status,reason FROM physical_findings WHERE group_id=? ORDER BY path LIMIT 128")?;
                let findings=query.query_map([group.id.to_string()],|r|Ok(serde_json::json!({"path":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"reason":r.get::<_,String>(2)?})))?.collect::<Result<Vec<_>,_>>()?;
                Ok(serde_json::json!({"group":group,"readiness":readiness,"protection":protection,"physical_findings":findings}))
            })
            .collect::<AppResult<Vec<_>>>()?;
        let space=SpaceSnapshot {
            budget:engine.config.budget,reserve:engine.config.reserve,
            filesystem_free:measured.map(|m|m.filesystem_free),state_filesystem_free:measured.map(|m|m.state_filesystem_free),occupancy_observed_at:occupancy.observed_at,occupancy_reason:occupancy.reason.clone(),
            exclusive_total,resident,metadata:measured.map(|m|m.state.total),staging:measured.map(|m|m.local.staging.saturating_add(m.state.staging)),snapshots:measured.map(|m|m.state.snapshots),source_companions:measured.map(|m|m.local.companions),retained_displaced:measured.map(|m|m.local.retained),catalog_overhead:measured.map(|m|m.state.catalog),
            pinned:subset("r.pin=1")?,protected:subset("r.protected=1 OR r.conflict=1")?,evictable:subset("r.pin=0 AND r.protected=0 AND r.conflict=0")?,remaining_reservations:catalog.total_remaining_reservations()?,
            subset_note:"Filesystem observations are cached at occupancy_observed_at; null means unmeasured. Metadata includes state snapshots, staging and catalog overhead. Pinned/protected/evictable overlap resident bytes. Do not add overlapping subsets or cached/current observations; admission and deletion recheck fresh evidence.".into(),
        };

        let active_count: u64 = catalog.db.query_row("SELECT count(*) FROM jobs WHERE json_extract(state,'$') NOT IN ('Complete','Cancelled','Failed')", [], |r| r.get(0))?;
        let mut active_query = catalog.db.prepare("SELECT id FROM jobs WHERE json_extract(state,'$') NOT IN ('Complete','Cancelled','Failed') ORDER BY CASE WHEN json_extract(request,'$.kind.type')='import' THEN 0 ELSE 1 END, rowid DESC LIMIT 8")?;
        let active_ids = active_query
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut active = Vec::new();
        for id in active_ids {
            let id: JobId = serde_json::from_value(id.into())?;
            let job = catalog.job(id)?;
            let detail = if matches!(job.request.kind, JobKind::Import { .. })
                && catalog.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM import_stats WHERE job=?)",
                    [id.to_string()],
                    |r| r.get::<_, bool>(0),
                )? {
                serde_json::to_value(engine.report(id)?)?
            } else {
                serde_json::Value::Null
            };
            active.push(serde_json::json!({"job":job,"detail":detail}));
        }
        let pending_upload: u64 = catalog.db.query_row("SELECT count(*) FROM (SELECT DISTINCT i.digest,i.destination FROM import_items i JOIN jobs j ON j.id=i.parent WHERE i.archived=0 AND i.state NOT IN ('image_duplicate','conflict','jpeg_pending') AND json_extract(j.state,'$') NOT IN ('Complete','Cancelled','Failed','CancelRequested') EXCEPT SELECT digest,destination FROM import_items WHERE archived=1)", [], |r| r.get(0))?;
        let recent = catalog
            .successful_operations(3)?
            .into_iter()
            .map(|id| catalog.job(id))
            .collect::<AppResult<Vec<_>>>()?;
        let generated_total: u64 = catalog.db.query_row(
            "SELECT count(*) FROM sidecars WHERE owner=?",
            [GENERATED_OUTPUT_OWNER],
            |row| row.get(0),
        )?;
        let generated_pending: u64 = catalog.db.query_row(
            "SELECT count(*) FROM sidecars s WHERE s.owner=? AND (EXISTS(SELECT 1 FROM generated_observations o WHERE o.path=s.path AND o.present=1 AND (o.captured_stamp IS NULL OR o.captured_stamp<>o.stamp)) OR EXISTS(SELECT 1 FROM local_heads l WHERE l.sidecar=s.id AND NOT EXISTS(SELECT 1 FROM nas_projections n WHERE n.sidecar=l.sidecar AND n.version=l.version AND n.applied=1)) OR EXISTS(SELECT 1 FROM heads h WHERE h.sidecar=s.id AND NOT EXISTS(SELECT 1 FROM archived_versions a WHERE a.version=h.version)) OR EXISTS(SELECT 1 FROM nas_projections p WHERE p.sidecar=s.id AND p.applied=0))",
            [GENERATED_OUTPUT_OWNER], |row| row.get(0),
        )?;
        let activity = serde_json::json!({"active_count":active_count,"active":active,"recent":recent,"pending_upload":pending_upload,"generated":{"total":generated_total,"pending":generated_pending}});

        Ok(Self {
            activity,
            session_metrics: crate::metrics::snapshot(),
            pagination,
            schema_version: crate::catalog::schema::VERSION as u32,
            protocol_version: 1,
            profile_id: engine.config.profile_id,
            writer_id: engine.config.writer_id,
            archive_id: engine.config.archive_id,
            archive,
            capabilities: serde_json::json!({"nas_publication":{"allowed":false,"reason":"deployment qualification must match this mount and writer mode"},"nas_metadata_replacement":{"allowed":false,"reason":"metadata replacement requires qualified atomic exchange"},"immich_ratings":{"allowed":false,"reason":"outbound writes require matching runtime evidence and explicit best_effort"},"immich_favorites":{"allowed":false,"reason":"Favorite writes require separate runtime evidence, native Pick qualification and explicit best_effort"}}),
            space,
            last_successful_operations: catalog.successful_operations(query.limit)?,
            jobs,
            groups,
            conflicts,
            watcher: serde_json::json!({"recovery":"startup reconciliation pending"}),
            worker,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_status_counts_local_saved_heads_before_publication() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["local", "state", "archive"] {
            std::fs::create_dir(temp.path().join(name)).unwrap();
        }
        let config = crate::config::Config::new(
            temp.path().join("local"),
            temp.path().join("state"),
            temp.path().join("archive"),
        );
        let root =
            crate::fs::RootHandle::open(&config.state_root, crate::fs::RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let output = catalog
            .register_generated_output(&ArchivePath::parse("render.tif").unwrap())
            .unwrap();
        crate::edit::VersionStore::new(&mut catalog, &root)
            .record(crate::edit::SnapshotIntent {
                sidecar: output.id,
                parents: vec![],
                operation: OperationId::new(),
                payload: crate::edit::SnapshotPayload::Content(b"saved HDR".to_vec()),
                origin: VersionOrigin::Observed,
            })
            .unwrap();
        let engine = ImportCoordinator::new(config, catalog);
        let status = StatusSnapshot::collect_page(
            &engine,
            &super::super::space::SpaceMeter::default(),
            serde_json::Value::Null,
            serde_json::Value::Null,
            &super::super::protocol::StatusQuery::default(),
        )
        .unwrap();
        assert_eq!(status.activity["generated"]["total"], 1);
        assert_eq!(status.activity["generated"]["pending"], 1);
    }

    #[test]
    fn activity_ignores_cancelled_uploads_and_finds_active_jobs_beyond_the_page() {
        let temp = tempfile::tempdir().unwrap();
        for dir in ["local", "state", "archive"] {
            std::fs::create_dir(temp.path().join(dir)).unwrap();
        }
        let config = crate::config::Config::new(
            temp.path().join("local"),
            temp.path().join("state"),
            temp.path().join("archive"),
        );
        let root =
            crate::fs::RootHandle::open(&config.state_root, crate::fs::RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let mut active = None;
        for index in 0..130 {
            let id = catalog
                .enqueue(DurableRequest {
                    operation_id: OperationId::new(),
                    kind: JobKind::ArchiveInit,
                })
                .unwrap();
            let sorted = format!("00000000-0000-0000-0000-{index:012x}");
            catalog
                .db
                .execute(
                    "UPDATE jobs SET id=? WHERE id=?",
                    rusqlite::params![sorted, id.to_string()],
                )
                .unwrap();
            let id: JobId = serde_json::from_value(sorted.into()).unwrap();
            catalog
                .db
                .execute(
                    "UPDATE jobs SET state=? WHERE id=?",
                    rusqlite::params![
                        if index == 129 {
                            "\"WaitingNas\""
                        } else {
                            "\"Cancelled\""
                        },
                        id.to_string()
                    ],
                )
                .unwrap();
            if index == 129 {
                active = Some(id);
            }
            catalog.db.execute("INSERT INTO import_items(id,parent,source_root,source_path,digest,size,role,destination,state) VALUES(?,?,?,?,?,1,'raw',?,'planned')", rusqlite::params![JobId::new().to_string(),id.to_string(),"/card",format!("{index}.ORF"),"digest","image.ORF"]).unwrap();
        }
        let active = active.unwrap();
        let engine = ImportCoordinator::new(config, catalog);
        let status = StatusSnapshot::collect_page(
            &engine,
            &super::super::space::SpaceMeter::default(),
            serde_json::json!({}),
            serde_json::json!({}),
            &super::super::protocol::StatusQuery::default(),
        )
        .unwrap();
        assert_eq!(status.activity["active_count"], 1);
        assert_eq!(status.activity["pending_upload"], 1);
        assert_eq!(
            status.activity["active"][0]["job"]["id"],
            active.to_string()
        );
    }
}
