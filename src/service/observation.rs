use crate::{
    catalog::*,
    domain::*,
    edit::*,
    error::{AppError, AppResult, ErrorCode},
    fs::RootHandle,
    import::{ImportCoordinator, SourceRole},
    transfer::*,
};
use rusqlite::{OptionalExtension, params};
use std::collections::VecDeque;
#[derive(Default, Debug, serde::Serialize)]
pub(crate) struct ObservationStatus {
    pub captured: u64,
    pub resets: u64,
    pub invalid: u64,
    pub unreadable: u64,
    pub startup_complete: bool,
    pub reason: Option<String>,
}
pub(crate) struct ObservationDriver {
    pub worker: WorkerSupervisor,
    active: Option<(WorkerTicket, Phase)>,
    watch_ready: bool,
    discover: bool,
    discover_started: bool,
    known: bool,
    after: String,
    queue: VecDeque<SidecarOwner>,
    events_due: bool,
    pub status: ObservationStatus,
    pub blocked_group: Option<GroupId>,
}
struct CaptureResult {
    owner: SidecarOwner,
    previous: Option<VersionId>,
    operation: OperationId,
    receipt: SnapshotReceipt,
}
enum Phase {
    Watch,
    Start,
    List,
    Capture {
        context: CaptureContext,
        recovering: bool,
    },
    Events,
}
impl ObservationDriver {
    pub fn new(worker: WorkerSupervisor) -> Self {
        Self {
            worker,
            active: None,
            watch_ready: false,
            discover: true,
            discover_started: false,
            known: true,
            after: String::new(),
            queue: VecDeque::new(),
            events_due: false,
            status: ObservationStatus::default(),
            blocked_group: None,
        }
    }
    pub fn advance(
        &mut self,
        engine: &mut ImportCoordinator,
        local: &RootHandle,
        state: &RootHandle,
        clock: &dyn Clock,
        observe: bool,
        reconcile: bool,
    ) -> AppResult<()> {
        if reconcile {
            self.discover = true;
            self.known = true;
            self.after.clear();
        }
        if observe {
            self.events_due = true;
            self.enqueue_absences(&engine.catalog)?;
            self.enqueue_generated(engine, local, clock)?;
            associate_generated_companions(&mut engine.catalog)?;
        }
        if let Some((ticket, phase)) = self.active.take() {
            if let Phase::Capture { context, .. } = &phase {
                reconcile_capture_reservation(&mut engine.catalog, state, context)?;
            }
            let reply = match self.worker.poll(ticket)? {
                WorkerState::Active | WorkerState::TimedOutStillAlive => {
                    self.active = Some((ticket, phase));
                    return Ok(());
                }
                WorkerState::Failed(error) => {
                    if let Phase::Capture { context, .. } = &phase
                        && context.owner.kind == SidecarKind::Generated
                        && (error.code == ErrorCode::SourceChanged
                            || error.errno == Some(libc::ENOENT)
                            || (error.code == ErrorCode::Unavailable
                                && matches!(local.metadata_relative(&context.owner.path), Err(error) if error.errno == Some(libc::ENOENT))))
                    {
                        fail_capture(&mut engine.catalog, context, &error.to_string())?;
                        engine.catalog.db.execute(
                            "UPDATE capture_requests SET done=1 WHERE operation=?",
                            [context.operation.to_string()],
                        )?;
                        context.cleanup(state)?;
                        engine.catalog.db.execute(
                            "UPDATE generated_observations SET stable_since=? WHERE path=?",
                            params![clock.now(), context.owner.path.as_str()],
                        )?;
                        self.known = true;
                        self.after.clear();
                        return Ok(());
                    }
                    self.status.unreadable += 1;
                    self.status.reason = Some(error.to_string());
                    if let Phase::Capture {
                        context,
                        recovering,
                    } = &phase
                    {
                        self.unreadable(&mut engine.catalog, &context.owner, &error.to_string())?;
                        if *recovering {
                            fail_capture(&mut engine.catalog, context, &error.to_string())?;
                            engine.catalog.db.execute(
                                "UPDATE capture_requests SET done=1 WHERE operation=?",
                                [context.operation.to_string()],
                            )?;
                        }
                    }
                    self.discover_started = false;
                    return Ok(());
                }
                WorkerState::Reaped => {
                    self.active = Some((ticket, phase));
                    self.status.reason = Some(
                        "observation worker disappeared; retained snapshots require recovery"
                            .into(),
                    );
                    return Ok(());
                }
                WorkerState::Completed(reply) => reply,
            };
            match (phase, reply) {
                (Phase::Watch, NasReply::Done) => self.watch_ready = true,
                (Phase::Start, NasReply::Done) => self.discover_started = true,
                (Phase::List, NasReply::LocalFiles { entries, end, .. }) => {
                    for entry in &entries {
                        if matches!(entry.role, SourceRole::Original(_)) {
                            self.observe_media(engine, local, clock, &entry.path)?;
                        }
                    }
                    for entry in entries {
                        if matches!(entry.role, SourceRole::Companion(_)) {
                            ensure_generated_base(engine, local, &entry.path)?;
                            let owner = discover(&mut engine.catalog, &entry.path)?;
                            self.queue.push_back(owner);
                        }
                    }
                    if end {
                        self.discover = false;
                        self.discover_started = false;
                    }
                }
                (Phase::Capture { context, .. }, NasReply::Captured(receipt)) => {
                    self.record(
                        engine,
                        local,
                        state,
                        clock,
                        CaptureResult {
                            owner: context.owner.clone(),
                            previous: context.previous,
                            operation: context.operation,
                            receipt,
                        },
                    )?;
                    engine.catalog.db.execute(
                        "UPDATE capture_requests SET done=1 WHERE operation=?",
                        [context.operation.to_string()],
                    )?;
                    finish_capture(&mut engine.catalog, &context)?;
                    context.cleanup(state)?;
                }
                (Phase::Events, NasReply::LocalEvents(events)) => {
                    for event in events {
                        match event {
                            LocalEvent::RescanRequired => {
                                self.discover = true;
                                self.known = true;
                                self.after.clear();
                            }
                            LocalEvent::Changed(path) => {
                                if is_generated_output_path(&path)
                                    || matches!(
                                        crate::import::source_role(path.as_str()),
                                        Some(SourceRole::Original(_))
                                    )
                                {
                                    self.observe_media(engine, local, clock, &path)?;
                                }
                                if matches!(
                                    crate::import::source_role(path.as_str()),
                                    Some(SourceRole::Companion(_))
                                ) {
                                    ensure_generated_base(engine, local, &path)?;
                                    let owner = discover(&mut engine.catalog, &path)?;
                                    if !self.queue.iter().any(|queued| queued.id == owner.id) {
                                        if let Some(group) = owner.group {
                                            engine.catalog.db.execute("UPDATE residency SET last_activity=? WHERE group_id=?",params![clock.now(),group.to_string()])?;
                                        }
                                        if self.queue.len() < 128 {
                                            self.queue.push_back(owner);
                                        } else {
                                            self.known = true;
                                            self.after.clear();
                                        }
                                    }
                                }
                                engine.catalog.db.execute("UPDATE residency SET last_activity=? WHERE group_id IN(SELECT group_id FROM members WHERE path=?)",params![clock.now(),path.as_str()])?;
                            }
                        }
                    }
                }
                _ => return Err(AppError::integrity("unexpected observation worker reply")),
            }
        }
        if !self.worker.available()? {
            return Ok(());
        }
        let recovery: Option<String> = engine
            .catalog
            .db
            .query_row(
                "SELECT context FROM capture_requests WHERE done=0 AND direct=0 ORDER BY rowid LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let (command, phase) = if let Some(context) = recovery {
            let context: CaptureContext = serde_json::from_str(&context)?;
            (
                NasCommand::RecoverCapture {
                    state: engine.config.state_root.clone(),
                    context: context.clone(),
                    ceiling: engine.config.json_ceiling,
                },
                Phase::Capture {
                    context,
                    recovering: true,
                },
            )
        } else if !self.watch_ready {
            (
                NasCommand::WatchLocal {
                    root: engine.config.working_root.clone(),
                },
                Phase::Watch,
            )
        } else if let Some(owner) =
            next_unblocked(&mut self.queue, &engine.catalog, self.blocked_group)?
        {
            let pending:bool=engine.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM capture_requests WHERE done=0 AND json_extract(context,'$.owner.id')=?)",[owner.id.to_string()],|r|r.get(0))?;
            if pending {
                return Ok(());
            }
            if owner.kind == SidecarKind::Generated
                && !generated_ready(
                    &mut engine.catalog,
                    local,
                    &owner.path,
                    clock.now(),
                    engine.config.debounce_seconds,
                )?
            {
                return Ok(());
            }
            let size = match local.metadata_relative(&owner.path) {
                Ok(metadata) => metadata.stx_size,
                Err(e) if e.errno == Some(libc::ENOENT) => {
                    let observed: bool = engine.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM local_heads WHERE sidecar=?) OR EXISTS(SELECT 1 FROM observations WHERE path=? AND (digest IS NOT NULL OR size IS NOT NULL OR absent_since IS NOT NULL))",params![owner.id.to_string(),owner.path.as_str()],|row|row.get(0))?;
                    if !observed {
                        return Ok(());
                    }
                    0
                }
                Err(e) => {
                    self.unreadable(&mut engine.catalog, &owner, &e.to_string())?;
                    return Ok(());
                }
            };
            let measurement = match engine.catalog.admission_measurement(local, state) {
                Ok(measurement) => measurement,
                Err(error) => {
                    self.queue.push_front(owner);
                    return Err(error);
                }
            };
            let occupied = measurement
                .local
                .total
                .checked_add(measurement.state.total)
                .ok_or_else(|| AppError::invalid("occupancy overflow"))?;
            let admitted = crate::storage::admit_with_budget_reservations(
                &crate::storage::AdmissionInput {
                    budget: engine.config.budget,
                    occupied,
                    reserved: engine
                        .catalog
                        .remaining_reservations(crate::storage::FilesystemId(state.device()))?,
                    available: measurement.state_filesystem_free,
                    reserve: engine.config.reserve,
                    required: size,
                    existing: 0,
                    partial: 0,
                    overhead: 16 * 1024,
                    nas_available: false,
                    allow_direct: false,
                },
                engine.catalog.total_remaining_reservations()?,
            );
            let direct =
                size > 0 && !matches!(admitted, crate::storage::AdmissionDecision::Local { .. });
            let previous = engine.catalog.local_head(owner.id)?;
            let context = CaptureContext {
                operation: OperationId::new(),
                owner,
                previous,
            };
            let job = engine.catalog.enqueue(DurableRequest {
                operation_id: context.operation,
                kind: JobKind::SnapshotCapture {
                    context: serde_json::to_value(&context)?,
                    direct,
                },
            })?;
            let reservation = if !direct {
                Some(engine.catalog.reserve(
                    job,
                    crate::storage::FilesystemId(state.device()),
                    size,
                )?)
            } else {
                None
            };
            engine.catalog.db.execute("INSERT INTO capture_requests(operation,context,direct,job,reservation,reserved_size) VALUES(?,?,?,?,?,?)",params![context.operation.to_string(),serde_json::to_string(&context)?,direct,job.to_string(),reservation.map(|id|id.to_string()),size])?;
            context.persist(state)?;
            if direct {
                self.status.reason=Some("saved bytes remain live and protected; direct archive history capture awaits qualified NAS worker".into());
                if let Some(group) = context.owner.group {
                    engine.catalog.db.execute(
                        "UPDATE residency SET protected=1 WHERE group_id=?",
                        [group.to_string()],
                    )?;
                }
                return Ok(());
            }
            engine.catalog.transition(job, 0, JobTransition::Start)?;
            (
                NasCommand::CaptureOwned {
                    local: engine.config.working_root.clone(),
                    state: engine.config.state_root.clone(),
                    context: context.clone(),
                    ceiling: engine.config.json_ceiling,
                },
                Phase::Capture {
                    context,
                    recovering: false,
                },
            )
        } else if self.discover {
            if self.discover_started {
                (NasCommand::LocalInventoryNext { limit: 128 }, Phase::List)
            } else {
                (
                    NasCommand::LocalInventoryStart {
                        root: engine.config.working_root.clone(),
                    },
                    Phase::Start,
                )
            }
        } else if self.known {
            let mut query = engine.catalog.db.prepare(
                "SELECT id,group_id,path,owner FROM sidecars WHERE id>? ORDER BY id LIMIT 128",
            )?;
            let rows = query
                .query_map([&self.after], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(query);
            for (id, group, path, owner) in rows {
                self.after = id.clone();
                self.queue
                    .push_back(owner_from_row(id, group, path, owner)?);
            }
            if self.queue.is_empty() {
                self.known = false;
                self.status.startup_complete = true;
                return Ok(());
            }
            return Ok(());
        } else if self.events_due {
            self.events_due = false;
            (NasCommand::LocalEvents, Phase::Events)
        } else {
            return Ok(());
        };
        let ticket = self.worker.submit(command)?;
        self.active = Some((ticket, phase));
        Ok(())
    }
    fn observe_media(
        &mut self,
        engine: &mut ImportCoordinator,
        local: &RootHandle,
        clock: &dyn Clock,
        path: &ArchivePath,
    ) -> AppResult<()> {
        let present = match local.metadata_relative(path) {
            Ok(_) => true,
            Err(error) if error.errno == Some(libc::ENOENT) => false,
            Err(error) => return Err(error),
        };
        if original_claimed(&engine.catalog, path)? {
            engine.catalog.queue_observed_original(path, present)?;
            return Ok(());
        }
        let owner = match engine.catalog.generated_owner(path)? {
            Some(owner) => Some(owner),
            None if present && is_generated_output_path(path) => {
                Some(engine.catalog.register_generated_output(path)?)
            }
            None => None,
        };
        if let Some(owner) = owner
            && generated_ready(
                &mut engine.catalog,
                local,
                path,
                clock.now(),
                engine.config.debounce_seconds,
            )?
            && !self.queue.iter().any(|queued| queued.id == owner.id)
        {
            self.queue.push_back(owner);
        }
        Ok(())
    }
    fn enqueue_generated(
        &mut self,
        engine: &mut ImportCoordinator,
        local: &RootHandle,
        clock: &dyn Clock,
    ) -> AppResult<()> {
        let mut query = engine.catalog.db.prepare("SELECT path FROM generated_observations WHERE present=1 AND (captured_stamp IS NULL OR stamp<>captured_stamp) ORDER BY stable_since LIMIT 128")?;
        let paths = query
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(query);
        for path in paths {
            self.observe_media(engine, local, clock, &ArchivePath::parse(&path)?)?;
        }
        Ok(())
    }
    pub(crate) fn record_archived(
        &mut self,
        engine: &mut ImportCoordinator,
        local: &RootHandle,
        state: &RootHandle,
        context: CaptureContext,
        receipt: SnapshotReceipt,
    ) -> AppResult<()> {
        if !receipt.archive_history {
            return Err(AppError::integrity(
                "direct capture returned a local receipt",
            ));
        }
        if receipt.source_stamp.is_none() {
            self.known = true;
            self.after.clear();
        }
        self.record(
            engine,
            local,
            state,
            &SystemClock::default(),
            CaptureResult {
                owner: context.owner.clone(),
                previous: context.previous,
                operation: context.operation,
                receipt,
            },
        )?;
        finish_capture(&mut engine.catalog, &context)?;
        context.cleanup(state)
    }
    fn enqueue_absences(&mut self, catalog: &Catalog) -> AppResult<()> {
        if self.queue.len() >= 128 {
            return Ok(());
        }
        let mut query=catalog.db.prepare("SELECT s.id,s.group_id,s.path,s.owner FROM sidecars s JOIN observations o ON o.path=s.path WHERE o.absent_since IS NOT NULL LIMIT 128")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let owner = owner_from_row(row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)?;
            if !self.queue.iter().any(|o| o.id == owner.id) {
                self.queue.push_back(owner);
            }
        }
        Ok(())
    }
    fn unreadable(
        &self,
        catalog: &mut Catalog,
        owner: &SidecarOwner,
        reason: &str,
    ) -> AppResult<()> {
        catalog.db.execute("INSERT INTO observations(path,readable) VALUES(?,0) ON CONFLICT(path) DO UPDATE SET readable=0,absent_since=NULL",[owner.path.as_str()])?;
        catalog.db.execute("INSERT INTO sidecar_diagnostics(sidecar,reason) VALUES(?,?) ON CONFLICT(sidecar) DO UPDATE SET reason=excluded.reason",params![owner.id.to_string(),reason])?;
        if let Some(group) = owner.group {
            catalog.db.execute(
                "UPDATE residency SET protected=1 WHERE group_id=?",
                [group.to_string()],
            )?;
        }
        Ok(())
    }
    fn record(
        &mut self,
        engine: &mut ImportCoordinator,
        local: &RootHandle,
        state: &RootHandle,
        clock: &dyn Clock,
        capture: CaptureResult,
    ) -> AppResult<()> {
        let CaptureResult {
            owner,
            previous,
            operation,
            receipt,
        } = capture;
        if receipt.path != owner.path {
            return Err(AppError::integrity("sidecar observation pathname mismatch"));
        }
        let mut changed = false;
        let current = engine.catalog.local_head(owner.id)?;
        if receipt.body == VersionBody::Reset {
            if owner.kind == SidecarKind::Generated {
                engine.catalog.db.execute(
                    "UPDATE generated_observations SET present=0 WHERE path=?",
                    [owner.path.as_str()],
                )?;
                return Ok(());
            }
            if owner.kind == SidecarKind::JpegXmpExternal || current.is_none() {
                return Ok(());
            }
            let current = current.unwrap();
            if engine.catalog.version(current)?.body == VersionBody::Reset {
                engine.catalog.db.execute(
                    "UPDATE observations SET absent_since=NULL WHERE path=?",
                    [owner.path.as_str()],
                )?;
                return Ok(());
            }
            if previous != Some(current) || !receipt.still_matches(local)? {
                return Ok(());
            }
            let since: Option<u64> = engine
                .catalog
                .db
                .query_row(
                    "SELECT absent_since FROM observations WHERE path=?",
                    [owner.path.as_str()],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let Some(since) = since else {
                engine.catalog.db.execute("INSERT INTO observations(path,absent_since,readable) VALUES(?,?,1) ON CONFLICT(path) DO UPDATE SET absent_since=excluded.absent_since,readable=1",params![owner.path.as_str(),clock.now()])?;
                return Ok(());
            };
            if clock.now().saturating_sub(since) < engine.config.debounce_seconds {
                return Ok(());
            }
            VersionStore::new(&mut engine.catalog, state).record(SnapshotIntent {
                sidecar: owner.id,
                parents: vec![current],
                operation,
                payload: SnapshotPayload::Reset,
                origin: VersionOrigin::Observed,
            })?;
            self.status.resets += 1;
            changed = true;
        } else {
            let still_current = previous == current && receipt.still_matches(local)?;
            let stored = if receipt.archive_history {
                None
            } else {
                Some(StoredSnapshot::from_receipt(receipt.clone(), state)?)
            };
            if receipt.archive_history {
                let VersionBody::Content { digest, size } = &receipt.body else {
                    return Err(AppError::integrity("archive snapshot body missing"));
                };
                engine.catalog.db.execute("INSERT INTO history_receipts(archive,digest,size) VALUES(?,?,?) ON CONFLICT DO NOTHING",params![engine.config.archive_id.ok_or_else(||AppError::invalid("archive snapshot without archive identity"))?.to_string(),digest.as_str(),size])?;
            }
            if let Some(current) = current
                && engine.catalog.version(current)?.body == receipt.body
            {
                if let Some(stored) = stored {
                    stored.register(state, &mut engine.catalog)?;
                }
            } else {
                let intent = SnapshotIntent {
                    sidecar: owner.id,
                    parents: previous.into_iter().collect(),
                    operation,
                    payload: if let Some(stored) = stored {
                        SnapshotPayload::Stored(stored)
                    } else {
                        SnapshotPayload::Archived(ArchivedSnapshot::from_receipt(&receipt)?)
                    },
                    origin: VersionOrigin::Observed,
                };
                if still_current {
                    VersionStore::new(&mut engine.catalog, state).record(intent)?;
                } else {
                    VersionStore::new(&mut engine.catalog, state).record_detached(intent)?;
                }
                self.status.captured += 1;
                changed = true;
            }
            if receipt.opaque_invalid {
                self.status.invalid += 1;
                engine.catalog.db.execute("INSERT INTO sidecar_diagnostics(sidecar,reason) VALUES(?,'malformed or oversized editor JSON retained as opaque bytes') ON CONFLICT(sidecar) DO UPDATE SET reason=excluded.reason",[owner.id.to_string()])?;
            } else {
                engine.catalog.db.execute(
                    "DELETE FROM sidecar_diagnostics WHERE sidecar=?",
                    [owner.id.to_string()],
                )?;
            }
        }
        let (digest, size) = match &receipt.body {
            VersionBody::Content { digest, size } => (Some(digest.as_str()), Some(*size)),
            VersionBody::Reset => (None, None),
        };
        engine.catalog.db.execute("INSERT INTO observations(path,digest,size,readable) VALUES(?,?,?,1) ON CONFLICT(path) DO UPDATE SET digest=excluded.digest,size=excluded.size,readable=1,absent_since=NULL",params![owner.path.as_str(),digest,size])?;
        if let Some(group) = owner.group
            && (changed || receipt.opaque_invalid)
        {
            engine.catalog.refresh_protection(group)?;
            engine.catalog.db.execute(
                "UPDATE residency SET protected=1,last_activity=? WHERE group_id=?",
                params![clock.now(), group.to_string()],
            )?;
        }
        if owner.kind == SidecarKind::Generated
            && receipt.still_matches(local)?
            && let Some(stamp) = &receipt.source_stamp
        {
            engine.catalog.db.execute("UPDATE generated_observations SET captured_stamp=?,present=1 WHERE path=? AND stamp=?", params![serde_json::to_string(stamp)?,owner.path.as_str(),serde_json::to_string(stamp)?])?;
        }
        self.status.reason = None;
        Ok(())
    }
}
fn owner_from_row(
    id: String,
    group: Option<String>,
    path: String,
    owner: String,
) -> AppResult<SidecarOwner> {
    Ok(SidecarOwner {
        id: serde_json::from_str(&format!("\"{id}\""))?,
        group: group
            .map(|s| serde_json::from_str(&format!("\"{s}\"")))
            .transpose()?,
        path: ArchivePath::parse(&path)?,
        kind: match owner.as_str() {
            "rapidraw-base" | "rapidraw-virtual" => SidecarKind::RapidRaw,
            "rrexif" => SidecarKind::Rrexif,
            "raw-xmp" => SidecarKind::RawXmp,
            "jpeg-xmp-external" => SidecarKind::JpegXmpExternal,
            GENERATED_OUTPUT_OWNER => SidecarKind::Generated,
            _ => SidecarKind::Opaque,
        },
        virtual_copy: None,
    })
}
fn discover(catalog: &mut Catalog, path: &ArchivePath) -> AppResult<SidecarOwner> {
    let existing: Option<(String, Option<String>, String)> = catalog
        .db
        .query_row(
            "SELECT id,group_id,owner FROM sidecars WHERE path=? ORDER BY id LIMIT 1",
            [path.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((id, group, owner)) = existing {
        return owner_from_row(id, group, path.as_str().into(), owner);
    }
    if let Some(owner) = generated_companion_owner(catalog, path)? {
        catalog.register_sidecar(owner.id, owner.group, path.clone(), owner_name(&owner))?;
        return Ok(owner);
    }
    let mut query=catalog.db.prepare("SELECT m.path,m.digest,b.size,m.role,m.group_id FROM members m JOIN blobs b ON b.digest=m.digest WHERE substr(?,1,length(m.path))=m.path ORDER BY length(m.path) DESC LIMIT 128")?;
    let rows = query
        .query_map([path.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(query);
    let members = rows
        .iter()
        .map(|(path, digest, size, role, _)| {
            Ok(OriginalMember {
                path: ArchivePath::parse(path)?,
                digest: Digest::parse(digest)?,
                size: *size,
                role: serde_json::from_str(role)?,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    let (group, kind, virtual_copy, owner) = match resolve_owner(path, &members) {
        OwnerDecision::Owned {
            member,
            kind,
            virtual_copy,
        } => {
            let group = rows
                .iter()
                .find(|r| r.0 == member.as_str())
                .map(|r| serde_json::from_str(&format!("\"{}\"", r.4)))
                .transpose()?;
            let owner = match kind {
                SidecarKind::RapidRaw => {
                    if virtual_copy.is_some() {
                        "rapidraw-virtual"
                    } else {
                        "rapidraw-base"
                    }
                }
                SidecarKind::Rrexif => "rrexif",
                SidecarKind::RawXmp => "raw-xmp",
                SidecarKind::JpegXmpExternal => "jpeg-xmp-external",
                SidecarKind::Opaque => "opaque",
                SidecarKind::Generated => GENERATED_OUTPUT_OWNER,
            };
            (group, kind, virtual_copy, owner)
        }
        OwnerDecision::Unassociated => (None, SidecarKind::Opaque, None, "unassociated"),
    };
    let id = catalog.managed_sidecar_id(path)?;
    catalog.register_sidecar(id, group, path.clone(), owner)?;
    if group.is_none() {
        catalog.record_ambiguity(
            "orphan_sidecar",
            None,
            serde_json::json!({"sidecar":id,"path":path}),
        )?;
    }
    Ok(SidecarOwner {
        id,
        group,
        path: path.clone(),
        kind,
        virtual_copy,
    })
}

fn original_claimed(catalog: &Catalog, path: &ArchivePath) -> AppResult<bool> {
    Ok(catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM members WHERE path=? COLLATE NOCASE) OR EXISTS(SELECT 1 FROM placements WHERE path=? COLLATE NOCASE) OR EXISTS(SELECT 1 FROM import_items WHERE destination=? COLLATE NOCASE)", params![path.as_str(),path.as_str(),path.as_str()], |row| row.get(0))?)
}
fn generated_ready(
    catalog: &mut Catalog,
    local: &RootHandle,
    path: &ArchivePath,
    now: Timestamp,
    debounce: u64,
) -> AppResult<bool> {
    let metadata = match local.open_relative(path, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => file.metadata()?,
        Err(error) if error.errno == Some(libc::ENOENT) => {
            catalog.db.execute(
                "UPDATE generated_observations SET present=0 WHERE path=?",
                [path.as_str()],
            )?;
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Err(AppError::invalid("generated output must be a regular file"));
    }
    let stamp = serde_json::to_string(&SnapshotStamp::of(&metadata))?;
    catalog.db.execute("INSERT INTO generated_observations(path,stamp,stable_since,present) VALUES(?,?,?,1) ON CONFLICT(path) DO UPDATE SET stamp=excluded.stamp,stable_since=excluded.stable_since,present=1 WHERE generated_observations.stamp IS NOT excluded.stamp OR generated_observations.present<>1", params![path.as_str(),stamp,now])?;
    Ok(catalog.db.query_row("SELECT captured_stamp IS NOT stamp AND ? >= stable_since AND ? - stable_since >= ? FROM generated_observations WHERE path=?", params![now,now,debounce,path.as_str()], |row| row.get(0))?)
}
fn companion_base(path: &ArchivePath) -> Option<ArchivePath> {
    let (base, extension) = path.as_str().rsplit_once('.')?;
    if !matches!(
        extension,
        "rrdata" | "rrexif" | "xmp" | "orig" | "pp3" | "dop" | "on1"
    ) {
        return None;
    }
    let direct = ArchivePath::parse(base).ok()?;
    if is_generated_output_path(&direct) {
        return Some(direct);
    }
    if extension == "rrdata" {
        let (base, copy) = base.rsplit_once('.')?;
        if copy.len() == 6
            && copy
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            let base = ArchivePath::parse(base).ok()?;
            return is_generated_output_path(&base).then_some(base);
        }
    }
    None
}
fn ensure_generated_base(
    engine: &mut ImportCoordinator,
    local: &RootHandle,
    path: &ArchivePath,
) -> AppResult<()> {
    if let Some(base) = companion_base(path)
        && !original_claimed(&engine.catalog, &base)?
        && engine.catalog.generated_owner(&base)?.is_none()
    {
        match local.open_relative(&base, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
            Ok(file) if file.metadata()?.is_file() => {
                engine.catalog.register_generated_output(&base)?;
            }
            Ok(_) => {}
            Err(error) if error.errno == Some(libc::ENOENT) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
fn generated_companion_owner(
    catalog: &Catalog,
    path: &ArchivePath,
) -> AppResult<Option<SidecarOwner>> {
    let Some(base) = companion_base(path) else {
        return Ok(None);
    };
    let Some(base_owner) = catalog.generated_owner(&base)? else {
        return Ok(None);
    };
    let member = OriginalMember {
        path: base.clone(),
        digest: Digest::of(&[]),
        size: 0,
        role: MediaRole::for_filename(base.as_str()).unwrap_or(MediaRole::OtherStill),
    };
    let OwnerDecision::Owned {
        kind, virtual_copy, ..
    } = resolve_owner(path, &[member])
    else {
        return Ok(None);
    };
    Ok(Some(SidecarOwner {
        id: catalog.managed_sidecar_id(path)?,
        group: base_owner.group,
        path: path.clone(),
        kind,
        virtual_copy,
    }))
}
fn owner_name(owner: &SidecarOwner) -> &'static str {
    match owner.kind {
        SidecarKind::RapidRaw if owner.virtual_copy.is_some() => "rapidraw-virtual",
        SidecarKind::RapidRaw => "rapidraw-base",
        SidecarKind::Rrexif => "rrexif",
        SidecarKind::RawXmp => "raw-xmp",
        SidecarKind::JpegXmpExternal => "jpeg-xmp-external",
        SidecarKind::Opaque => "opaque",
        SidecarKind::Generated => GENERATED_OUTPUT_OWNER,
    }
}
fn associate_generated_companions(catalog: &mut Catalog) -> AppResult<()> {
    let mut query = catalog.db.prepare("SELECT c.id,c.revision,s.path,s.id FROM conflicts c JOIN sidecars s ON s.id=json_extract(c.details,'$.sidecar') WHERE c.kind='orphan_sidecar' AND c.resolved=0 AND s.group_id IS NULL AND EXISTS(SELECT 1 FROM sidecars base WHERE base.owner='generated-output' AND substr(s.path,1,length(base.path))=base.path AND (substr(s.path,length(base.path)+1) IN('.rrdata','.rrexif','.xmp','.orig','.pp3','.dop','.on1') OR substr(s.path,length(base.path)+1) GLOB '.[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f].rrdata')) AND EXISTS(SELECT 1 FROM published_sidecars p WHERE p.id=s.id) AND NOT EXISTS(SELECT 1 FROM pending_shared_records p WHERE p.committed=0 AND json_extract(p.records,'$[0].request.conflict')=c.id) ORDER BY c.id LIMIT 128")?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(query);
    for (conflict, revision, path, sidecar) in rows {
        let path = ArchivePath::parse(&path)?;
        let Some(base) = companion_base(&path) else {
            continue;
        };
        let Some(base_owner) = catalog.generated_owner(&base)? else {
            continue;
        };
        let archived: bool = catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM local_heads l JOIN archived_versions a ON a.version=l.version WHERE l.sidecar=?)", [base_owner.id.to_string()], |row| row.get(0))?;
        if !archived {
            continue;
        }
        let Some(group) = base_owner.group else {
            continue;
        };
        let sidecar: SidecarId = serde_json::from_value(sidecar.into())?;
        let request = ResolutionRequest {
            operation: OperationId::new(),
            conflict: serde_json::from_value(conflict.into())?,
            expected_revision: revision,
            expected_heads: vec![],
            expected_memberships: vec![(group, catalog.group(group)?.membership_revision)],
            resolution: ConflictResolution::AssociateSidecar { sidecar, group },
        };
        let mut records = catalog.resolution_records(&request)?;
        let projection: Option<(String,String,String,Option<String>)> = catalog.db.query_row("SELECT v.id,v.body,s.owner,p.committed FROM sidecars s JOIN local_heads l ON l.sidecar=s.id JOIN heads h ON h.sidecar=s.id AND h.version=l.version JOIN versions v ON v.id=h.version JOIN archived_versions a ON a.version=v.id LEFT JOIN projections p ON p.sidecar=s.id WHERE s.id=? AND (SELECT count(*) FROM heads h WHERE h.sidecar=s.id)=1",[sidecar.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
        if let Some((version, body, owner, previous)) = projection
            && crate::archive::owns_projection(&owner, &path)
        {
            let previous = previous
                .map(|previous| serde_json::from_str::<crate::archive::ProjectionPlan>(&previous))
                .transpose()?;
            let version: VersionId = serde_json::from_value(version.into())?;
            let body: VersionBody = serde_json::from_str(&body)?;
            if !previous
                .as_ref()
                .is_some_and(|plan| plan.version == version && plan.body == body)
            {
                records.push(crate::archive::ArchiveRecord::ProjectionPlanned {
                    plan: crate::archive::ProjectionPlan {
                        sidecar,
                        path: path.clone(),
                        version,
                        expected: previous.map(|plan| plan.body).unwrap_or(VersionBody::Reset),
                        body,
                        owner,
                    },
                });
            }
        }
        catalog.db.execute(
            "INSERT INTO pending_shared_records(operation,records) VALUES(?,?)",
            params![
                request.operation.to_string(),
                serde_json::to_string(&records)?
            ],
        )?;
    }
    Ok(())
}

fn finish_capture(catalog: &mut Catalog, context: &CaptureContext) -> AppResult<()> {
    let job: Option<String> = catalog
        .db
        .query_row(
            "SELECT job FROM capture_requests WHERE operation=?",
            [context.operation.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    catalog.db.execute(
        "UPDATE capture_requests SET done=1 WHERE operation=?",
        [context.operation.to_string()],
    )?;
    if let Some(job) = job {
        let job: JobId = serde_json::from_value(job.into())?;
        catalog
            .db
            .execute("DELETE FROM reservations WHERE job=?", [job.to_string()])?;
        let record = catalog.job(job)?;
        if !matches!(record.state, JobState::Complete | JobState::Cancelled) {
            catalog.transition(
                job,
                record.generation,
                if record.cancel_requested {
                    JobTransition::ConfirmCancelled
                } else {
                    JobTransition::Complete
                },
            )?;
        }
    }
    Ok(())
}
fn reconcile_capture_reservation(
    catalog: &mut Catalog,
    state: &RootHandle,
    context: &CaptureContext,
) -> AppResult<()> {
    let reservation: Option<String> = catalog
        .db
        .query_row(
            "SELECT reservation FROM capture_requests WHERE operation=?",
            [context.operation.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if let Some(reservation) = reservation {
        let id: ReservationId = serde_json::from_value(reservation.into())?;
        let partial = ArchivePath::parse(&format!(
            ".photo-sorter/captures/{}/payload",
            context.operation
        ))?;
        let consumed = match state.metadata_relative(&partial) {
            Ok(m) => m.stx_size,
            Err(e) if e.errno == Some(libc::ENOENT) => 0,
            Err(e) => return Err(e),
        };
        let requested: u64 = catalog.db.query_row(
            "SELECT reserved_size FROM capture_requests WHERE reservation=?",
            [id.to_string()],
            |r| r.get(0),
        )?;
        catalog.update_reservation(id, requested.saturating_sub(consumed))?;
    }
    Ok(())
}

fn fail_capture(catalog: &mut Catalog, context: &CaptureContext, reason: &str) -> AppResult<()> {
    let job: Option<String> = catalog
        .db
        .query_row(
            "SELECT job FROM capture_requests WHERE operation=?",
            [context.operation.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if let Some(job) = job {
        let job: JobId = serde_json::from_value(job.into())?;
        catalog
            .db
            .execute("DELETE FROM reservations WHERE job=?", [job.to_string()])?;
        let record = catalog.job(job)?;
        if !matches!(record.state, JobState::Complete | JobState::Cancelled) {
            catalog.transition(
                job,
                record.generation,
                if record.cancel_requested {
                    JobTransition::ConfirmCancelled
                } else {
                    JobTransition::Wait(JobState::Failed, reason.into())
                },
            )?;
        }
    }
    Ok(())
}

fn next_unblocked(
    queue: &mut VecDeque<SidecarOwner>,
    catalog: &Catalog,
    blocked: Option<GroupId>,
) -> AppResult<Option<SidecarOwner>> {
    for _ in 0..queue.len() {
        let Some(owner) = queue.pop_front() else {
            break;
        };
        let defer = if let Some(group) = blocked {
            catalog.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sidecars WHERE id=? AND group_id=?)",
                params![owner.id.to_string(), group.to_string()],
                |r| r.get::<_, bool>(0),
            )?
        } else {
            false
        };
        if defer {
            queue.push_back(owner);
        } else {
            return Ok(Some(owner));
        }
    }
    Ok(None)
}
#[cfg(test)]
mod application_serialization_tests {
    use super::*;
    use crate::fs::RootPolicy;
    #[test]
    fn application_defers_its_group_capture_without_starving_other_saved_edits() {
        let temp = tempfile::tempdir().unwrap();
        let mut catalog =
            Catalog::open(&RootHandle::open(temp.path(), RootPolicy::Local).unwrap()).unwrap();
        let groups = [
            catalog.create_group(false).unwrap(),
            catalog.create_group(false).unwrap(),
        ];
        let mut queue = VecDeque::new();
        for (index, group) in groups.into_iter().enumerate() {
            let owner = SidecarOwner {
                id: SidecarId::new(),
                group: Some(group),
                path: ArchivePath::parse(&format!("RAW/{index}.ORF.rrdata")).unwrap(),
                kind: SidecarKind::RapidRaw,
                virtual_copy: None,
            };
            catalog
                .register_sidecar(owner.id, owner.group, owner.path.clone(), "rapidraw-base")
                .unwrap();
            queue.push_back(owner);
        }
        assert_eq!(
            next_unblocked(&mut queue, &catalog, Some(groups[0]))
                .unwrap()
                .unwrap()
                .group,
            Some(groups[1])
        );
        assert!(
            next_unblocked(&mut queue, &catalog, Some(groups[0]))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            next_unblocked(&mut queue, &catalog, None)
                .unwrap()
                .unwrap()
                .group,
            Some(groups[0])
        );
    }
}

#[cfg(test)]
mod fast_startup_tests {
    use super::*;
    use crate::{config::Config, fs::RootPolicy};
    fn missing_sidecar_allocations(observed: bool) -> (u64, u64) {
        let temp = tempfile::tempdir().unwrap();
        let local_path = temp.path().join("local");
        let state_path = temp.path().join("state");
        let archive_path = temp.path().join("archive");
        for path in [&local_path, &state_path, &archive_path] {
            std::fs::create_dir(path).unwrap();
        }
        let local = RootHandle::open(&local_path, RootPolicy::Local).unwrap();
        let state = RootHandle::open(&state_path, RootPolicy::Local).unwrap();
        let catalog = Catalog::open(&state).unwrap();
        let id = SidecarId::new();
        catalog
            .db
            .execute(
                "INSERT INTO sidecars(id,path,owner) VALUES(?,'remote.ORF.rrdata','rapidraw-base')",
                [id.to_string()],
            )
            .unwrap();
        let owner = SidecarOwner {
            id,
            group: None,
            path: ArchivePath::parse("remote.ORF.rrdata").unwrap(),
            kind: SidecarKind::RapidRaw,
            virtual_copy: None,
        };
        if observed {
            catalog
                .db
                .execute(
                    "INSERT INTO observations(path,digest,size) VALUES(?,'previous',7)",
                    [owner.path.as_str()],
                )
                .unwrap();
        }
        let config = Config::new(local_path, state_path, archive_path);
        let mut engine = ImportCoordinator::new(config, catalog);
        let shell = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("bash"))
            .find(|path| path.is_file())
            .unwrap();
        let worker_path = temp.path().join("idle-worker");
        std::fs::write(
            &worker_path,
            format!("#!{}\nwhile read -r line; do :; done\n", shell.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&worker_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let worker = WorkerSupervisor::new_local(worker_path, vec![]).unwrap();
        let mut driver = ObservationDriver::new(worker);
        driver.watch_ready = true;
        driver.discover = false;
        driver.known = false;
        driver.queue.push_back(owner);
        driver
            .advance(
                &mut engine,
                &local,
                &state,
                &ManualClock::default(),
                false,
                false,
            )
            .unwrap();
        let captures: u64 = engine
            .catalog
            .db
            .query_row("SELECT count(*) FROM capture_requests", [], |r| r.get(0))
            .unwrap();
        let reservations: u64 = engine
            .catalog
            .db
            .query_row("SELECT count(*) FROM reservations", [], |r| r.get(0))
            .unwrap();
        (captures, reservations)
    }
    #[test]
    fn remote_only_missing_sidecar_does_not_allocate_capture_or_reservation() {
        assert_eq!(missing_sidecar_allocations(false), (0, 0));
    }
    #[test]
    fn previously_observed_missing_sidecar_retains_deletion_capture() {
        assert_eq!(missing_sidecar_allocations(true), (1, 1));
    }
}

#[cfg(test)]
mod generated_observation_tests {
    use super::*;
    use crate::fs::RootPolicy;

    #[test]
    fn generated_stamp_debounce_survives_reopen_and_detects_same_size_save() {
        let temp = tempfile::tempdir().unwrap();
        let local_path = temp.path().join("local");
        let state_path = temp.path().join("state");
        std::fs::create_dir(&local_path).unwrap();
        std::fs::create_dir(&state_path).unwrap();
        let local = RootHandle::open(&local_path, RootPolicy::Local).unwrap();
        let state = RootHandle::open(&state_path, RootPolicy::Local).unwrap();
        let path = ArchivePath::parse("render.tif").unwrap();
        std::fs::write(local_path.join(path.as_str()), b"save one").unwrap();
        let mut catalog = Catalog::open(&state).unwrap();
        catalog.register_generated_output(&path).unwrap();
        assert!(!generated_ready(&mut catalog, &local, &path, 100, 2).unwrap());
        assert!(!generated_ready(&mut catalog, &local, &path, 101, 2).unwrap());
        assert!(generated_ready(&mut catalog, &local, &path, 102, 2).unwrap());
        catalog
            .db
            .execute("UPDATE generated_observations SET captured_stamp=stamp", [])
            .unwrap();
        drop(catalog);
        let mut catalog = Catalog::open(&state).unwrap();
        assert!(!generated_ready(&mut catalog, &local, &path, 200, 2).unwrap());
        std::fs::write(local_path.join(path.as_str()), b"save two").unwrap();
        assert!(!generated_ready(&mut catalog, &local, &path, 201, 2).unwrap());
        assert!(generated_ready(&mut catalog, &local, &path, 203, 2).unwrap());
        std::fs::remove_file(local_path.join(path.as_str())).unwrap();
        assert!(!generated_ready(&mut catalog, &local, &path, 204, 2).unwrap());
        let present: bool = catalog
            .db
            .query_row("SELECT present FROM generated_observations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(!present);
        std::fs::write(local_path.join(path.as_str()), b"save two").unwrap();
        assert!(!generated_ready(&mut catalog, &local, &path, 205, 2).unwrap());
        assert!(generated_ready(&mut catalog, &local, &path, 207, 2).unwrap());
    }

    #[test]
    fn generated_capture_disappearance_retries_without_unreadable_diagnostic() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let local_path = temp.path().join("local");
        let state_path = temp.path().join("state");
        let archive_path = temp.path().join("archive");
        for path in [&local_path, &state_path, &archive_path] {
            std::fs::create_dir(path).unwrap();
        }
        let local = RootHandle::open(&local_path, RootPolicy::Local).unwrap();
        let state = RootHandle::open(&state_path, RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&state).unwrap();
        let path = ArchivePath::parse("render.tif").unwrap();
        std::fs::write(local_path.join(path.as_str()), b"saved render").unwrap();
        let owner = catalog.register_generated_output(&path).unwrap();
        let mut config = crate::config::Config::new(local_path.clone(), state_path, archive_path);
        config.debounce_seconds = 0;
        let mut engine = ImportCoordinator::new(config, catalog);
        let shell = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("bash"))
            .find(|path| path.is_file())
            .unwrap();
        let response = temp.path().join("response");
        let executable = temp.path().join("capture-worker");
        std::fs::write(&executable,format!("#!{}\nwhile read -r command; do while IFS= read -r reply; do printf '%s\\n' \"$reply\"; done < '{}'; done\n",shell.display(),response.display())).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut driver =
            ObservationDriver::new(WorkerSupervisor::new_local(executable, vec![]).unwrap());
        driver.watch_ready = true;
        driver.discover = false;
        driver.known = false;
        driver.queue.push_back(owner.clone());
        let clock = ManualClock::default();
        driver
            .advance(&mut engine, &local, &state, &clock, false, false)
            .unwrap();
        let ticket = driver.active.as_ref().unwrap().0;
        std::fs::write(&response,format!("{}\n",serde_json::json!({"ticket":ticket,"result":{"Err":AppError::new(ErrorCode::Unavailable,"generated output locally absent")}}))).unwrap();
        std::fs::remove_file(local_path.join(path.as_str())).unwrap();
        let deadline = std::time::Instant::now();
        while driver.active.is_some() {
            driver
                .advance(&mut engine, &local, &state, &clock, false, false)
                .unwrap();
            assert!(deadline.elapsed() < std::time::Duration::from_secs(3));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(driver.status.unreadable, 0);
        let diagnostics: u64 = engine
            .catalog
            .db
            .query_row("SELECT count(*) FROM sidecar_diagnostics", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(diagnostics, 0);
        let pending: u64 = engine
            .catalog
            .db
            .query_row(
                "SELECT count(*) FROM capture_requests WHERE done=0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 0);
    }

    #[test]
    fn superseded_generated_receipt_preserves_snapshot_without_acknowledging_new_save() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let local_path = temp.path().join("local");
        let state_path = temp.path().join("state");
        let archive_path = temp.path().join("archive");
        for path in [&local_path, &state_path, &archive_path] {
            std::fs::create_dir(path).unwrap();
        }
        let local = RootHandle::open(&local_path, RootPolicy::Local).unwrap();
        let state = RootHandle::open(&state_path, RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&state).unwrap();
        let path = ArchivePath::parse("render.tif").unwrap();
        let owner = catalog.register_generated_output(&path).unwrap();
        let config = crate::config::Config::new(local_path.clone(), state_path, archive_path);
        let mut engine = ImportCoordinator::new(config, catalog);
        std::fs::write(local_path.join(path.as_str()), b"captured previous save").unwrap();
        assert!(generated_ready(&mut engine.catalog, &local, &path, 0, 0).unwrap());
        let context = CaptureContext {
            operation: OperationId::new(),
            owner: owner.clone(),
            previous: None,
        };
        let receipt = crate::edit::capture_owned(&local, &state, &context, 32).unwrap();
        std::fs::write(local_path.join(path.as_str()), b"new saved bytes").unwrap();
        assert!(generated_ready(&mut engine.catalog, &local, &path, 0, 0).unwrap());
        let shell = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("bash"))
            .find(|path| path.is_file())
            .unwrap();
        let executable = temp.path().join("idle-worker");
        std::fs::write(
            &executable,
            format!("#!{}\nwhile read -r line; do :; done\n", shell.display()),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut driver =
            ObservationDriver::new(WorkerSupervisor::new_local(executable, vec![]).unwrap());
        driver
            .record(
                &mut engine,
                &local,
                &state,
                &ManualClock::default(),
                CaptureResult {
                    owner: owner.clone(),
                    previous: None,
                    operation: context.operation,
                    receipt,
                },
            )
            .unwrap();
        assert_eq!(engine.catalog.local_head(owner.id).unwrap(), None);
        let acknowledged: Option<String> = engine
            .catalog
            .db
            .query_row(
                "SELECT captured_stamp FROM generated_observations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(acknowledged, None);
        let snapshot = snapshot_path(&Digest::of(b"captured previous save")).unwrap();
        assert_eq!(
            state.read(&snapshot, 32).unwrap(),
            b"captured previous save"
        );
        let context = CaptureContext {
            operation: OperationId::new(),
            owner: owner.clone(),
            previous: None,
        };
        let receipt = crate::edit::capture_owned(&local, &state, &context, 32).unwrap();
        driver
            .record(
                &mut engine,
                &local,
                &state,
                &ManualClock::default(),
                CaptureResult {
                    owner: owner.clone(),
                    previous: None,
                    operation: context.operation,
                    receipt,
                },
            )
            .unwrap();
        assert!(!generated_ready(&mut engine.catalog, &local, &path, 0, 0).unwrap());
        let current = engine
            .catalog
            .version(engine.catalog.local_head(owner.id).unwrap().unwrap())
            .unwrap();
        assert_eq!(
            current.body,
            VersionBody::Content {
                digest: Digest::of(b"new saved bytes"),
                size: 15
            }
        );
    }

    #[test]
    fn archived_orphan_metadata_is_associated_through_native_resolution() {
        use crate::archive::{ArchiveRecord, CommitEnvelope};
        let temp = tempfile::tempdir().unwrap();
        let state = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&state).unwrap();
        let archive = ArchiveId::new();
        let writer = WriterId::new();
        let sidecar = SidecarId::new();
        let conflict = ConflictId::new();
        let path = ArchivePath::parse("HDR/render.tif.rrdata").unwrap();
        let first = CommitEnvelope::new(
            archive,
            writer,
            1,
            vec![],
            vec![
                ArchiveRecord::SidecarCreated {
                    id: sidecar,
                    group: None,
                    path: path.clone(),
                    owner: "preserved-source-companion".into(),
                },
                ArchiveRecord::AmbiguityRecorded {
                    id: conflict,
                    kind: "orphan_sidecar".into(),
                    group: None,
                    details: serde_json::json!({"sidecar":sidecar,"path":path}),
                },
            ],
        )
        .unwrap();
        catalog.apply_commit(&first).unwrap();
        for index in 0..129 {
            let orphan = SidecarId::new();
            let id = format!("00000000-0000-0000-0000-{index:012}");
            let path = format!("unrelated-{index}.rrdata");
            catalog
                .db
                .execute(
                    "INSERT INTO sidecars(id,path,owner) VALUES(?,?,'unassociated')",
                    params![orphan.to_string(), path],
                )
                .unwrap();
            catalog
                .db
                .execute(
                    "INSERT INTO published_sidecars(id) VALUES(?)",
                    [orphan.to_string()],
                )
                .unwrap();
            catalog
                .db
                .execute(
                    "INSERT INTO conflicts(id,kind,details) VALUES(?,'orphan_sidecar',?)",
                    params![
                        id,
                        serde_json::json!({"sidecar":orphan,"path":path}).to_string()
                    ],
                )
                .unwrap();
        }
        let generated = catalog
            .register_generated_output(&ArchivePath::parse("HDR/render.tif").unwrap())
            .unwrap();
        associate_generated_companions(&mut catalog).unwrap();
        let pending: u64 = catalog
            .db
            .query_row("SELECT count(*) FROM pending_shared_records", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(pending, 0);
        let version = Version {
            id: VersionId::new(),
            sidecar: generated.id,
            parents: vec![],
            body: VersionBody::Content {
                digest: Digest::of(b"HDR"),
                size: 3,
            },
            origin: VersionOrigin::Observed,
        };
        let second = CommitEnvelope::new(
            archive,
            writer,
            2,
            vec![first.commit_id],
            vec![
                ArchiveRecord::GroupCreated {
                    id: generated.group.unwrap(),
                },
                ArchiveRecord::SidecarCreated {
                    id: generated.id,
                    group: generated.group,
                    path: generated.path.clone(),
                    owner: GENERATED_OUTPUT_OWNER.into(),
                },
                ArchiveRecord::VersionAdded {
                    version: version.clone(),
                    operation: OperationId::new(),
                },
                ArchiveRecord::HeadsChanged {
                    sidecar: generated.id,
                    expected: vec![],
                    heads: vec![version.id],
                },
            ],
        )
        .unwrap();
        catalog.apply_commit(&second).unwrap();
        catalog
            .db
            .execute(
                "INSERT INTO local_heads(sidecar,version) VALUES(?,?)",
                params![generated.id.to_string(), version.id.to_string()],
            )
            .unwrap();
        associate_generated_companions(&mut catalog).unwrap();
        associate_generated_companions(&mut catalog).unwrap();
        let records: String = catalog
            .db
            .query_row("SELECT records FROM pending_shared_records", [], |row| {
                row.get(0)
            })
            .unwrap();
        let records: Vec<ArchiveRecord> = serde_json::from_str(&records).unwrap();
        assert!(matches!(
            records.as_slice(),
            [ArchiveRecord::SemanticAmbiguityResolved { .. }]
        ));
        let third =
            CommitEnvelope::new(archive, writer, 3, vec![second.commit_id], records).unwrap();
        catalog.apply_commit(&third).unwrap();
        assert!(catalog.conflict(conflict).unwrap().resolved);
        let group: String = catalog
            .db
            .query_row(
                "SELECT group_id FROM sidecars WHERE id=?",
                [sidecar.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(group, generated.group.unwrap().to_string());
        let owner: String = catalog
            .db
            .query_row(
                "SELECT owner FROM sidecars WHERE id=?",
                [sidecar.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner, "preserved-source-companion");
    }
}

#[cfg(test)]
mod generated_lookup_performance {
    use super::*;
    #[test]
    fn camera_ownership_lookup_uses_index_for_case_insensitive_misses() {
        let directory = tempfile::tempdir().unwrap();
        let root = RootHandle::open(directory.path(), crate::fs::RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let group = catalog.create_group(false).unwrap();
        let archive = ArchiveId::new();
        let digest = Digest::of(b"fixture");
        catalog
            .db
            .execute(
                "INSERT INTO archives(id,writer) VALUES(?,?)",
                params![archive.to_string(), WriterId::new().to_string()],
            )
            .unwrap();
        catalog
            .db
            .execute(
                "INSERT INTO blobs(digest,size) VALUES(?,7)",
                [digest.as_str()],
            )
            .unwrap();
        let tx = catalog.db.transaction().unwrap();
        for index in 0..10_000 {
            tx.execute(
                "INSERT INTO members(archive,path,group_id,digest,role) VALUES(?,?,?,?,?)",
                params![
                    archive.to_string(),
                    format!("RAW/{index:05}.ORF"),
                    group.to_string(),
                    digest.as_str(),
                    serde_json::to_string(&MediaRole::Raw).unwrap()
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        unsafe extern "C" fn budget(pointer: *mut std::ffi::c_void) -> i32 {
            let calls = unsafe { &mut *(pointer as *mut u32) };
            *calls += 1;
            i32::from(*calls > 20)
        }
        let mut calls = 0u32;
        let handle = unsafe { catalog.db.handle() };
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                handle,
                1000,
                Some(budget),
                (&mut calls as *mut u32).cast(),
            );
        }
        let result = original_claimed(&catalog, &ArchivePath::parse("raw/missing.orf").unwrap());
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(handle, 0, None, std::ptr::null_mut());
        }
        assert!(
            matches!(result, Ok(false)),
            "ownership lookup exceeded bounded metadata work: {result:?}"
        );
        assert!(original_claimed(&catalog, &ArchivePath::parse("raw/09999.orf").unwrap()).unwrap());
    }
}
