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
    #[serde(default)]
    metadata_live: bool,
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
            if let Some(attempt) = active.attempt {
                catalog.update_reservation(attempt.reservation, 0)?;
            }
            catalog
                .db
                .execute("DELETE FROM pin_attempts WHERE job=?", [job.to_string()])?;
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
                    } else if active.target.metadata_live {
                        catalog.db.execute("INSERT INTO pin_live_failures(job,digest) VALUES(?,?) ON CONFLICT DO NOTHING",params![job.to_string(),active.target.member.digest.as_str()])?;
                        catalog.db.execute(
                            "UPDATE residency SET metadata_current=0,protected=1 WHERE group_id=?",
                            [group.to_string()],
                        )?;
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
            if catalog.job(job)?.generation != attempt.spec.generation {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "obsolete pin attempt generation; owned partial retained",
                ));
            }
            readmit_attempt(catalog, config, &attempt)?;
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
        let metadata_only = matches!(
            catalog.job(job)?.request.kind,
            JobKind::MetadataFetch { .. }
        );
        for member in current.members.into_iter().filter(|_| !metadata_only) {
            if !checked(catalog, job, &member, current.membership_revision)? {
                target = Some(Target {
                    group,
                    revision: current.membership_revision,
                    member,
                    metadata: false,
                    metadata_live: false,
                });
                break;
            }
        }
        if target.is_none() {
            let bodies = catalog.pin_metadata_bodies(job, group, current.membership_revision)?;
            for body in bodies {
                if let VersionBody::Content { digest, size } = body {
                    let mut member = OriginalMember {
                        path: crate::edit::snapshot_path(&digest)?,
                        digest,
                        size,
                        role: MediaRole::OtherStill,
                    };
                    if !checked(catalog, job, &member, current.membership_revision)? {
                        let live: Option<String> = if !metadata_only {
                            catalog.db.query_row("SELECT s.path FROM sidecars s JOIN local_heads l ON l.sidecar=s.id JOIN heads h ON h.sidecar=s.id AND h.version=l.version JOIN versions v ON v.id=h.version JOIN projections p ON p.sidecar=s.id AND p.pending=0 JOIN history_receipts r ON r.digest=json_extract(v.body,'$.Content.digest') AND r.archive=? WHERE s.group_id=? AND json_extract(v.body,'$.Content.digest')=? AND NOT EXISTS(SELECT 1 FROM pin_live_failures f WHERE f.job=? AND f.digest=r.digest) ORDER BY s.id LIMIT 1",params![bound_archive(config)?.to_string(),group.to_string(),member.digest.as_str(),job.to_string()],|r|r.get(0)).optional()?
                        } else {
                            None
                        };
                        let metadata_live = live.is_some();
                        if let Some(path) = live {
                            member.path = ArchivePath::parse(&path)?;
                        }
                        target = Some(Target {
                            group,
                            revision: current.membership_revision,
                            member,
                            metadata: true,
                            metadata_live,
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
    if target.metadata && !target.metadata_live {
        &config.state_root
    } else {
        &config.working_root
    }
}
fn pin_group(catalog: &Catalog, job: JobId) -> AppResult<GroupId> {
    match catalog.job(job)?.request.kind {
        JobKind::Pin { group } | JobKind::MetadataFetch { group, .. } => {
            catalog.canonical_group(group)
        }
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
    if target.metadata && !target.metadata_live {
        catalog.db.execute("INSERT INTO snapshots(digest,path,size,archived) VALUES(?,?,?,1) ON CONFLICT(digest) DO UPDATE SET archived=1",params![target.member.digest.as_str(),target.member.path.as_str(),target.member.size])?;
    } else if !target.metadata {
        catalog.record_materialization(target.group, &target.member, true)?;
    }
    catalog.db.execute(
        "INSERT INTO pin_checks(job,path,digest,revision) VALUES(?,?,?,?) ON CONFLICT DO NOTHING",
        params![
            job.to_string(),
            if target.metadata_live {
                crate::edit::snapshot_path(&target.member.digest)?
            } else {
                target.member.path.clone()
            }
            .as_str(),
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
    let decision = admit_with_budget_reservations(
        &AdmissionInput {
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
        },
        catalog.total_remaining_reservations()?,
    );
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
fn partial_size(attempt: &Attempt) -> AppResult<u64> {
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
    if partial > attempt.spec.expected_size {
        return Err(AppError::integrity("pin partial exceeds object size"));
    }
    Ok(partial)
}
fn reconcile_reservation(catalog: &mut Catalog, attempt: &Attempt) -> AppResult<()> {
    catalog.update_reservation(
        attempt.reservation,
        attempt.spec.expected_size - partial_size(attempt)?,
    )
}
fn readmit_attempt(catalog: &mut Catalog, config: &Config, attempt: &Attempt) -> AppResult<()> {
    let local = RootHandle::open(&config.working_root, RootPolicy::Local)?;
    let state = RootHandle::open(&config.state_root, RootPolicy::Local)?;
    let root = RootHandle::open(&attempt.spec.destination_root, RootPolicy::Local)?;
    let own: u64 = catalog.db.query_row(
        "SELECT remaining FROM reservations WHERE id=?",
        [attempt.reservation.to_string()],
        |r| r.get(0),
    )?;
    let reserved = catalog
        .remaining_reservations(FilesystemId(root.device()))?
        .checked_sub(own)
        .ok_or_else(|| AppError::integrity("pin reservation filesystem mismatch"))?;
    let occupied = occupied_bytes(&local)?
        .checked_add(state_occupied_bytes(&state)?)
        .ok_or_else(|| AppError::invalid("occupancy overflow"))?;
    let decision = admit_with_budget_reservations(
        &AdmissionInput {
            budget: config.budget,
            occupied,
            reserved,
            available: available_bytes(&root)?,
            reserve: config.reserve,
            required: attempt.spec.expected_size,
            existing: 0,
            partial: partial_size(attempt)?,
            overhead: 0,
            nas_available: false,
            allow_direct: false,
        },
        catalog
            .total_remaining_reservations()?
            .checked_sub(own)
            .ok_or_else(|| AppError::integrity("budget reservation mismatch"))?,
    );
    if let AdmissionDecision::Local { additional_bytes } = decision {
        catalog.update_reservation(attempt.reservation, additional_bytes)
    } else {
        catalog.update_reservation(attempt.reservation, 0)?;
        Err(AppError::new(
            ErrorCode::Capacity,
            "pin retry waits for current capacity; partial bytes and pin retained",
        ))
    }
}
impl Catalog {
    pub(crate) fn pin_metadata_bodies(
        &self,
        job: JobId,
        group: GroupId,
        revision: MembershipRevision,
    ) -> AppResult<Vec<VersionBody>> {
        let (metadata_only, versions) = match self.job(job)?.request.kind {
            JobKind::MetadataFetch {
                group: requested,
                versions,
            } => {
                if requested != group || versions.len() > 3 || versions.is_empty() {
                    return Err(AppError::invalid(
                        "metadata merge fetch requires one to three exact versions of its group",
                    ));
                }
                for id in &versions {
                    let sidecar = self.version(*id)?.sidecar;
                    let owner: Option<String> = self.db.query_row(
                        "SELECT group_id FROM sidecars WHERE id=?",
                        [sidecar.to_string()],
                        |r| r.get(0),
                    )?;
                    if owner.as_deref() != Some(&group.to_string()) {
                        return Err(AppError::integrity(
                            "metadata fetch crosses group ownership",
                        ));
                    }
                }
                (true, versions)
            }
            _ => (false, vec![]),
        };
        let mut query=self.db.prepare("SELECT DISTINCT v.body FROM sidecars s JOIN versions v ON v.sidecar=s.id WHERE s.group_id=? AND (v.id IN(SELECT value FROM json_each(?)) OR (?=0 AND EXISTS(SELECT 1 FROM heads h WHERE h.version=v.id AND h.sidecar=s.id))) AND json_extract(v.body,'$.Content.digest') IS NOT NULL AND NOT EXISTS(SELECT 1 FROM pin_checks p WHERE p.job=? AND p.digest=json_extract(v.body,'$.Content.digest') AND p.revision=?) ORDER BY v.id LIMIT 128")?;
        query
            .query_map(
                params![
                    group.to_string(),
                    serde_json::to_string(&versions)?,
                    metadata_only,
                    job.to_string(),
                    revision.0
                ],
                |r| r.get::<_, String>(0),
            )?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn failed_pin_attempt_readmits_partial_against_current_capacity() {
        let temporary = tempfile::tempdir().unwrap();
        let local = temporary.path().join("local");
        let state = temporary.path().join("state");
        let archive = temporary.path().join("archive");
        for path in [&local, &state, &archive] {
            std::fs::create_dir(path).unwrap();
        }
        let mut config = Config::new(local.clone(), state.clone(), archive);
        config.reserve = 0;
        let state_root = RootHandle::open(&state, RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&state_root).unwrap();
        let group = catalog.create_group(false).unwrap();
        let job = catalog.request_pin(group, OperationId::new()).unwrap();
        let target = Target {
            group,
            revision: catalog.group(group).unwrap().membership_revision,
            member: OriginalMember {
                path: ArchivePath::parse("RAW/frame.orf").unwrap(),
                digest: Digest::of(&[1; 100]),
                size: 100,
                role: MediaRole::Raw,
            },
            metadata: false,
            metadata_live: false,
        };
        let attempt = prepare_attempt(&mut catalog, &config, job, &target)
            .unwrap()
            .unwrap();
        catalog.update_reservation(attempt.reservation, 0).unwrap();
        let local_root = RootHandle::open(&local, RootPolicy::Local).unwrap();
        local_root
            .mkdir_all(
                &ArchivePath::parse(
                    attempt
                        .spec
                        .staged_path
                        .as_str()
                        .rsplit_once('/')
                        .unwrap()
                        .0,
                )
                .unwrap(),
            )
            .unwrap();
        local_root
            .write_new(&attempt.spec.staged_path, &[1; 20])
            .unwrap();
        let occupied =
            occupied_bytes(&local_root).unwrap() + state_occupied_bytes(&state_root).unwrap();
        config.budget = occupied + 50;
        assert!(readmit_attempt(&mut catalog, &config, &attempt).is_err());
        config.budget = occupied + 80;
        readmit_attempt(&mut catalog, &config, &attempt).unwrap();
        assert_eq!(
            catalog
                .remaining_reservations(FilesystemId(local_root.device()))
                .unwrap(),
            80
        );
    }
}
