use crate::{
    archive::*,
    catalog::*,
    config::Config,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::{RootHandle, RootPolicy},
    import::ImportCoordinator,
    transfer::*,
};
use rusqlite::{OptionalExtension, params};
#[derive(Default)]
pub(crate) struct ArchiveReader {
    active: Option<Active>,
    pub last_success: Option<Timestamp>,
    pub completed_syncs: u64,
    pub reason: Option<String>,
    pub requested: bool,
    retry_at: Timestamp,
}
struct Active {
    config: Config,
    job: Option<JobId>,
    mode: Mode,
    ticket: WorkerTicket,
    phase: Phase,
    session: Option<OperationId>,
    batch: Option<JournalBatch>,
    index: usize,
    report: IntegrityReport,
    object_cursor: i64,
    objects_done: bool,
    deferred: Option<(NasCommand, bool)>,
}
enum Mode {
    Sync,
    Verify {
        catalog: Catalog,
        _root: RootHandle,
    },
    Rebuild {
        catalog: Catalog,
        root: RootHandle,
        report: RecoveryReport,
    },
}
#[derive(Clone, Copy)]
enum Phase {
    Backup,
    Begin,
    List,
    Read,
    Objects,
    End,
    LocalStart,
    LocalRead,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct RebuildIntent {
    schema_version: u32,
    job: JobId,
    profile: ProfileId,
    archive: ArchiveId,
    writer: WriterId,
}
impl ArchiveReader {
    pub fn request_fresh_sync(&mut self) -> u64 {
        self.requested = true;
        self.retry_at = 0;
        self.reason = None;
        self.completed_syncs
            + 1
            + u64::from(
                self.active
                    .as_ref()
                    .is_some_and(|a| matches!(a.mode, Mode::Sync)),
            )
    }
    pub fn busy(&self) -> bool {
        self.active.is_some()
    }
    pub fn active_job(&self) -> Option<JobId> {
        self.active.as_ref().and_then(|a| a.job)
    }
    pub fn start_verify(
        &mut self,
        engine: &mut ImportCoordinator,
        state: &RootHandle,
        worker: &mut WorkerSupervisor,
        job: JobId,
    ) -> AppResult<()> {
        if self.busy() || !worker.available()? {
            return Ok(());
        }
        let path = ArchivePath::parse(&format!(".photo-sorter/audits/{job}"))?;
        state.mkdir_all(&path)?;
        let root = RootHandle::open(&state.path().join(path.as_str()), RootPolicy::Local)?;
        let catalog = Catalog::open(&root)?;
        let ticket = worker.submit(start(&engine.config)?)?;
        self.active = Some(Active {
            config: engine.config.clone(),
            job: Some(job),
            mode: Mode::Verify {
                catalog,
                _root: root,
            },
            ticket,
            phase: Phase::Begin,
            session: None,
            batch: None,
            index: 0,
            report: IntegrityReport::default(),
            object_cursor: 0,
            objects_done: false,
            deferred: None,
        });
        Ok(())
    }
    pub fn start_rebuild(
        &mut self,
        engine: &mut ImportCoordinator,
        maintenance: &mut WorkerSupervisor,
        job: JobId,
        replacement: &std::path::Path,
    ) -> AppResult<()> {
        if self.busy() || !maintenance.available()? {
            return Ok(());
        }
        let config = &engine.config;
        let root = RootHandle::open(replacement, RootPolicy::Local)?;
        for original in [
            &config.state_root,
            &config.working_root,
            &config.archive_root,
        ] {
            if replacement.starts_with(original) || original.starts_with(replacement) {
                return Err(AppError::invalid(
                    "replacement state cannot overlap a managed root",
                ));
            }
        }
        let intent_path = ArchivePath::parse("rebuild-intent.json")?;
        let intent = match root.read(&intent_path, 64 * 1024) {
            Ok(bytes) => {
                let intent: RebuildIntent = serde_json::from_slice(&bytes)?;
                if intent.schema_version != 1
                    || intent.job != job
                    || intent.profile != config.profile_id
                    || Some(intent.archive) != config.archive_id
                {
                    return Err(AppError::new(
                        ErrorCode::Conflict,
                        "replacement belongs to another recovery operation",
                    ));
                }
                intent
            }
            Err(error) if error.errno == Some(libc::ENOENT) => {
                if std::fs::read_dir(replacement)?.next().is_some() {
                    return Err(AppError::invalid(
                        "replacement must be an empty local directory",
                    ));
                }
                let intent = RebuildIntent {
                    schema_version: 1,
                    job,
                    profile: config.profile_id,
                    archive: config
                        .archive_id
                        .ok_or_else(|| AppError::invalid("archive binding required"))?,
                    writer: WriterId::new(),
                };
                root.write_new(&intent_path, &serde_json::to_vec(&intent)?)?;
                intent
            }
            Err(error) => return Err(error),
        };
        root.mkdir_all(&ArchivePath::parse("preserved")?)?;
        let catalog = Catalog::open(&root)?;
        catalog.db.execute(
            "INSERT INTO archives(id,writer) VALUES(?,?) ON CONFLICT DO NOTHING",
            params![intent.archive.to_string(), intent.writer.to_string()],
        )?;
        let default = root.path().join("preserved/live-catalog.sqlite");
        let destination = if default.exists() {
            root.path()
                .join(format!("preserved/attempt-{}.sqlite", OperationId::new()))
        } else {
            default
        };
        let ticket = maintenance.submit(NasCommand::BackupCatalog {
            state: config.state_root.clone(),
            destination,
        })?;
        let report=RecoveryReport{writer:intent.writer,replacement:replacement.to_path_buf(),private_state_lost:true,local_observations:0,commits_applied:0,limitations:vec!["Pins, private jobs and unpublished version ancestry are not reconstructed from shared records; existing local bytes and old state remain intact.".into(),"The running service continues to own the original profile; switching to this new state and writer requires an explicit stopped-service deployment and archive ownership qualification.".into()]};
        self.active = Some(Active {
            config: engine.config.clone(),
            job: Some(job),
            mode: Mode::Rebuild {
                catalog,
                root,
                report,
            },
            ticket,
            phase: Phase::Backup,
            session: None,
            batch: None,
            index: 0,
            report: IntegrityReport::default(),
            object_cursor: 0,
            objects_done: false,
            deferred: None,
        });
        Ok(())
    }
    pub fn advance(
        &mut self,
        engine: &mut ImportCoordinator,
        state: &RootHandle,
        worker: &mut WorkerSupervisor,
        maintenance: &mut WorkerSupervisor,
    ) -> AppResult<()> {
        let now = SystemClock::default().now();
        if self.active.is_none() {
            if !self.requested || now < self.retry_at || !worker.available()? {
                return Ok(());
            }
            let ticket = worker.submit(start(&engine.config)?)?;
            self.requested = false;
            self.active = Some(Active {
                config: engine.config.clone(),
                job: None,
                mode: Mode::Sync,
                ticket,
                phase: Phase::Begin,
                session: None,
                batch: None,
                index: 0,
                report: IntegrityReport::default(),
                object_cursor: 0,
                objects_done: true,
                deferred: None,
            });
            return Ok(());
        }
        let mut active = self
            .active
            .take()
            .ok_or_else(|| AppError::integrity("archive reader state missing"))?;
        if let Some((command, local)) = active.deferred.take() {
            let worker = if local {
                &mut *maintenance
            } else {
                &mut *worker
            };
            if !worker.available()? {
                active.deferred = Some((command, local));
                self.active = Some(active);
                return Ok(());
            }
            active.ticket = worker.submit(command)?;
            self.active = Some(active);
            return Ok(());
        }
        let local_phase = matches!(
            active.phase,
            Phase::Backup | Phase::LocalStart | Phase::LocalRead
        );
        let status = if local_phase {
            maintenance.poll(active.ticket)?
        } else {
            worker.poll(active.ticket)?
        };
        let reply = match status {
            WorkerState::Active | WorkerState::TimedOutStillAlive => {
                self.active = Some(active);
                return Ok(());
            }
            WorkerState::Reaped => {
                self.reason = Some(
                    "read worker disappeared; replacement requires proven process quiescence"
                        .into(),
                );
                self.active = Some(active);
                return Ok(());
            }
            WorkerState::Failed(error) => {
                self.reason = Some(error.to_string());
                if matches!(active.phase, Phase::Read) {
                    let entry = active
                        .batch
                        .as_ref()
                        .and_then(|b| b.entries.get(active.index))
                        .ok_or_else(|| AppError::integrity("failed read has no descriptor"))?;
                    let finding = IntegrityFinding {
                        path: entry.path.clone(),
                        status: if error.code == ErrorCode::Unsupported {
                            IntegrityStatus::Unknown
                        } else {
                            IntegrityStatus::Corrupt
                        },
                        reason: error.to_string(),
                    };
                    record_finding(engine, &mut active, finding)?;
                    active.index += 1;
                    let command = next_read(&engine.catalog, &mut active)?;
                    active.ticket = worker.submit(command)?;
                    self.active = Some(active);
                    return Ok(());
                }
                if let Some(job) = active.job {
                    block_job(
                        &mut engine.catalog,
                        job,
                        JobState::WaitingNas,
                        &error.to_string(),
                    )?;
                }
                self.retry_at = now + engine.config.poll_seconds;
                self.requested = active.job.is_none();
                return Ok(());
            }
            WorkerState::Completed(reply) => reply,
        };
        let (command, local) = match (active.phase, reply) {
            (Phase::Backup, NasReply::Done) => {
                active.phase = Phase::Begin;
                (start(&engine.config)?, false)
            }
            (Phase::Begin, NasReply::ReadSession(session)) => {
                active.session = Some(session);
                active.phase = Phase::List;
                (
                    NasCommand::ReadJournalNext {
                        session,
                        limit: 128,
                    },
                    false,
                )
            }
            (Phase::List, NasReply::Journal(batch)) => {
                active.batch = Some(batch);
                active.index = 0;
                (next_read(&engine.catalog, &mut active)?, false)
            }
            (Phase::Read, NasReply::Commit(commit)) => {
                let result = match &mut active.mode {
                    Mode::Sync => engine.catalog.apply_commit(&commit),
                    Mode::Verify { catalog, .. } | Mode::Rebuild { catalog, .. } => {
                        catalog.apply_commit(&commit)
                    }
                };
                active.report.checked += 1;
                if let Err(error) = result {
                    let entry = active.batch.as_ref().unwrap().entries[active.index]
                        .path
                        .clone();
                    record_finding(
                        engine,
                        &mut active,
                        IntegrityFinding {
                            path: entry,
                            status: IntegrityStatus::Corrupt,
                            reason: error.to_string(),
                        },
                    )?;
                } else if let Mode::Rebuild { report, .. } = &mut active.mode {
                    report.commits_applied += 1;
                }
                active.index += 1;
                (next_read(&engine.catalog, &mut active)?, false)
            }
            (Phase::Objects, NasReply::Integrity(report)) => {
                active.report.checked += report.checked;
                for finding in report.findings {
                    record_finding(engine, &mut active, finding)?;
                }
                (next_objects(&engine.catalog, &mut active)?, false)
            }
            (Phase::End, NasReply::Done) => {
                let readiness = match &mut active.mode {
                    Mode::Sync => {
                        engine.catalog.drain_pending(128)?;
                        engine.catalog.publication_readiness()?
                    }
                    Mode::Verify { catalog, .. } | Mode::Rebuild { catalog, .. } => {
                        while catalog.drain_pending(128)? > 0 {}
                        catalog.publication_readiness()?
                    }
                };
                if let PublicationReadiness::Blocked(reasons) = readiness {
                    for reason in reasons.into_iter().take(128) {
                        record_finding(
                            engine,
                            &mut active,
                            IntegrityFinding {
                                path: ArchivePath::parse(".photo-sorter/journal")?,
                                status: IntegrityStatus::Unknown,
                                reason,
                            },
                        )?;
                    }
                }
                match &mut active.mode {
                    Mode::Sync => {
                        if active.report.findings.is_empty() {
                            self.last_success = Some(now);
                            self.completed_syncs += 1;
                            self.reason = None;
                        }
                        self.retry_at = if self.requested {
                            now
                        } else {
                            now + engine.config.poll_seconds
                        };
                        return Ok(());
                    }
                    Mode::Verify { .. } => {
                        finish_report(&mut engine.catalog, &active)?;
                        self.last_success = Some(now);
                        return Ok(());
                    }
                    Mode::Rebuild { .. } => {
                        if !active.report.findings.is_empty() {
                            finish_report(&mut engine.catalog, &active)?;
                            return Ok(());
                        }
                        active.phase = Phase::LocalStart;
                        (
                            NasCommand::LocalForensicStart {
                                root: engine.config.working_root.clone(),
                            },
                            true,
                        )
                    }
                }
            }
            (Phase::LocalStart, NasReply::Done) => {
                active.phase = Phase::LocalRead;
                (NasCommand::LocalInventoryHashNext { limit: 128 }, true)
            }
            (Phase::LocalRead, NasReply::LocalObserved { entries, end, .. }) => {
                let Mode::Rebuild {
                    catalog,
                    root,
                    report,
                } = &mut active.mode
                else {
                    return Err(AppError::integrity("local inventory has no rebuild target"));
                };
                for entry in entries {
                    catalog.db.execute("INSERT INTO observations(path,digest,size,readable) VALUES(?,?,?,1) ON CONFLICT(path) DO UPDATE SET digest=excluded.digest,size=excluded.size,readable=1",params![entry.path.as_str(),entry.digest.as_str(),entry.size])?;
                    report.local_observations += 1;
                }
                if end {
                    catalog.db.execute("UPDATE residency SET pin=0,protected=1,ready_revision=NULL,metadata_current=0",[])?;
                    let path = ArchivePath::parse("recovery.json")?;
                    let bytes = serde_json::to_vec(report)?;
                    match root.write_new(&path, &bytes) {
                        Ok(()) => {}
                        Err(error) if error.errno == Some(libc::EEXIST) => {
                            let alternate = ArchivePath::parse(&format!(
                                "recovery-{}.json",
                                OperationId::new()
                            ))?;
                            root.write_new(&alternate, &bytes)?;
                        }
                        Err(error) => return Err(error),
                    }
                    let job = active
                        .job
                        .ok_or_else(|| AppError::integrity("rebuild job missing"))?;
                    let value = serde_json::to_string(report)?;
                    engine.catalog.db.execute("INSERT INTO job_results(job,result) VALUES(?,?) ON CONFLICT(job) DO UPDATE SET result=excluded.result",params![job.to_string(),value])?;
                    let record = engine.catalog.job(job)?;
                    engine
                        .catalog
                        .transition(job, record.generation, JobTransition::Complete)?;
                    return Ok(());
                }
                (NasCommand::LocalInventoryHashNext { limit: 128 }, true)
            }
            _ => {
                return Err(AppError::integrity(
                    "unexpected read-only archive worker reply",
                ));
            }
        };
        let destination = if local { maintenance } else { worker };
        if destination.available()? {
            active.ticket = destination.submit(command)?;
        } else {
            active.deferred = Some((command, local));
        }
        self.active = Some(active);
        let _ = state;
        Ok(())
    }
}
fn start(config: &Config) -> AppResult<NasCommand> {
    Ok(NasCommand::ReadJournalStart {
        root: config.archive_root.clone(),
        archive: config
            .archive_id
            .ok_or_else(|| AppError::invalid("archive binding required"))?,
        writer: config.writer_id,
    })
}
fn next_read(main: &Catalog, active: &mut Active) -> AppResult<NasCommand> {
    let session = active
        .session
        .ok_or_else(|| AppError::integrity("read session missing"))?;
    let batch = active
        .batch
        .as_ref()
        .ok_or_else(|| AppError::integrity("read batch missing"))?;
    while active.index < batch.entries.len() {
        let entry = &batch.entries[active.index];
        let known = if matches!(active.mode, Mode::Sync) {
            main.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM commits WHERE id=? AND applied=1)",
                [entry.id.to_string()],
                |r| r.get::<_, bool>(0),
            )?
        } else {
            false
        };
        if known {
            active.index += 1;
            continue;
        }
        active.phase = Phase::Read;
        return Ok(NasCommand::ReadJournalCommit {
            session,
            path: entry.path.clone(),
        });
    }
    if batch.end {
        if matches!(active.mode, Mode::Verify { .. }) {
            return next_objects(main, active);
        }
        active.phase = Phase::End;
        Ok(NasCommand::ReadJournalEnd { session })
    } else {
        active.phase = Phase::List;
        Ok(NasCommand::ReadJournalNext {
            session,
            limit: 128,
        })
    }
}
fn next_objects(main: &Catalog, active: &mut Active) -> AppResult<NasCommand> {
    let config = active.config.clone();
    let session = active
        .session
        .ok_or_else(|| AppError::integrity("read session missing"))?;
    if active.objects_done {
        active.phase = Phase::End;
        return Ok(NasCommand::ReadJournalEnd { session });
    }
    let mut objects = vec![];
    let mut query=main.db.prepare("SELECT p.rowid,p.path,p.digest,b.size FROM placements p JOIN blobs b ON b.digest=p.digest WHERE archived=1 AND p.rowid>? ORDER BY p.rowid LIMIT 128")?;
    let mut rows = query.query([active.object_cursor])?;
    while let Some(row) = rows.next()? {
        active.object_cursor = row.get(0)?;
        objects.push(VerifyObject {
            path: ArchivePath::parse(&row.get::<_, String>(1)?)?,
            digest: Digest::parse(&row.get::<_, String>(2)?)?,
            size: row.get(3)?,
        });
    }
    if objects.is_empty() {
        active.objects_done = true;
        active.phase = Phase::End;
        return Ok(NasCommand::ReadJournalEnd { session });
    }
    active.phase = Phase::Objects;
    let archive = if config.archive_id.is_some() {
        config.archive_id
    } else {
        main.db
            .query_row("SELECT id FROM archives LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .map(|id| serde_json::from_str(&format!("\"{id}\"")))
            .transpose()?
    };
    Ok(NasCommand::AuditArchive {
        root: config.archive_root.clone(),
        archive: archive.ok_or_else(|| AppError::invalid("archive binding required"))?,
        writer: config.writer_id,
        objects,
    })
}
fn record_finding(
    engine: &mut ImportCoordinator,
    active: &mut Active,
    finding: IntegrityFinding,
) -> AppResult<()> {
    let report = IntegrityReport {
        checked: 0,
        findings: vec![finding.clone()],
    };
    engine.catalog.protect_integrity(&report)?;
    if active.report.findings.len() < 128 {
        active.report.findings.push(finding);
    }
    Ok(())
}
fn block_job(catalog: &mut Catalog, job: JobId, state: JobState, reason: &str) -> AppResult<()> {
    let record = catalog.job(job)?;
    catalog.transition(
        job,
        record.generation,
        JobTransition::Wait(state, reason.into()),
    )
}
fn finish_report(catalog: &mut Catalog, active: &Active) -> AppResult<()> {
    let job = active
        .job
        .ok_or_else(|| AppError::integrity("audit job missing"))?;
    catalog.db.execute("INSERT INTO job_results(job,result) VALUES(?,?) ON CONFLICT(job) DO UPDATE SET result=excluded.result",params![job.to_string(),serde_json::to_string(&active.report)?])?;
    if active.report.findings.is_empty() {
        let record = catalog.job(job)?;
        catalog.transition(job, record.generation, JobTransition::Complete)?;
    } else {
        block_job(
            catalog,
            job,
            JobState::Conflict,
            "read-only audit found missing, corrupt or unknown evidence; no archive bytes were changed",
        )?;
    }
    Ok(())
}
