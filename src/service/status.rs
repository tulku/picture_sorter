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

        Ok(Self {
            session_metrics: crate::metrics::snapshot(),
            pagination,
            schema_version: 1,
            protocol_version: 1,
            profile_id: engine.config.profile_id,
            writer_id: engine.config.writer_id,
            archive_id: engine.config.archive_id,
            archive,
            capabilities: serde_json::json!({"nas_publication":{"allowed":false,"reason":"deployment qualification must match this mount and writer mode"},"immich_ratings":{"allowed":false,"reason":"outbound writes require matching runtime evidence and explicit best_effort"},"immich_favorites":{"allowed":false,"reason":"Favorite writes require separate runtime evidence, native Pick qualification and explicit best_effort"}}),
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
