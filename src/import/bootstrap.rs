use super::*;
use crate::{
    archive::*,
    catalog::*,
    config::Config,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    transfer::*,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapRequest {
    pub operation: OperationId,
    pub archive: ArchiveId,
    pub scope: Option<ArchivePath>,
    pub batch_size: usize,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BootstrapReport {
    pub complete: bool,
    pub originals: u64,
    pub companions: u64,
    pub skipped: u64,
    pub metadata_calls: u64,
    pub reason: Option<String>,
}
#[derive(Default)]
pub struct BootstrapDriver {
    active: Option<Active>,
}
impl BootstrapDriver {
    pub fn cancel(&mut self, worker: &mut WorkerSupervisor, job: JobId) -> AppResult<()> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        if active.job != job {
            self.active = Some(active);
            return Ok(());
        }
        match worker.poll(active.ticket)? {
            WorkerState::Active | WorkerState::TimedOutStillAlive | WorkerState::Reaped => {
                self.active = Some(active)
            }
            WorkerState::Completed(reply) => {
                if let NasReply::Session { session, .. } = reply {
                    active.session = Some(session);
                }
                if !matches!(active.phase, Phase::End | Phase::Abort)
                    && let Some(session) = active.session
                {
                    active.phase = Phase::Abort;
                    active.ticket = worker.submit(NasCommand::EndArchive { session })?;
                    self.active = Some(active);
                }
            }
            WorkerState::Failed(_) => {
                if matches!(active.phase, Phase::End | Phase::Abort) {
                    worker.mark_uncertain();
                    self.active = Some(active);
                } else if let Some(session) = active.session {
                    active.phase = Phase::Abort;
                    active.ticket = worker.submit(NasCommand::EndArchive { session })?;
                    self.active = Some(active);
                }
            }
        }
        Ok(())
    }
    pub fn active_job(&self) -> Option<JobId> {
        self.active.as_ref().map(|active| active.job)
    }
}
#[derive(Clone)]
struct Active {
    job: JobId,
    ticket: WorkerTicket,
    phase: Phase,
    session: Option<OperationId>,
    batch: Option<JournalBatch>,
    index: usize,
    inventory: Option<ArchiveInventoryBatch>,
    plan: Option<PublicationPlan>,
}
#[derive(Clone, Copy)]
enum Phase {
    Begin,
    List,
    Read,
    Ack,
    InventoryStart,
    InventoryNext,
    Metadata,
    Publish,
    End,
    Abort,
    Drain,
}
pub struct BootstrapCoordinator<'a> {
    config: &'a Config,
    catalog: &'a mut Catalog,
    driver: &'a mut BootstrapDriver,
    worker: &'a mut WorkerSupervisor,
}
impl<'a> BootstrapCoordinator<'a> {
    pub fn new(
        config: &'a Config,
        catalog: &'a mut Catalog,
        driver: &'a mut BootstrapDriver,
        worker: &'a mut WorkerSupervisor,
    ) -> Self {
        Self {
            config,
            catalog,
            driver,
            worker,
        }
    }
    pub fn start(&mut self, request: BootstrapRequest) -> AppResult<JobId> {
        if Some(request.archive) != self.config.archive_id
            || request.batch_size == 0
            || request.batch_size > 128
        {
            return Err(AppError::invalid(
                "bootstrap requires matching archive and batch 1..128",
            ));
        }
        if request
            .scope
            .as_ref()
            .is_some_and(|p| p.as_str().starts_with(".photo-sorter"))
        {
            return Err(AppError::invalid("bootstrap scope cannot be tool metadata"));
        }
        let job = self.catalog.enqueue(DurableRequest {
            operation_id: request.operation,
            kind: JobKind::Bootstrap {
                scope: request.scope.clone(),
            },
        })?;
        let old: Option<String> = self
            .catalog
            .db
            .query_row(
                "SELECT request FROM bootstrap_scans WHERE job=?",
                [job.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            if old != serde_json::to_string(&request)? {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "bootstrap operation parameters changed",
                ));
            }
        } else {
            self.catalog.db.execute(
                "INSERT INTO bootstrap_scans(job,request,report) VALUES(?,?,?)",
                params![
                    job.to_string(),
                    serde_json::to_string(&request)?,
                    serde_json::to_string(&BootstrapReport::default())?
                ],
            )?;
        }
        Ok(job)
    }
    pub fn report(&self, job: JobId) -> AppResult<BootstrapReport> {
        let text: String = self.catalog.db.query_row(
            "SELECT report FROM bootstrap_scans WHERE job=?",
            [job.to_string()],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&text)?)
    }
    pub fn advance(&mut self, job: JobId) -> AppResult<BootstrapReport> {
        let backup = self.driver.active.clone();
        let result = self.advance_inner(job);
        if result.is_err()
            && self.driver.active.is_none()
            && let Some(mut active) = backup
            && let Some(ticket) = self.worker.latest_ticket()
        {
            active.ticket = ticket;
            active.phase = Phase::Drain;
            self.driver.active = Some(active);
        }
        result
    }
    fn advance_inner(&mut self, job: JobId) -> AppResult<BootstrapReport> {
        let mut report = self.report(job)?;
        if report.complete {
            return Ok(report);
        }
        let text: String = self.catalog.db.query_row(
            "SELECT request FROM bootstrap_scans WHERE job=?",
            [job.to_string()],
            |r| r.get(0),
        )?;
        let request: BootstrapRequest = serde_json::from_str(&text)?;
        if self.driver.active.is_none() {
            if !self.worker.available()? {
                return Ok(report);
            }
            let ticket = self.worker.submit(NasCommand::BeginArchive {
                root: self.config.archive_root.clone(),
                archive: request.archive,
                writer: self.config.writer_id,
            })?;
            self.driver.active = Some(Active {
                job,
                ticket,
                phase: Phase::Begin,
                session: None,
                batch: None,
                index: 0,
                inventory: None,
                plan: None,
            });
            return Ok(report);
        }
        let mut active = self
            .driver
            .active
            .take()
            .ok_or_else(|| AppError::integrity("bootstrap state missing"))?;
        if active.job != job {
            self.driver.active = Some(active);
            return Ok(report);
        }
        let reply = match self.worker.poll(active.ticket)? {
            WorkerState::Active | WorkerState::TimedOutStillAlive => {
                self.driver.active = Some(active);
                return Ok(report);
            }
            WorkerState::Failed(e) => {
                if matches!(active.phase, Phase::List) {
                    self.catalog.block_journal_enumeration(&e.to_string())?;
                }
                report.reason = Some(e.to_string());
                let record = self.catalog.job(job)?;
                self.catalog.transition(
                    job,
                    record.generation,
                    JobTransition::Wait(JobState::WaitingNas, e.to_string()),
                )?;
                if let Some(session) = active.session {
                    if matches!(active.phase, Phase::Abort) {
                        self.worker.mark_uncertain();
                        self.driver.active = Some(active);
                    } else {
                        active.phase = Phase::Abort;
                        active.ticket = self.worker.submit(NasCommand::EndArchive { session })?;
                        self.driver.active = Some(active);
                    }
                }
                self.save(job, &report)?;
                return Ok(report);
            }
            WorkerState::Reaped => {
                report.reason =
                    Some("worker disappeared; explicit abandoned-lock recovery is required".into());
                self.driver.active = Some(active);
                self.save(job, &report)?;
                return Ok(report);
            }
            WorkerState::Completed(reply) => reply,
        };
        let command = match (active.phase, reply) {
            (Phase::Drain, reply) => {
                if let NasReply::Session { session, .. } = reply {
                    active.session = Some(session);
                }
                if let Some(session) = active.session {
                    active.phase = Phase::Abort;
                    NasCommand::EndArchive { session }
                } else {
                    return Ok(report);
                }
            }
            (Phase::Abort, NasReply::Done) => return Ok(report),
            (Phase::Begin, NasReply::Session { session, .. }) => {
                active.session = Some(session);
                active.phase = Phase::List;
                NasCommand::JournalNext {
                    session,
                    limit: 128,
                }
            }
            (Phase::List, NasReply::Journal(batch)) => {
                active.batch = Some(batch);
                active.index = 0;
                self.replay_next(&mut active)?
            }
            (Phase::Read, NasReply::Commit(commit)) => {
                self.catalog.apply_commit(&commit)?;
                active.index += 1;
                self.replay_next(&mut active)?
            }
            (Phase::Ack, NasReply::Done) => {
                if active.batch.as_ref().is_some_and(|b| b.end) {
                    while self.catalog.drain_pending(128)? > 0 {}
                    if !matches!(
                        self.catalog.publication_readiness()?,
                        PublicationReadiness::Ready
                    ) {
                        return Err(AppError::integrity(
                            "bootstrap blocked by incomplete or invalid journal",
                        ));
                    }
                    let pending: Option<String> = self.catalog.db.query_row(
                        "SELECT pending FROM bootstrap_scans WHERE job=?",
                        [job.to_string()],
                        |r| r.get(0),
                    )?;
                    if let Some(pending) = pending {
                        let plan: PublicationPlan = serde_json::from_str(&pending)?;
                        active.plan = Some(plan.clone());
                        active.phase = Phase::Publish;
                        NasCommand::Publish {
                            session: session(&active)?,
                            plan,
                        }
                    } else {
                        active.phase = Phase::InventoryStart;
                        NasCommand::InventoryStart {
                            session: session(&active)?,
                            scope: request.scope.clone(),
                        }
                    }
                } else {
                    active.phase = Phase::List;
                    NasCommand::JournalNext {
                        session: session(&active)?,
                        limit: 128,
                    }
                }
            }
            (Phase::InventoryStart, NasReply::Done) => {
                active.phase = Phase::InventoryNext;
                NasCommand::InventoryNext {
                    session: session(&active)?,
                    limit: request.batch_size,
                }
            }
            (Phase::InventoryNext, NasReply::Inventory(batch)) => {
                for inspection in &batch.jpeg_inspections {
                    if !batch.entries.iter().any(|entry| {
                        matches!(entry.role, SourceRole::Original(MediaRole::Jpeg))
                            && entry.digest == inspection.digest
                            && entry.size == inspection.size
                    }) {
                        return Err(AppError::integrity(
                            "JPEG fingerprint left its archive inventory batch",
                        ));
                    }
                    self.catalog.cache_jpeg_inspection(inspection)?;
                }
                report.skipped = batch.skipped;
                if batch.end {
                    active.phase = Phase::End;
                    NasCommand::EndArchive {
                        session: session(&active)?,
                    }
                } else {
                    let mut paths = vec![];
                    for entry in &batch.entries {
                        if matches!(entry.role, SourceRole::Original(_))
                            && self.catalog.metadata(&entry.digest)?.is_none()
                        {
                            paths.push(entry.path.clone());
                        }
                    }
                    active.inventory = Some(batch);
                    if paths.is_empty() {
                        self.prepare(&mut active, vec![], &mut report)?
                    } else {
                        active.phase = Phase::Metadata;
                        NasCommand::InventoryMetadata {
                            session: session(&active)?,
                            paths,
                        }
                    }
                }
            }
            (Phase::Metadata, NasReply::Metadata(metadata)) => {
                report.metadata_calls += 1;
                self.prepare(&mut active, metadata, &mut report)?
            }
            (Phase::Publish, NasReply::Published(receipt)) => {
                self.catalog.apply_commit(&receipt.commit)?;
                let tx = self.catalog.db.transaction()?;
                tx.execute(
                    "UPDATE bootstrap_scans SET pending=NULL WHERE job=?",
                    [job.to_string()],
                )?;
                if let Some(batch) = &active.inventory {
                    for entry in &batch.entries {
                        let inserted=tx.execute("INSERT INTO bootstrap_observations(job,path,digest) VALUES(?,?,?) ON CONFLICT DO NOTHING",params![job.to_string(),entry.path.as_str(),entry.digest.as_str()])?;
                        if inserted > 0 {
                            match entry.role {
                                SourceRole::Original(_) => report.originals += 1,
                                SourceRole::Companion(_) => report.companions += 1,
                            };
                        }
                    }
                }
                tx.execute(
                    "UPDATE bootstrap_scans SET report=? WHERE job=?",
                    params![serde_json::to_string(&report)?, job.to_string()],
                )?;
                tx.commit()?;
                if active.inventory.is_none() {
                    active.phase = Phase::InventoryStart;
                    NasCommand::InventoryStart {
                        session: session(&active)?,
                        scope: request.scope.clone(),
                    }
                } else {
                    active.inventory = None;
                    active.phase = Phase::InventoryNext;
                    NasCommand::InventoryNext {
                        session: session(&active)?,
                        limit: request.batch_size,
                    }
                }
            }
            (Phase::End, NasReply::Done) => {
                report.complete = true;
                report.reason = None;
                self.save(job, &report)?;
                let record = self.catalog.job(job)?;
                self.catalog
                    .transition(job, record.generation, JobTransition::Archived)?;
                self.catalog
                    .transition(job, record.generation, JobTransition::Complete)?;
                return Ok(report);
            }
            _ => return Err(AppError::integrity("unexpected bootstrap worker response")),
        };
        active.ticket = self.worker.submit(command)?;
        self.driver.active = Some(active);
        self.save(job, &report)?;
        Ok(report)
    }
    fn save(&self, job: JobId, report: &BootstrapReport) -> AppResult<()> {
        self.catalog.db.execute(
            "UPDATE bootstrap_scans SET report=? WHERE job=?",
            params![serde_json::to_string(report)?, job.to_string()],
        )?;
        Ok(())
    }
    fn replay_next(&self, active: &mut Active) -> AppResult<NasCommand> {
        let batch = active
            .batch
            .as_ref()
            .ok_or_else(|| AppError::integrity("no journal batch"))?;
        while active.index < batch.entries.len() {
            let entry = &batch.entries[active.index];
            let known: bool = self.catalog.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM commits WHERE id=? AND applied=1)",
                [entry.id.to_string()],
                |r| r.get(0),
            )?;
            if known {
                active.index += 1;
            } else {
                active.phase = Phase::Read;
                return Ok(NasCommand::JournalRead {
                    session: session(active)?,
                    path: entry.path.clone(),
                });
            }
        }
        active.phase = Phase::Ack;
        Ok(NasCommand::JournalAck {
            session: session(active)?,
            token: batch.token,
        })
    }
    fn prepare(
        &mut self,
        active: &mut Active,
        metadata: Vec<MetadataResult>,
        report: &mut BootstrapReport,
    ) -> AppResult<NasCommand> {
        let batch = active
            .inventory
            .as_ref()
            .ok_or_else(|| AppError::integrity("no inventory batch"))?;
        let mut records = vec![];
        for result in metadata {
            if !batch.entries.iter().any(|e| e.digest == result.digest) {
                return Err(AppError::new(
                    ErrorCode::SourceChanged,
                    "bootstrap metadata digest changed",
                ));
            }
            self.catalog.cache_metadata(&result)?;
            records.push(ArchiveRecord::MetadataCached { result });
        }
        for entry in &batch.entries {
            match entry.role {
                SourceRole::Original(role) => {
                    let old: Option<(String, String)> = self
                        .catalog
                        .db
                        .query_row(
                            "SELECT group_id,digest FROM members WHERE archive=? AND path=?",
                            params![
                                self.config.archive_id.unwrap().to_string(),
                                entry.path.as_str()
                            ],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?;
                    let group = if let Some((group, digest)) = old {
                        if digest != entry.digest.as_str() {
                            self.catalog.block_record(
                                entry.path.as_str(),
                                "bootstrap found changed immutable original",
                            )?;
                            return Err(AppError::integrity(
                                "existing committed original changed; preserved and protected",
                            ));
                        }
                        let archived: bool = self.catalog.db.query_row(
                            "SELECT EXISTS(SELECT 1 FROM placements WHERE group_id=? AND path=? AND digest=? AND archived=1)",
                            params![group, entry.path.as_str(), entry.digest.as_str()],
                            |row| row.get(0),
                        )?;
                        if archived {
                            continue;
                        }
                        serde_json::from_value(group.into())?
                    } else {
                        GroupId::new()
                    };
                    records.push(ArchiveRecord::GroupCreated { id: group });
                    records.push(ArchiveRecord::MemberAttached {
                        group,
                        member: OriginalMember {
                            digest: entry.digest.clone(),
                            size: entry.size,
                            path: entry.path.clone(),
                            role,
                        },
                        evidence: PairEvidence::ObservedPlacement {
                            path: entry.path.clone(),
                            digest: entry.digest.clone(),
                        },
                    });
                }
                SourceRole::Companion(_) => {
                    let old: Option<String> = self
                        .catalog
                        .db
                        .query_row(
                            "SELECT id FROM sidecars WHERE path=? ORDER BY id LIMIT 1",
                            [entry.path.as_str()],
                            |r| r.get(0),
                        )
                        .optional()?;
                    let (sidecar, heads) = if let Some(id) = old {
                        let id = serde_json::from_str(&format!("\"{id}\""))?;
                        (id, self.catalog.heads(id)?)
                    } else {
                        (self.catalog.managed_sidecar_id(&entry.path)?, vec![])
                    };
                    let body = VersionBody::Content {
                        digest: entry.digest.clone(),
                        size: entry.size,
                    };
                    if heads
                        .iter()
                        .any(|id| self.catalog.version(*id).is_ok_and(|v| v.body == body))
                    {
                        continue;
                    }
                    if heads.is_empty() {
                        records.push(ArchiveRecord::SidecarCreated {
                            id: sidecar,
                            group: None,
                            path: entry.path.clone(),
                            owner: "bootstrap-unassociated".into(),
                        });
                        records.push(ArchiveRecord::AmbiguityRecorded{id:ConflictId::new(),kind:"orphan_sidecar".into(),group:None,details:serde_json::json!({"sidecar":sidecar,"path":entry.path,"digest":entry.digest})});
                    }
                    let version = Version {
                        id: VersionId::new(),
                        sidecar,
                        parents: vec![],
                        body,
                        origin: VersionOrigin::Observed,
                    };
                    records.push(ArchiveRecord::VersionAdded {
                        version: version.clone(),
                        operation: OperationId::new(),
                    });
                    let mut next = heads.clone();
                    next.push(version.id);
                    records.push(ArchiveRecord::HeadsChanged {
                        sidecar,
                        expected: heads.clone(),
                        heads: next.clone(),
                    });
                    if !heads.is_empty() {
                        records.push(ArchiveRecord::ConflictRecorded {
                            id: ConflictId::new(),
                            group: None,
                            kind: "bootstrap-sidecar".into(),
                            heads: next,
                        });
                    }
                }
            }
        }
        let operation = OperationId::new();
        let plan = self.catalog.prepare_publication(
            self.config.archive_id.unwrap(),
            self.config.writer_id,
            operation,
            records,
        )?;
        self.catalog.db.execute(
            "UPDATE bootstrap_scans SET pending=?,report=? WHERE job=?",
            params![
                serde_json::to_string(&plan)?,
                serde_json::to_string(report)?,
                active.job.to_string()
            ],
        )?;
        active.plan = Some(plan.clone());
        active.phase = Phase::Publish;
        Ok(NasCommand::Publish {
            session: session(active)?,
            plan,
        })
    }
}
fn session(active: &Active) -> AppResult<OperationId> {
    active
        .session
        .ok_or_else(|| AppError::integrity("bootstrap has no session"))
}
