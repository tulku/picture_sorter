use crate::{
    archive::history_path,
    catalog::*,
    config::Config,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
    storage::*,
    transfer::*,
};
use rusqlite::{OptionalExtension, params};
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Target {
    group: GroupId,
    revision: MembershipRevision,
    member: OriginalMember,
    metadata: bool,
}
struct Active {
    job: JobId,
    target: Target,
    ticket: WorkerTicket,
    phase: Phase,
    attempt: Option<Attempt>,
    deferred: Option<TransferReceipt>,
}
enum Phase {
    Check,
    Fetch,
    Finish,
}
#[derive(Clone)]
struct Attempt {
    spec: TransferSpec,
    reservation: ReservationId,
}
#[derive(Default)]
pub(crate) struct PinDriver {
    active: Option<Active>,
    verified: Option<JobId>,
}
impl PinDriver {
    pub fn verified(&self, job: JobId) -> bool {
        self.verified == Some(job)
    }
    pub fn nas_active_job(&self) -> Option<JobId> {
        self.active
            .as_ref()
            .filter(|a| matches!(a.phase, Phase::Fetch) && a.deferred.is_none())
            .map(|a| a.job)
    }
    pub fn active_job(&self) -> Option<JobId> {
        self.active.as_ref().map(|a| a.job)
    }
    pub fn cancel(
        &mut self,
        catalog: &mut Catalog,
        nas: &mut WorkerSupervisor,
        local: &mut WorkerSupervisor,
        job: JobId,
    ) -> AppResult<()> {
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        if active.job != job {
            self.active = Some(active);
            return Ok(());
        }
        if active.deferred.is_some() {
            return Ok(());
        }
        let worker = if matches!(active.phase, Phase::Fetch) {
            nas
        } else {
            local
        };
        match worker.poll(active.ticket)? {
            WorkerState::Active | WorkerState::TimedOutStillAlive | WorkerState::Reaped => {
                self.active = Some(active)
            }
            WorkerState::Completed(_) | WorkerState::Failed(_) => {
                if let Some(attempt) = active.attempt {
                    catalog.update_reservation(attempt.reservation, 0)?;
                }
            }
        }
        Ok(())
    }
    pub fn advance(
        &mut self,
        catalog: &mut Catalog,
        config: &Config,
        nas: &mut WorkerSupervisor,
        local: &mut WorkerSupervisor,
        job: JobId,
        nas_allowed: bool,
    ) -> AppResult<Readiness> {
        self.verified = None;
        let group = pin_group(catalog, job)?;
        if let Some(mut active) = self.active.take() {
            if active.job != job {
                self.active = Some(active);
                return catalog.readiness(group);
            }
            if let Some(receipt) = active.deferred.take() {
                if !local.available()? {
                    active.deferred = Some(receipt);
                    self.active = Some(active);
                    return catalog.readiness(group);
                }
                active.ticket = local.submit(NasCommand::PublishLocal {
                    root: target_root(config, &active.target).to_owned(),
                    receipt,
                    destination: active.target.member.path.clone(),
                })?;
                active.phase = Phase::Finish;
                self.active = Some(active);
                return catalog.readiness(group);
            }
            if let Some(attempt) = &active.attempt {
                reconcile_reservation(catalog, attempt)?;
            }
            let result = if matches!(active.phase, Phase::Fetch) {
                nas.poll(active.ticket)?
            } else {
                local.poll(active.ticket)?
            };
            match result {
                WorkerState::Active | WorkerState::TimedOutStillAlive | WorkerState::Reaped => {
                    self.active = Some(active);
                    return catalog.readiness(group);
                }
                WorkerState::Failed(error) => {
                    if let Some(attempt) = active.attempt {
                        catalog.update_reservation(attempt.reservation, 0)?;
                    }
                    return Err(error);
                }
                WorkerState::Completed(NasReply::Integrity(report))
                    if matches!(active.phase, Phase::Check) =>
                {
                    if report.findings.is_empty() {
                        acknowledge(catalog, job, &active.target)?;
                    } else if report
                        .findings
                        .iter()
                        .all(|f| f.status == IntegrityStatus::Missing)
                    {
                        if !active.target.metadata {
                            catalog.db.execute(
                                "UPDATE placements SET local_ready=0 WHERE group_id=? AND path=?",
                                params![group.to_string(), active.target.member.path.as_str()],
                            )?;
                        }
                        if let Some(attempt) =
                            prepare_attempt(catalog, config, job, &active.target)?
                        {
                            if !nas_allowed || !nas.available()? {
                                return catalog.readiness(group);
                            }
                            active.ticket = nas.submit(NasCommand::FetchArchive {
                                archive: bound_archive(config)?,
                                writer: config.writer_id,
                                spec: attempt.spec.clone(),
                            })?;
                            active.phase = Phase::Fetch;
                            active.attempt = Some(attempt);
                            self.active = Some(active);
                            return catalog.readiness(group);
                        }
                    } else {
                        catalog.db.execute(
                            "UPDATE residency SET protected=1 WHERE group_id=?",
                            [group.to_string()],
                        )?;
                        return Err(AppError::new(
                            ErrorCode::Conflict,
                            "local bytes differ from pinned identity; preserved without replacement",
                        ));
                    }
                }
                WorkerState::Completed(NasReply::Staged(receipt))
                    if matches!(active.phase, Phase::Fetch) =>
                {
                    let attempt = active
                        .attempt
                        .as_ref()
                        .ok_or_else(|| AppError::integrity("pin receipt has no attempt"))?;
                    if !receipt.matches(job, attempt.spec.generation)
                        || receipt.digest != attempt.spec.expected_digest
                        || receipt.size != attempt.spec.expected_size
                        || receipt.staged_path != attempt.spec.staged_path
                        || catalog.job(job)?.generation != attempt.spec.generation
                    {
                        return Err(AppError::integrity("pin receipt identity mismatch"));
                    }
                    active.deferred = Some(receipt);
                    self.active = Some(active);
                    return catalog.readiness(group);
                }
                WorkerState::Completed(NasReply::Staged(_))
                    if matches!(active.phase, Phase::Finish) =>
                {
                    acknowledge(catalog, job, &active.target)?;
                    let attempt = active
                        .attempt
                        .ok_or_else(|| AppError::integrity("pin publication has no attempt"))?;
                    catalog.update_reservation(attempt.reservation, 0)?;
                    catalog
                        .db
                        .execute("DELETE FROM pin_attempts WHERE job=?", [job.to_string()])?;
                }
                _ => return Err(AppError::integrity("unexpected pin worker response")),
            }
        }
        if !local.available()? {
            return catalog.readiness(group);
        }
        let pending: Option<(String, String, String)> = catalog
            .db
            .query_row(
                "SELECT spec,reservation,target FROM pin_attempts WHERE job=?",
                [job.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((spec, reservation, target)) = pending {
            if !nas_allowed || !nas.available()? {
                return catalog.readiness(group);
            }
            let attempt = Attempt {
                spec: serde_json::from_str(&spec)?,
                reservation: serde_json::from_str(&format!("\"{reservation}\""))?,
            };
            let target: Target = serde_json::from_str(&target)?;
            reconcile_reservation(catalog, &attempt)?;
            let ticket = nas.submit(NasCommand::FetchArchive {
                archive: bound_archive(config)?,
                writer: config.writer_id,
                spec: attempt.spec.clone(),
            })?;
            self.active = Some(Active {
                job,
                target,
                ticket,
                phase: Phase::Fetch,
                attempt: Some(attempt),
                deferred: None,
            });
            return catalog.readiness(group);
        }
        let current = catalog.group(group)?;
        let mut target = None;
        for member in current.members {
            if !checked(catalog, job, &member, current.membership_revision)? {
                target = Some(Target {
                    group,
                    revision: current.membership_revision,
                    member,
                    metadata: false,
                });
                break;
            }
        }
        if target.is_none() {
            let mut query=catalog.db.prepare("SELECT v.body FROM sidecars s JOIN heads h ON h.sidecar=s.id JOIN versions v ON v.id=h.version WHERE s.group_id=? ORDER BY s.id LIMIT 128")?;
            let bodies = query
                .query_map([group.to_string()], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(query);
            for body in bodies {
                if let VersionBody::Content { digest, size } = serde_json::from_str(&body)? {
                    let member = OriginalMember {
                        path: crate::edit::snapshot_path(&digest)?,
                        digest,
                        size,
                        role: MediaRole::OtherStill,
                    };
                    if !checked(catalog, job, &member, current.membership_revision)? {
                        target = Some(Target {
                            group,
                            revision: current.membership_revision,
                            member,
                            metadata: true,
                        });
                        break;
                    }
                }
            }
        }
        if target.is_none() {
            self.verified = Some(job);
        }
        if let Some(target) = target {
            let ticket = local.submit(NasCommand::AuditLocal {
                root: target_root(config, &target).to_owned(),
                objects: vec![VerifyObject {
                    path: target.member.path.clone(),
                    digest: target.member.digest.clone(),
                    size: target.member.size,
                }],
            })?;
            self.active = Some(Active {
                job,
                target,
                ticket,
                phase: Phase::Check,
                attempt: None,
                deferred: None,
            });
        }
        catalog.readiness(group)
    }
}
fn bound_archive(config: &Config) -> AppResult<ArchiveId> {
    config
        .archive_id
        .ok_or_else(|| AppError::new(ErrorCode::Unavailable, "archive is unbound"))
}
fn target_root<'a>(config: &'a Config, target: &Target) -> &'a std::path::Path {
    if target.metadata {
        &config.state_root
    } else {
        &config.working_root
    }
}
fn pin_group(catalog: &Catalog, job: JobId) -> AppResult<GroupId> {
    match catalog.job(job)?.request.kind {
        JobKind::Pin { group } => catalog.canonical_group(group),
        JobKind::PinUrl { .. } => {
            let group: String = catalog.db.query_row(
                "SELECT group_id FROM pin_targets WHERE job=?",
                [job.to_string()],
                |r| r.get(0),
            )?;
            catalog.canonical_group(serde_json::from_str(&format!("\"{group}\""))?)
        }
        _ => Err(AppError::invalid("pin driver requires pin job")),
    }
}
fn checked(
    catalog: &Catalog,
    job: JobId,
    member: &OriginalMember,
    revision: MembershipRevision,
) -> AppResult<bool> {
    Ok(catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM pin_checks WHERE job=? AND path=? AND digest=? AND revision=?)",params![job.to_string(),member.path.as_str(),member.digest.as_str(),revision.0],|r|r.get(0))?)
}
fn acknowledge(catalog: &mut Catalog, job: JobId, target: &Target) -> AppResult<()> {
    if catalog.group(target.group)?.membership_revision != target.revision {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "pin membership changed; checked bytes retained",
        ));
    }
    if target.metadata {
        catalog.db.execute("INSERT INTO snapshots(digest,path,size,archived) VALUES(?,?,?,1) ON CONFLICT(digest) DO UPDATE SET archived=1",params![target.member.digest.as_str(),target.member.path.as_str(),target.member.size])?;
    } else {
        catalog.record_materialization(target.group, &target.member, true)?;
    }
    catalog.db.execute(
        "INSERT INTO pin_checks(job,path,digest,revision) VALUES(?,?,?,?) ON CONFLICT DO NOTHING",
        params![
            job.to_string(),
            target.member.path.as_str(),
            target.member.digest.as_str(),
            target.revision.0
        ],
    )?;
    Ok(())
}
fn prepare_attempt(
    catalog: &mut Catalog,
    config: &Config,
    job: JobId,
    target: &Target,
) -> AppResult<Option<Attempt>> {
    let local = RootHandle::open(&config.working_root, RootPolicy::Local)?;
    let state = RootHandle::open(&config.state_root, RootPolicy::Local)?;
    let root = if target.metadata { &state } else { &local };
    let occupied = occupied_bytes(&local)?
        .checked_add(state_occupied_bytes(&state)?)
        .ok_or_else(|| AppError::invalid("occupancy overflow"))?;
    let decision = admit(&AdmissionInput {
        budget: config.budget,
        occupied,
        reserved: catalog.remaining_reservations(FilesystemId(root.device()))?,
        available: available_bytes(root)?,
        reserve: config.reserve,
        required: target.member.size,
        existing: 0,
        partial: 0,
        overhead: 0,
        nas_available: false,
        allow_direct: false,
    });
    let AdmissionDecision::Local { additional_bytes } = decision else {
        return Err(AppError::new(
            ErrorCode::Capacity,
            "pin is retained but missing bytes do not fit",
        ));
    };
    let record = catalog.job(job)?;
    catalog.transition(job, record.generation, JobTransition::Start)?;
    let generation = catalog.job(job)?.generation;
    let spec = TransferSpec {
        job,
        generation,
        source: SourceLocation {
            root: config.archive_root.clone(),
            path: if target.metadata {
                history_path(&target.member.digest)?
            } else {
                target.member.path.clone()
            },
        },
        destination_root: target_root(config, target).to_owned(),
        staged_path: staging_path(job, generation),
        expected_digest: target.member.digest.clone(),
        expected_size: target.member.size,
    };
    let reservation = catalog.reserve(job, FilesystemId(root.device()), additional_bytes)?;
    catalog.db.execute(
        "INSERT INTO pin_attempts(job,spec,reservation,target) VALUES(?,?,?,?)",
        params![
            job.to_string(),
            serde_json::to_string(&spec)?,
            reservation.to_string(),
            serde_json::to_string(target)?
        ],
    )?;
    Ok(Some(Attempt { spec, reservation }))
}
fn reconcile_reservation(catalog: &mut Catalog, attempt: &Attempt) -> AppResult<()> {
    let root = RootHandle::open(&attempt.spec.destination_root, RootPolicy::Local)?;
    let partial = match root.open_relative(
        &attempt.spec.staged_path,
        libc::O_RDONLY | libc::O_NONBLOCK,
        0,
    ) {
        Ok(f) => {
            let m = f.metadata()?;
            if !m.is_file() {
                return Err(AppError::integrity("pin partial is not regular"));
            }
            m.len()
        }
        Err(e) if e.errno == Some(libc::ENOENT) => 0,
        Err(e) => return Err(e),
    };
    let remaining = attempt
        .spec
        .expected_size
        .checked_sub(partial)
        .ok_or_else(|| AppError::integrity("pin partial exceeds object size"))?;
    catalog.update_reservation(attempt.reservation, remaining)
}
