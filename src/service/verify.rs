use crate::{
    catalog::*,
    config::Config,
    domain::*,
    error::{AppError, AppResult},
    transfer::*,
};
use rusqlite::params;
#[derive(Default)]
pub(crate) struct LocalAudit {
    active: Option<Active>,
}
struct Active {
    job: JobId,
    phase: usize,
    cursor: String,
    ticket: Option<WorkerTicket>,
    report: IntegrityReport,
    targets: Vec<VerificationTarget>,
}
impl LocalAudit {
    pub fn active_job(&self) -> Option<JobId> {
        self.active
            .as_ref()
            .and_then(|active| active.ticket.map(|_| active.job))
    }
    pub fn cancel(&mut self, worker: &mut WorkerSupervisor, job: JobId) -> AppResult<()> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        if active.job != job {
            self.active = Some(active);
            return Ok(());
        }
        if let Some(ticket) = active.ticket.take()
            && matches!(
                worker.poll(ticket)?,
                WorkerState::Active | WorkerState::TimedOutStillAlive | WorkerState::Reaped
            )
        {
            active.ticket = Some(ticket);
            self.active = Some(active);
        }
        Ok(())
    }
    pub fn advance(
        &mut self,
        catalog: &mut Catalog,
        config: &Config,
        worker: &mut WorkerSupervisor,
        job: JobId,
    ) -> AppResult<()> {
        let mut active = self.active.take().unwrap_or(Active {
            job,
            phase: 0,
            cursor: String::new(),
            ticket: None,
            report: IntegrityReport::default(),
            targets: Vec::new(),
        });
        if active.job != job {
            self.active = Some(active);
            return Ok(());
        }
        if let Some(ticket) = active.ticket.take() {
            match worker.poll(ticket)? {
                WorkerState::Active | WorkerState::TimedOutStillAlive | WorkerState::Reaped => {
                    active.ticket = Some(ticket);
                    self.active = Some(active);
                    return Ok(());
                }
                WorkerState::Completed(NasReply::Integrity(mut report)) => {
                    if active.phase == 0 {
                        let root = crate::fs::RootHandle::open(
                            &config.working_root,
                            crate::fs::RootPolicy::Local,
                        )?;
                        let mut verified = Vec::new();
                        for (path, stamp) in std::mem::take(&mut report.verified) {
                            let current = root
                                .open_relative(&path, libc::O_RDONLY | libc::O_NONBLOCK, 0)
                                .and_then(|file| {
                                    Ok(crate::edit::SnapshotStamp::of(&file.metadata()?))
                                });
                            if current.is_ok_and(|current| current == stamp) {
                                verified.push((path, stamp));
                            } else {
                                report.findings.push(IntegrityFinding {
                                    path,
                                    status: IntegrityStatus::Unknown,
                                    reason: "original changed after verification".into(),
                                });
                            }
                        }
                        report.verified = verified;
                    }
                    catalog.protect_integrity_scoped(
                        if active.phase == 1 { "state" } else { "local" },
                        &report,
                    )?;
                    if active.phase == 0 {
                        for (path, _) in &report.verified {
                            let target = active
                                .targets
                                .iter()
                                .find(|target| target.path == *path)
                                .ok_or_else(|| {
                                    AppError::integrity("verified original left its audit batch")
                                })?;
                            let VersionBody::Content { digest, size } = &target.body else {
                                return Err(AppError::integrity(
                                    "verified original has no content identity",
                                ));
                            };
                            catalog.db.execute("UPDATE placements SET local_ready=1 WHERE path=? AND digest=? AND EXISTS(SELECT 1 FROM local_residency_expectations e WHERE e.placement=placements.id)",params![path.as_str(),digest.as_str()])?;
                            catalog.db.execute("DELETE FROM physical_findings WHERE path=? AND digest=? AND size=?",params![path.as_str(),digest.as_str(),size])?;
                            catalog.db.execute(
                                "DELETE FROM startup_checks WHERE path=? AND digest=? AND size=?",
                                params![path.as_str(), digest.as_str(), size],
                            )?;
                            let groups = catalog.db.prepare("SELECT DISTINCT group_id FROM placements WHERE path=? AND digest=?")?
                                .query_map(params![path.as_str(),digest.as_str()],|row|row.get::<_,String>(0))?
                                .collect::<Result<Vec<_>,_>>()?;
                            for group in groups {
                                catalog
                                    .refresh_protection(serde_json::from_value(group.into())?)?;
                            }
                        }
                    }
                    active.report.checked += report.checked;
                    let remaining = 128usize.saturating_sub(active.report.findings.len());
                    active
                        .report
                        .findings
                        .extend(report.findings.into_iter().take(remaining));
                }
                WorkerState::Failed(e) => return Err(e),
                _ => return Err(AppError::integrity("unexpected local audit receipt")),
            }
        }
        while active.phase < 3 {
            let scope = ["originals", "history", "local_projections"][active.phase];
            let batch = catalog.verification_batch(scope, &active.cursor)?;
            if batch.is_empty() {
                active.phase += 1;
                active.cursor.clear();
                continue;
            }
            if worker.available()? {
                active.cursor = batch.last().unwrap().0.clone();
                active.targets = batch.into_iter().map(|(_, target)| target).collect();
                active.ticket = Some(worker.submit(NasCommand::AuditTargets {
                    root: if active.phase == 1 {
                        config.state_root.clone()
                    } else {
                        config.working_root.clone()
                    },
                    archive: None,
                    writer: config.writer_id,
                    targets: active.targets.clone(),
                })?);
            }
            self.active = Some(active);
            return Ok(());
        }
        catalog.db.execute("INSERT INTO job_results(job,result) VALUES(?,?) ON CONFLICT(job) DO UPDATE SET result=excluded.result",params![job.to_string(),serde_json::to_string(&active.report)?])?;
        let record = catalog.job(job)?;
        catalog.transition(
            job,
            record.generation,
            if active.report.findings.is_empty() {
                JobTransition::Complete
            } else {
                JobTransition::Wait(
                    JobState::Conflict,
                    "integrity findings require review; no repairs performed".into(),
                )
            },
        )?;
        Ok(())
    }
}
