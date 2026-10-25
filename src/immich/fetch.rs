use crate::{
    catalog::*,
    config::Config,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
    transfer::*,
};
use rusqlite::OptionalExtension;
#[derive(Default)]
pub struct PinFetchDriver {
    active: Option<Fetch>,
}
struct Fetch {
    job: JobId,
    group: GroupId,
    member: OriginalMember,
    ticket: WorkerTicket,
    generation: u64,
    metadata: Option<(SidecarId, VersionId)>,
}
impl PinFetchDriver {
    pub fn cancel(&mut self, worker: &mut WorkerSupervisor, job: JobId) -> AppResult<()> {
        let Some(active) = self.active.take() else {
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
            WorkerState::Completed(_) | WorkerState::Failed(_) => {}
        }
        Ok(())
    }

    pub fn active_job(&self) -> Option<JobId> {
        self.active.as_ref().map(|active| active.job)
    }

    pub fn advance(
        &mut self,
        catalog: &mut Catalog,
        config: &Config,
        worker: &mut WorkerSupervisor,
        job: JobId,
    ) -> AppResult<crate::storage::Readiness> {
        let record = catalog.job(job)?;
        let group = match record.request.kind {
            JobKind::Pin { group } => catalog.canonical_group(group)?,
            JobKind::PinUrl { .. } => {
                let group: Option<String> = catalog
                    .db
                    .query_row(
                        "SELECT group_id FROM pin_targets WHERE job=?",
                        [job.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?;
                let group = group.ok_or_else(|| {
                    AppError::new(ErrorCode::Unavailable, "URL mapping remains pending")
                })?;
                catalog.canonical_group(serde_json::from_str(&format!("\"{group}\""))?)?
            }
            _ => return Err(AppError::invalid("job is not a pin")),
        };
        let local = RootHandle::open(&config.working_root, RootPolicy::Local)?;
        if let Some(active) = self.active.take() {
            if active.job != job {
                self.active = Some(active);
                return catalog.readiness(group);
            }
            match worker.poll(active.ticket)? {
                WorkerState::Active | WorkerState::TimedOutStillAlive => {
                    self.active = Some(active);
                    return catalog.readiness(group);
                }
                WorkerState::Failed(e) => return Err(e),
                WorkerState::Reaped => {
                    return Err(AppError::new(
                        ErrorCode::Unavailable,
                        "fetch worker reaped; reconcile retained stage",
                    ));
                }
                WorkerState::Completed(NasReply::Staged(receipt)) => {
                    if !receipt.matches(job, active.generation)
                        || catalog.job(job)?.generation != active.generation
                        || catalog.job(job)?.cancel_requested
                    {
                        return Err(AppError::new(
                            ErrorCode::Conflict,
                            "obsolete fetch receipt; staged bytes retained",
                        ));
                    }
                    if let Some((sidecar, version)) = active.metadata {
                        let version = catalog.version(version)?;
                        if version.sidecar != sidecar
                            || version.body
                                != (VersionBody::Content {
                                    digest: active.member.digest.clone(),
                                    size: active.member.size,
                                })
                        {
                            return Err(AppError::integrity("metadata fetch identity changed"));
                        }
                        let state = RootHandle::open(&config.state_root, RootPolicy::Local)?;
                        state.mkdir_all(&ArchivePath::parse(".photo-sorter/snapshots")?)?;
                        if publish_noreplace(&state, &receipt.staged_path, &active.member.path)?
                            == PublishOutcome::Exists
                        {
                            let mut old =
                                state.open_relative(&active.member.path, libc::O_RDONLY, 0)?;
                            if digest_file(&mut old)?
                                != (active.member.digest.clone(), active.member.size)
                            {
                                return Err(AppError::integrity(
                                    "existing local metadata snapshot is corrupt",
                                ));
                            }
                        }
                        catalog.db.execute("INSERT INTO snapshots(digest,path,size,archived) VALUES(?,?,?,1) ON CONFLICT(digest) DO UPDATE SET archived=1",rusqlite::params![active.member.digest.as_str(),active.member.path.as_str(),active.member.size])?;
                    } else {
                        let current = catalog.group(active.group)?;
                        if !current.members.contains(&active.member) {
                            return Err(AppError::new(
                                ErrorCode::Conflict,
                                "membership changed; replan fetch",
                            ));
                        }
                        if let Some((parent, _)) = active.member.path.as_str().rsplit_once('/') {
                            local.mkdir_all(&ArchivePath::parse(parent)?)?;
                        }
                        if publish_noreplace(&local, &receipt.staged_path, &active.member.path)?
                            == PublishOutcome::Exists
                        {
                            let mut old =
                                local.open_relative(&active.member.path, libc::O_RDONLY, 0)?;
                            if digest_file(&mut old)?
                                != (active.member.digest.clone(), active.member.size)
                            {
                                return Err(AppError::new(
                                    ErrorCode::Conflict,
                                    "local original differs; neither file overwritten",
                                ));
                            }
                        }
                        catalog.record_materialization(current.id, &active.member, true)?;
                    }
                }
                _ => return Err(AppError::integrity("unexpected pin fetch response")),
            }
        }
        let current = catalog.group(group)?;
        for member in &current.members {
            let present =
                match local.open_relative(&member.path, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
                    Ok(mut file) => {
                        if digest_file(&mut file)? != (member.digest.clone(), member.size) {
                            return Err(AppError::new(
                                ErrorCode::Conflict,
                                "changed local original retained",
                            ));
                        }
                        true
                    }
                    Err(e) if e.errno == Some(libc::ENOENT) => false,
                    Err(e) => return Err(e),
                };
            if present {
                catalog.record_materialization(group, member, false)?;
                continue;
            }
            let occupied = occupied_bytes(&local)?;
            let decision = crate::storage::admit(&crate::storage::AdmissionInput {
                budget: config.budget,
                occupied,
                reserved: catalog
                    .remaining_reservations(crate::storage::FilesystemId(local.device()))?,
                available: available_bytes(&local)?,
                reserve: config.reserve,
                required: member.size,
                existing: 0,
                partial: 0,
                overhead: 0,
                nas_available: false,
                allow_direct: false,
            });
            if !matches!(decision, crate::storage::AdmissionDecision::Local { .. }) {
                return Err(AppError::new(
                    ErrorCode::Capacity,
                    "pin retained but missing originals do not fit",
                ));
            }
            if !worker.available()? {
                return catalog.readiness(group);
            }
            let record = catalog.job(job)?;
            if record.state != JobState::Running {
                catalog.transition(job, record.generation, JobTransition::Start)?;
            }
            let generation = catalog.job(job)?.generation;
            let archive = config.archive_id.ok_or_else(|| {
                AppError::new(ErrorCode::Unavailable, "profile is not bound to archive")
            })?;
            let spec = TransferSpec {
                job,
                generation,
                source: SourceLocation {
                    root: config.archive_root.clone(),
                    path: member.path.clone(),
                },
                destination_root: config.working_root.clone(),
                staged_path: staging_path(job, generation),
                expected_digest: member.digest.clone(),
                expected_size: member.size,
            };
            let ticket = worker.submit(NasCommand::FetchArchive {
                archive,
                writer: config.writer_id,
                spec,
            })?;
            self.active = Some(Fetch {
                job,
                group,
                member: member.clone(),
                ticket,
                generation,
                metadata: None,
            });
            return catalog.readiness(group);
        }
        let state = RootHandle::open(&config.state_root, RootPolicy::Local)?;
        let mut query=catalog.db.prepare("SELECT s.id,v.id,v.body FROM sidecars s JOIN heads h ON h.sidecar=s.id JOIN versions v ON v.id=h.version WHERE s.group_id=? ORDER BY s.id LIMIT 128")?;
        let versions = query
            .query_map([group.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(query);
        for (sidecar, version, body) in versions {
            let VersionBody::Content { digest, size } = serde_json::from_str(&body)? else {
                continue;
            };
            let target = ArchivePath::parse(&format!(
                ".photo-sorter/snapshots/{}",
                &digest.as_str()[7..]
            ))?;
            match state.open_relative(&target, libc::O_RDONLY, 0) {
                Ok(mut file) => {
                    if digest_file(&mut file)? != (digest.clone(), size) {
                        return Err(AppError::integrity("local metadata snapshot changed"));
                    }
                    continue;
                }
                Err(e) if e.errno == Some(libc::ENOENT) => {}
                Err(e) => return Err(e),
            }
            if !worker.available()? {
                return catalog.readiness(group);
            }
            let occupied = occupied_bytes(&local)?
                .checked_add(occupied_bytes(&state)?)
                .ok_or_else(|| AppError::invalid("occupancy overflow"))?;
            if !matches!(
                crate::storage::admit(&crate::storage::AdmissionInput {
                    budget: config.budget,
                    occupied,
                    reserved: catalog
                        .remaining_reservations(crate::storage::FilesystemId(state.device()))?,
                    available: available_bytes(&state)?,
                    reserve: config.reserve,
                    required: size,
                    existing: 0,
                    partial: 0,
                    overhead: 0,
                    nas_available: false,
                    allow_direct: false
                }),
                crate::storage::AdmissionDecision::Local { .. }
            ) {
                return Err(AppError::new(
                    ErrorCode::Capacity,
                    "pinned metadata snapshot does not fit; intent retained",
                ));
            }
            let generation = catalog.job(job)?.generation;
            let spec = TransferSpec {
                job,
                generation,
                source: SourceLocation {
                    root: config.archive_root.clone(),
                    path: crate::archive::history_path(&digest)?,
                },
                destination_root: config.state_root.clone(),
                staged_path: staging_path(job, generation),
                expected_digest: digest.clone(),
                expected_size: size,
            };
            let ticket = worker.submit(NasCommand::FetchArchive {
                archive: config
                    .archive_id
                    .ok_or_else(|| AppError::new(ErrorCode::Unavailable, "archive unbound"))?,
                writer: config.writer_id,
                spec,
            })?;
            self.active = Some(Fetch {
                job,
                group,
                member: OriginalMember {
                    digest,
                    size,
                    path: target,
                    role: MediaRole::OtherStill,
                },
                ticket,
                generation,
                metadata: Some((
                    serde_json::from_str(&format!("\"{sidecar}\""))?,
                    serde_json::from_str(&format!("\"{version}\""))?,
                )),
            });
            return catalog.readiness(group);
        }
        let clock = SystemClock::default();
        crate::edit::EditTracker::new(&local, &state, catalog, &clock)
            .reconcile(crate::edit::ReconcileScope::All)?;
        let readiness = catalog.readiness(group)?;
        let record = catalog.job(job)?;
        if record.state != JobState::Complete {
            catalog.transition(
                job,
                record.generation,
                if readiness.metadata_current {
                    JobTransition::Complete
                } else {
                    JobTransition::Wait(
                        JobState::WaitingApplication,
                        "originals ready; metadata requires foreground refresh".into(),
                    )
                },
            )?;
        }
        Ok(readiness)
    }
}
