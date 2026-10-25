use crate::{
    catalog::*,
    domain::*,
    edit::*,
    error::{AppError, AppResult},
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
        }
        if let Some((ticket, phase)) = self.active.take() {
            let reply = match self.worker.poll(ticket)? {
                WorkerState::Active | WorkerState::TimedOutStillAlive => {
                    self.active = Some((ticket, phase));
                    return Ok(());
                }
                WorkerState::Failed(error) => {
                    self.status.unreadable += 1;
                    self.status.reason = Some(error.to_string());
                    if let Phase::Capture {
                        context,
                        recovering,
                    } = &phase
                    {
                        self.unreadable(&mut engine.catalog, &context.owner, &error.to_string())?;
                        if *recovering {
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
                    for entry in entries {
                        if matches!(entry.role, SourceRole::Companion(_)) {
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
                                if matches!(
                                    crate::import::source_role(path.as_str()),
                                    Some(SourceRole::Companion(_))
                                ) {
                                    let owner = discover(&mut engine.catalog, &path)?;
                                    if !self.queue.iter().any(|o| o.id == owner.id) {
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
                "SELECT context FROM capture_requests WHERE done=0 ORDER BY rowid LIMIT 1",
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
        } else if let Some(owner) = self.queue.pop_front() {
            let previous = engine.catalog.local_head(owner.id)?;
            let context = CaptureContext {
                operation: OperationId::new(),
                owner,
                previous,
            };
            engine.catalog.db.execute(
                "INSERT INTO capture_requests(operation,context) VALUES(?,?)",
                params![
                    context.operation.to_string(),
                    serde_json::to_string(&context)?
                ],
            )?;
            context.persist(state)?;
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
            return self.advance(engine, local, state, clock, false, false);
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
            let stored = StoredSnapshot::from_receipt(receipt.clone(), state)?;
            if let Some(current) = current
                && engine.catalog.version(current)?.body == receipt.body
            {
                stored.register(state, &mut engine.catalog)?;
            } else {
                let intent = SnapshotIntent {
                    sidecar: owner.id,
                    parents: previous.into_iter().collect(),
                    operation,
                    payload: SnapshotPayload::Stored(stored),
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
            engine.catalog.db.execute(
                "UPDATE residency SET protected=1,last_activity=? WHERE group_id=?",
                params![clock.now(), group.to_string()],
            )?;
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
