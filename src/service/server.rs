use super::{StatusSnapshot, protocol::*};
use crate::{
    archive::*,
    catalog::*,
    config::Config,
    domain::*,
    edit::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
    import::*,
    storage::*,
    transfer::*,
};
use rusqlite::{OptionalExtension, params};
use std::{
    collections::HashMap,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    time::{Duration, Instant},
};
pub(crate) fn check_peer(stream: &UnixStream) -> AppResult<()> {
    let mut peer: libc::ucred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SO_PEERCRED authenticates the kernel's connected Unix peer rather than request claims.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0
        || length as usize != std::mem::size_of::<libc::ucred>()
        || peer.uid != unsafe { libc::geteuid() }
    {
        return Err(AppError::new(
            ErrorCode::Permission,
            "service requires the same local Unix user",
        ));
    }
    Ok(())
}
struct Connection {
    stream: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    request: Option<Request>,
    waiting: Option<JobId>,
    authorization: Option<ApplicationSession>,
    closed: bool,
    done: bool,
    opened: Instant,
}
impl Connection {
    fn new(stream: UnixStream) -> AppResult<Self> {
        check_peer(&stream)?;
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            input: vec![],
            output: vec![],
            request: None,
            waiting: None,
            authorization: None,
            closed: false,
            done: false,
            opened: Instant::now(),
        })
    }
    fn respond(&mut self, response: Response) -> AppResult<()> {
        self.output = serde_json::to_vec(&response)?;
        if self.output.len() >= MAX_MESSAGE {
            self.output = serde_json::to_vec(&Response::error(
                response.request_id,
                AppError::invalid("response exceeds one MiB; use bounded job/group selectors"),
            ))?;
        }
        self.output.push(b'\n');
        self.waiting = None;
        self.authorization = None;
        self.done = true;
        Ok(())
    }
    fn receive(&mut self) -> AppResult<()> {
        if self.done {
            return Ok(());
        }
        if self.request.is_some() {
            let mut byte = [0u8; 1];
            match self.stream.read(&mut byte) {
                Ok(_) => self.closed = true,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => self.closed = true,
            };
            return Ok(());
        }
        let mut bytes = [0u8; 8192];
        loop {
            match self.stream.read(&mut bytes) {
                Ok(0) => {
                    self.closed = true;
                    break;
                }
                Ok(n) => {
                    self.input.extend_from_slice(&bytes[..n]);
                    if self.input.len() > MAX_MESSAGE {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    self.closed = true;
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    fn flush(&mut self) -> AppResult<()> {
        while !self.output.is_empty() {
            match self.stream.write(&self.output) {
                Ok(0) => {
                    self.closed = true;
                    break;
                }
                Ok(n) => {
                    self.output.drain(..n);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    self.closed = true;
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}
#[derive(serde::Serialize, serde::Deserialize, PartialEq)]
struct RootIdentity {
    schema_version: u32,
    profile: ProfileId,
    device: u64,
    inode: u64,
}
fn bind_root(root: &RootHandle, profile: ProfileId, name: &str) -> AppResult<()> {
    let identity = RootIdentity {
        schema_version: 1,
        profile,
        device: root.device(),
        inode: root.inode(),
    };
    root.mkdir_all(&ArchivePath::parse(".photo-sorter")?)?;
    let path = ArchivePath::parse(name)?;
    match root.write_new(&path, &serde_json::to_vec(&identity)?) {
        Ok(()) => {}
        Err(error) => {
            let previous: RootIdentity =
                serde_json::from_slice(&root.read(&path, 64 * 1024).map_err(|_| error)?)?;
            if previous != identity {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "local root belongs to another profile or root identity; explicit recovery is required",
                ));
            }
        }
    }
    Ok(())
}
struct SocketOwner {
    path: std::path::PathBuf,
    inode: u64,
}
impl Drop for SocketOwner {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.file_type().is_socket() && m.ino() == self.inode)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
pub(crate) fn run(config: Config) -> AppResult<()> {
    config.validate()?;
    let state = RootHandle::open(&config.state_root, RootPolicy::Local)?;
    let profile = OwnershipLock::acquire(&state, "profile.lock")?;
    let local = RootHandle::open(&config.working_root, RootPolicy::Local)?;
    ensure_disjoint_roots(&local, &state)?;
    let work = OwnershipLock::acquire(&local, "working.lock")?;
    bind_root(&state, config.profile_id, ".photo-sorter/profile.json")?;
    bind_root(&local, config.profile_id, ".photo-sorter/working-root.json")?;
    let socket = config.state_root.join("service.sock");
    if let Ok(metadata) = std::fs::symlink_metadata(&socket) {
        if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "unexpected file at service socket path; preserved",
            ));
        }
        if UnixStream::connect(&socket).is_ok() {
            return Err(AppError::new(
                ErrorCode::Unavailable,
                "service socket is already active",
            ));
        }
        state.unlink(&ArchivePath::parse("service.sock")?, false)?;
    }
    let mut catalog = Catalog::open(&state)?;
    let _recovery = super::ServiceRecovery::restore(&mut catalog)?;
    let maintenance_worker = WorkerSupervisor::new_local_bound(
        std::env::current_exe()?,
        vec![profile.inherit()?, work.inherit()?],
        &config,
        &local,
        &state,
    )?;
    let observation_worker = WorkerSupervisor::new_local_bound(
        std::env::current_exe()?,
        vec![profile.inherit()?, work.inherit()?],
        &config,
        &local,
        &state,
    )?;
    let local_worker = WorkerSupervisor::new_local_bound(
        std::env::current_exe()?,
        vec![profile.inherit()?, work.inherit()?],
        &config,
        &local,
        &state,
    )?;
    let worker = WorkerSupervisor::new_bound(
        std::env::current_exe()?,
        vec![profile.inherit()?, work.inherit()?],
        &config,
        &local,
        &state,
    )?;
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let _socket_owner = SocketOwner {
        inode: std::fs::symlink_metadata(&socket)?.ino(),
        path: socket,
    };
    let (http, http_reason) = match super::http::AsyncImmich::new(&config, &local, &state) {
        Ok(http) => (http, None),
        Err(error) => (None, Some(error.to_string())),
    };
    let mut runtime = Runtime {
        http,
        maintenance: maintenance_worker,
        startup: super::recovery::StartupVerifier::default(),
        space: super::space::SpaceMeter::default(),
        local_audit: super::verify::LocalAudit::default(),
        refresh_application: super::refresh::RefreshApplication::default(),
        cleanup: super::cleanup::CleanupDriver::default(),
        reader: super::archive_reader::ArchiveReader::default(),
        http_reason,
        rating_cursor: None,
        favorite_cursor: None,
        favorite_intent_cursor: 0,
        favorite_active_intent: None,
        favorite_turn: false,
        rating_intent_cursor: 0,
        rating_active_intent: None,
        merge_cursor: None,
        job_cursor: None,
        engine: ImportCoordinator::new(config.clone(), catalog),
        local,
        state,
        worker,
        observation: super::observation::ObservationDriver::new(observation_worker),
        shared_publisher: super::publication::SharedPublisher::default(),
        scan: super::scan::ScanDriver::default(),
        local_transfer: super::local_transfer::LocalTransferDriver::new(local_worker),
        scheduler: super::Scheduler::new(&config),
        upload: UploadDriver::default(),
        bootstrap: BootstrapDriver::default(),
        fetch: super::pin::PinDriver::default(),
        sessions: ApplicationSessions::default(),
        authorizations: HashMap::new(),
        control: None,
        local_control: None,
        doctor_snapshot: crate::qualification::doctor(&config)?,
        editor_identity: crate::qualification::EditorRuntimeIdentity::capture(&config)
            .ok()
            .flatten(),
        archive_status: serde_json::json!({"availability":"unobserved","identity":null}),
        last_tick: Instant::now(),
    };
    runtime.engine.bind_archive()?;
    runtime.engine.catalog.enable_async_admission();
    runtime
        .engine
        .catalog
        .set_admission_limits(runtime.engine.config.budget, runtime.engine.config.reserve);
    let mut peers: Vec<Connection> = vec![];
    loop {
        for _ in 0..32 {
            match listener.accept() {
                Ok((stream, _)) => {
                    if peers.len() < 128
                        && let Ok(peer) = Connection::new(stream)
                    {
                        peers.push(peer);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        for peer in &mut peers {
            if peer.receive().is_err() {
                peer.closed = true;
            }
            if peer.request.is_none() && !peer.done {
                if peer.input.len() > MAX_MESSAGE {
                    peer.respond(Response::error(
                        OperationId::new(),
                        AppError::invalid("request exceeds one MiB"),
                    ))?;
                } else if let Some(end) = peer.input.iter().position(|b| *b == b'\n') {
                    let parsed = serde_json::from_slice::<Request>(&peer.input[..end]);
                    peer.input.clear();
                    match parsed {
                        Err(_) => peer.respond(Response::error(
                            OperationId::new(),
                            AppError::invalid("invalid bounded protocol request"),
                        ))?,
                        Ok(request) => {
                            if request.protocol_version != PROTOCOL_VERSION {
                                peer.respond(Response::error(
                                    request.request_id,
                                    AppError::invalid("unsupported protocol version"),
                                ))?;
                            } else {
                                if matches!(
                                    request.command,
                                    Command::Refresh {
                                        editor_closed: true,
                                        stage_only: false,
                                        ..
                                    }
                                ) && !peer.closed
                                {
                                    match runtime.sessions.authorize_peer(
                                        true,
                                        request.no_wait,
                                        &peer.stream,
                                    ) {
                                        Ok(authorization) => {
                                            peer.authorization = Some(authorization)
                                        }
                                        Err(error) => {
                                            peer.respond(Response::error(
                                                request.request_id,
                                                error,
                                            ))?;
                                            continue;
                                        }
                                    }
                                }
                                let outcome = runtime.dispatch(
                                    &request,
                                    peer.authorization.as_ref().map(ApplicationSession::id),
                                );
                                match outcome {
                                    Ok(ResponseOutcome::Accepted { job }) if !request.no_wait => {
                                        peer.waiting = Some(job);
                                        peer.request = Some(request)
                                    }
                                    Ok(outcome) => {
                                        let response = Response {
                                            protocol_version: 1,
                                            request_id: request.request_id,
                                            outcome,
                                        };
                                        peer.request = Some(request);
                                        peer.respond(response)?;
                                    }
                                    Err(error) => {
                                        let response = Response::error(request.request_id, error);
                                        peer.request = Some(request);
                                        peer.respond(response)?;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if peer.closed {
                peer.authorization = None;
            }
        }
        if runtime.last_tick.elapsed() >= Duration::from_millis(20) {
            runtime.tick()?;
            runtime.last_tick = Instant::now();
        }
        for peer in &mut peers {
            if !peer.closed
                && let (Some(job), Some(request)) = (peer.waiting, peer.request.as_ref())
                && let Some(response) = runtime.wait_response(request, job)?
            {
                peer.respond(response)?;
            }
            if peer.flush().is_err() {
                peer.closed = true;
            }
        }
        peers.retain(|peer| {
            !peer.closed
                && !(peer.done && peer.output.is_empty())
                && !(peer.request.is_none() && peer.opened.elapsed() > Duration::from_secs(5))
        });
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct RefreshProgress {
    cursor: Option<GroupId>,
    index: usize,
    child: Option<JobId>,
    current_group: Option<GroupId>,
    staged_groups: u64,
    applied: Vec<VersionId>,
    conflicts: Vec<SidecarId>,
    waiting_application: bool,
}
struct Control {
    job: JobId,
    ticket: WorkerTicket,
}
struct Runtime {
    engine: ImportCoordinator,
    maintenance: WorkerSupervisor,
    startup: super::recovery::StartupVerifier,
    space: super::space::SpaceMeter,
    local_audit: super::verify::LocalAudit,
    refresh_application: super::refresh::RefreshApplication,
    cleanup: super::cleanup::CleanupDriver,
    reader: super::archive_reader::ArchiveReader,
    http: Option<super::http::AsyncImmich>,
    http_reason: Option<String>,
    rating_cursor: Option<Option<uuid::Uuid>>,
    favorite_cursor: Option<Option<uuid::Uuid>>,
    favorite_intent_cursor: i64,
    favorite_active_intent: Option<(i64, OperationId)>,
    favorite_turn: bool,
    rating_intent_cursor: i64,
    rating_active_intent: Option<(i64, RatingIntentId)>,
    merge_cursor: Option<SidecarId>,
    job_cursor: Option<JobId>,
    local: RootHandle,
    state: RootHandle,
    worker: WorkerSupervisor,
    upload: UploadDriver,
    observation: super::observation::ObservationDriver,
    shared_publisher: super::publication::SharedPublisher,
    scan: super::scan::ScanDriver,
    local_transfer: super::local_transfer::LocalTransferDriver,
    scheduler: super::Scheduler,
    bootstrap: BootstrapDriver,
    fetch: super::pin::PinDriver,
    sessions: ApplicationSessions,
    authorizations: HashMap<JobId, ApplicationSessionId>,
    control: Option<Control>,
    local_control: Option<Control>,
    archive_status: serde_json::Value,
    doctor_snapshot: serde_json::Value,
    editor_identity: Option<crate::qualification::EditorRuntimeIdentity>,
    last_tick: Instant,
}
impl Runtime {
    fn enqueue(&mut self, request: &Request, kind: JobKind) -> AppResult<ResponseOutcome> {
        let job = self.engine.catalog.enqueue(DurableRequest {
            operation_id: request.request_id,
            kind,
        })?;
        Ok(ResponseOutcome::Accepted { job })
    }
    fn dispatch(
        &mut self,
        request: &Request,
        authorization: Option<ApplicationSessionId>,
    ) -> AppResult<ResponseOutcome> {
        let data = |data| Ok(ResponseOutcome::Data { data });
        if request.dry_run {
            return data(
                serde_json::json!({"dry_run":true,"command":"validated preview","effect":"no jobs, pins, reservations or archive markers created"}),
            );
        }
        match &request.command {
            Command::CheckApplication { job, session } => {
                let authorized = self.authorizations.get(job) == Some(session)
                    && self.sessions.authorized(*session)
                    && self.engine.catalog.job(*job).is_ok_and(|record| {
                        !record.cancel_requested
                            && matches!(
                                record.request.kind,
                                JobKind::Refresh {
                                    stage_only: false,
                                    ..
                                }
                            )
                    });
                data(serde_json::json!({"authorized":authorized}))
            }
            Command::Status | Command::StatusPage { .. } => {
                let default = StatusQuery::default();
                let query = if let Command::StatusPage { query } = &request.command {
                    query
                } else {
                    &default
                };
                let mut archive_status = self.archive_status.clone();
                archive_status["last_successful_sync"] =
                    serde_json::to_value(self.reader.last_success)?;
                archive_status["reason"] = serde_json::to_value(&self.reader.reason)?;
                if self.reader.reason.is_some() {
                    archive_status["availability"] = serde_json::json!("unavailable");
                } else if self.reader.last_success.is_some() {
                    archive_status["availability"] = serde_json::json!("online");
                }
                let mut status = StatusSnapshot::collect_page(
                    &self.engine,
                    &self.space,
                    serde_json::json!({"pid":self.worker.pid(),"local_pid":self.local_transfer.worker.pid(),"observation_pid":self.observation.worker.pid(),"maintenance_pid":self.maintenance.pid(),"available":self.worker.available()?,"control_job":self.control.as_ref().map(|control|control.job),"refresh_application_job":self.refresh_application.active_job(),"local_audit_job":self.local_audit.active_job()}),
                    archive_status,
                    query,
                )?;
                for metrics in [
                    self.worker.metrics(),
                    self.local_transfer.worker.metrics(),
                    self.observation.worker.metrics(),
                    self.maintenance.metrics(),
                ] {
                    status.session_metrics.add(&metrics);
                }
                let mut nas = self.worker.nas_decision().unwrap_or(
                    crate::qualification::CapabilityDecision {
                        allowed: false,
                        reason: "archive capability has not been observed by the worker".into(),
                    },
                );
                if self.worker.is_uncertain() {
                    nas.allowed = false;
                    nas.reason =
                        "worker outcome is uncertain; no replacement publisher is allowed".into();
                }
                status.capabilities["nas_publication"] = serde_json::to_value(nas)?;
                status.worker["publication_reason"] =
                    serde_json::to_value(&self.shared_publisher.reason)?;
                status.watcher = serde_json::to_value(&self.observation.status)?;
                status.watcher["physical_verification_reason"] =
                    serde_json::to_value(&self.startup.reason)?;
                data(serde_json::to_value(status)?)
            }
            Command::Import { source, strict } => {
                let job = self.engine.begin_scan(&ImportRequest {
                    source_path: source.clone(),
                    strict: *strict,
                    dry_run: false,
                    operation_id: request.request_id,
                })?;
                Ok(ResponseOutcome::Accepted { job })
            }
            Command::Bootstrap { scope } => {
                let config = self.engine.config.clone();
                let job = BootstrapCoordinator::new(
                    &config,
                    &mut self.engine.catalog,
                    &mut self.bootstrap,
                    &mut self.worker,
                )
                .start(BootstrapRequest {
                    operation: request.request_id,
                    archive: config
                        .archive_id
                        .ok_or_else(|| AppError::invalid("profile has no archive identity"))?,
                    scope: scope.clone(),
                    batch_size: config.metadata_batch,
                })?;
                Ok(ResponseOutcome::Accepted { job })
            }
            Command::Pin { target } => {
                if let Ok(group) = target.parse::<GroupId>() {
                    return Ok(ResponseOutcome::Accepted {
                        job: self.engine.catalog.request_pin(group, request.request_id)?,
                    });
                }
                let immich = self.engine.config.immich.as_ref().ok_or_else(|| {
                    AppError::invalid("pin requires a group UUID or configured Immich photo URL")
                })?;
                let origin = url::Url::parse(&immich.origin)
                    .map_err(|_| AppError::invalid("invalid Immich origin"))?;
                let url =
                    url::Url::parse(target).map_err(|_| AppError::invalid("invalid photo URL"))?;
                let asset = crate::immich::parse_photo_url(url.as_str(), &origin)?;
                self.enqueue(
                    request,
                    JobKind::PinUrl {
                        server: origin.origin().ascii_serialization(),
                        asset,
                    },
                )
            }
            Command::Unpin { group } => {
                self.engine.catalog.canonical_group(*group)?;
                self.enqueue(request, JobKind::Unpin { group: *group })
            }
            Command::Refresh {
                groups,
                editor_closed,
                stage_only,
            } => {
                if *editor_closed && request.no_wait && !stage_only {
                    return Err(AppError::invalid(
                        "applying refresh requires a foreground connection",
                    ));
                }
                let groups = groups
                    .iter()
                    .map(|group| self.engine.catalog.canonical_group(*group))
                    .collect::<AppResult<Vec<_>>>()?;
                let outcome = self.enqueue(
                    request,
                    JobKind::Refresh {
                        groups,
                        stage_only: *stage_only,
                    },
                )?;
                if let (ResponseOutcome::Accepted { job }, Some(session)) =
                    (&outcome, authorization)
                {
                    self.authorizations.insert(*job, session);
                }
                Ok(outcome)
            }
            Command::Clean => self.enqueue(request, JobKind::Clean),
            Command::JobShow { job, applied_after } => {
                let response =
                    self.job_response_page(request.request_id, *job, applied_after.unwrap_or(0))?;
                Ok(response.outcome)
            }
            Command::JobWait { job } => {
                self.engine.catalog.job(*job)?;
                Ok(ResponseOutcome::Accepted { job: *job })
            }
            Command::JobCancel { job } => {
                let record = self.engine.catalog.job(*job)?;
                self.engine.catalog.transition(
                    *job,
                    record.generation,
                    JobTransition::RequestCancel,
                )?;
                Ok(self.job_response(request.request_id, *job)?.outcome)
            }
            Command::ConflictsPage { after, limit } => {
                let conflicts = self.engine.catalog.conflicts_page(*after, *limit, false)?;
                let next = conflicts.last().map(|c| c.id);
                let has_more = if let Some(id) = next {
                    !self
                        .engine
                        .catalog
                        .conflicts_page(Some(id), 1, false)?
                        .is_empty()
                } else {
                    false
                };
                data(serde_json::json!({"conflicts":conflicts,"next":next,"has_more":has_more}))
            }
            Command::ConflictsList => data(serde_json::to_value(
                self.engine.catalog.conflicts_page(None, 128, false)?,
            )?),
            Command::ConflictResolve {
                request: resolution,
            } => {
                self.engine.catalog.resolution_records(resolution)?;
                self.enqueue(
                    request,
                    JobKind::ResolveAmbiguity {
                        request: resolution.clone(),
                    },
                )
            }
            Command::Verify { archive } => {
                self.enqueue(request, JobKind::Verify { archive: *archive })
            }
            Command::ChangeMode { request: change } => self.enqueue(
                request,
                JobKind::ChangeMode {
                    request: change.clone(),
                },
            ),
            Command::ArchiveInit => self.enqueue(request, JobKind::ArchiveInit),
            Command::RecoverLock {
                prior_owner_stopped,
                all_writers_quiescent,
            } => {
                if !prior_owner_stopped || !all_writers_quiescent {
                    return Err(AppError::invalid(
                        "recovery requires stopped prior owner and quiescent writers assertions",
                    ));
                }
                self.enqueue(
                    request,
                    JobKind::RecoverLock {
                        prior_owner_stopped: *prior_owner_stopped,
                        all_writers_quiescent: *all_writers_quiescent,
                    },
                )
            }
            Command::CatalogBackup { destination } => self.enqueue(
                request,
                JobKind::Backup {
                    destination: destination.clone(),
                },
            ),
            Command::CatalogRebuild { replacement } => self.enqueue(
                request,
                JobKind::Rebuild {
                    replacement: replacement.clone(),
                },
            ),
            Command::Doctor => data(self.doctor_snapshot.clone()),
            Command::Qualify { request: probe } => self.enqueue(
                request,
                JobKind::Qualify {
                    request: probe.clone(),
                },
            ),
        }
    }
    fn job_response(&self, id: OperationId, job: JobId) -> AppResult<Response> {
        self.job_response_page(id, job, 0)
    }
    fn job_response_page(&self, id: OperationId, job: JobId, after: u64) -> AppResult<Response> {
        if after > i64::MAX as u64 {
            return Err(AppError::invalid("application cursor out of range"));
        }
        let record = self.engine.catalog.job(job)?;
        let mut detail = match record.request.kind {
            JobKind::Import { .. } => serde_json::to_value(self.engine.report(job).ok())?,
            _ => {
                let text: Option<String> = self
                    .engine
                    .catalog
                    .db
                    .query_row(
                        "SELECT result FROM job_results WHERE job=?",
                        [job.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?;
                text.map(|t| serde_json::from_str(&t))
                    .transpose()?
                    .unwrap_or(serde_json::Value::Null)
            }
        };
        if matches!(record.request.kind, JobKind::Refresh { .. }) {
            if !detail.is_object() {
                detail = serde_json::json!({});
            }
            let mut query=self.engine.catalog.db.prepare("SELECT rowid,version FROM refresh_application_receipts WHERE job=? AND rowid>? ORDER BY rowid LIMIT 129")?;
            let mut rows = query
                .query_map(params![job.to_string(), after], |r| {
                    Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let more = rows.len() > 128;
            rows.truncate(128);
            let next = if more {
                rows.last().map(|(row, _)| *row)
            } else {
                None
            };
            let count: u64 = self.engine.catalog.db.query_row(
                "SELECT count(*) FROM refresh_application_receipts WHERE job=?",
                [job.to_string()],
                |r| r.get(0),
            )?;
            detail["applied"] = serde_json::to_value(
                rows.into_iter()
                    .map(|(_, version)| version)
                    .collect::<Vec<_>>(),
            )?;
            detail["applied_count"] = serde_json::json!(count);
            detail["applied_page"] = serde_json::json!({"next":next,"has_more":more});
        }
        Ok(Response {
            protocol_version: 1,
            request_id: id,
            outcome: ResponseOutcome::Job {
                job: record,
                detail,
            },
        })
    }
    fn wait_response(&self, request: &Request, job: JobId) -> AppResult<Option<Response>> {
        let record = self.engine.catalog.job(job)?;
        let conflict = matches!(
            record.state,
            JobState::Conflict | JobState::Failed | JobState::Cancelled
        );
        let complete = record.state == JobState::Complete;
        let milestone = match record.request.kind {
            JobKind::Import { .. } => match request.wait_for {
                Milestone::Ingested => record.local_ready,
                Milestone::Archived | Milestone::Ready => record.archive_committed,
            },
            _ => false,
        };
        if let JobKind::PinUrl { asset, .. } = record.request.kind
            && record.state == JobState::WaitingImmich
            && self
                .http
                .as_ref()
                .is_some_and(|http| !http.asset_failed(asset))
        {
            return Ok(None);
        }
        if record.state == JobState::WaitingCapacity
            && record
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("fresh worker capacity measurement pending"))
        {
            return Ok(None);
        }
        let blocked = matches!(
            record.state,
            JobState::WaitingSource
                | JobState::WaitingNas
                | JobState::WaitingImmich
                | JobState::WaitingCapacity
                | JobState::WaitingApplication
        );
        if conflict || complete || milestone || blocked {
            let mut response = self.job_response(request.request_id, job)?;
            if milestone
                && !conflict
                && let ResponseOutcome::Job { ref mut job, .. } = response.outcome
            {
                job.state = JobState::Complete;
            }
            return Ok(Some(response));
        }
        Ok(None)
    }
    fn finish_control(&mut self, job: JobId, reply: NasReply) -> AppResult<()> {
        if let NasReply::Captured(receipt) = &reply {
            let JobKind::SnapshotCapture {
                context,
                direct: true,
            } = self.engine.catalog.job(job)?.request.kind
            else {
                return Err(AppError::integrity(
                    "archive capture receipt has no matching direct-capture job",
                ));
            };
            let context: CaptureContext = serde_json::from_value(context)?;
            self.observation.record_archived(
                &mut self.engine,
                &self.local,
                &self.state,
                context,
                receipt.clone(),
            )?;
            self.result(
                job,
                serde_json::json!({"captured":receipt.body,"archive_history":true}),
            )?;
            return self.complete(job);
        }
        if let NasReply::Capability(report) = reply {
            let JobKind::Qualify { request } = self.engine.catalog.job(job)?.request.kind else {
                return Err(AppError::integrity(
                    "probe receipt has no qualification job",
                ));
            };
            let request: crate::qualification::QualificationRequest =
                serde_json::from_value(request)?;
            crate::qualification::validate_request(&self.engine.config, &request)?;
            let evidence = crate::qualification::persist_report(&self.engine.config, &report)?;
            self.engine.catalog.db.execute("INSERT INTO capabilities(id,report) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET report=excluded.report",params![Digest::of(&serde_json::to_vec(&report)?).as_str(),serde_json::to_string(&report)?])?;
            let qualified = crate::qualification::report_qualified(&request, &report);
            self.result(
                job,
                serde_json::json!({"report":report,"qualified":qualified,"evidence":evidence}),
            )?;
        } else {
            self.result(job, serde_json::to_value(reply)?)?;
        }
        self.complete(job)
    }
    fn advance_refresh(
        &mut self,
        record: &JobRecord,
        groups: &[GroupId],
        stage_only: bool,
    ) -> AppResult<()> {
        let job = record.id;
        let encoded: Option<String> = self
            .engine
            .catalog
            .db
            .query_row(
                "SELECT result FROM job_results WHERE job=?",
                [job.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let mut progress: RefreshProgress = encoded
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_default();
        if groups.is_empty() && progress.current_group.is_none() {
            let checking: bool = self.engine.catalog.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM startup_checks)",
                [],
                |r| r.get(0),
            )?;
            if checking {
                return Ok(());
            }
        }
        let group = if let Some(group) = progress.current_group {
            self.engine.catalog.canonical_group(group)?
        } else if groups.is_empty() {
            let page = self.engine.catalog.group_batch(progress.cursor, 128)?;
            if page.is_empty() {
                self.result(job, &progress)?;
                self.complete(job)?;
                self.authorizations.remove(&job);
                return Ok(());
            }
            let candidate = page
                .iter()
                .find(|g| {
                    self.engine
                        .catalog
                        .readiness(g.id)
                        .is_ok_and(|r| r.originals_ready)
                })
                .map(|g| g.id);
            if let Some(group) = candidate {
                group
            } else {
                progress.cursor = page.last().map(|g| g.id);
                self.result(job, progress)?;
                return Ok(());
            }
        } else if let Some(group) = groups.get(progress.index) {
            *group
        } else {
            self.result(job, &progress)?;
            self.complete(job)?;
            self.authorizations.remove(&job);
            return Ok(());
        };
        let child = if let Some(child) = progress.child {
            child
        } else {
            let child = self.engine.catalog.enqueue(DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::MetadataFetch {
                    group,
                    versions: vec![],
                },
            })?;
            progress.current_group = Some(group);
            progress.child = Some(child);
            self.result(job, &progress)?;
            child
        };
        let staged = self.engine.catalog.job(child)?;
        if staged.state != JobState::Complete {
            if matches!(
                staged.state,
                JobState::Conflict
                    | JobState::Failed
                    | JobState::WaitingNas
                    | JobState::WaitingCapacity
            ) {
                self.wait(
                    job,
                    staged.state,
                    staged
                        .reason
                        .unwrap_or_else(|| "metadata staging pending".into()),
                )?;
            }
            return Ok(());
        }
        if !stage_only {
            let report = if self.refresh_application.active_job() == Some(job) {
                let Some(report) = self.refresh_application.poll(
                    &mut self.engine.catalog,
                    &mut self.maintenance,
                    job,
                )?
                else {
                    return Ok(());
                };
                report
            } else {
                let Some(session) = self
                    .authorizations
                    .get(&job)
                    .copied()
                    .filter(|s| self.sessions.authorized(*s))
                else {
                    return self.wait(job,JobState::WaitingApplication,"metadata staged; a fresh foreground refresh --editor-closed is required before live metadata application".into());
                };
                let pending:Option<(String,String)>=self.engine.catalog.db.query_row("SELECT json_extract(j.state,'$'),coalesce(j.reason,'saved local capture is in progress') FROM capture_requests c JOIN sidecars s ON s.id=json_extract(c.context,'$.owner.id') JOIN jobs j ON j.id=c.job WHERE c.done=0 AND s.group_id=? LIMIT 1",[group.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                if let Some((state, reason)) = pending {
                    if matches!(state.as_str(), "Queued" | "Running") {
                        return Ok(());
                    }
                    return self.wait(job,JobState::WaitingApplication,format!("saved local metadata must finish preservation before application: {reason}"));
                }
                if !self.engine.catalog.readiness(group)?.originals_ready {
                    let checking: bool = self.engine.catalog.db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM startup_checks WHERE group_id=?)",
                        [group.to_string()],
                        |r| r.get(0),
                    )?;
                    if checking {
                        return Ok(());
                    }
                    return self.wait(
                        job,
                        JobState::WaitingSource,
                        "originals must be resident before live metadata application".into(),
                    );
                }
                self.refresh_application.start(
                    &mut self.engine.catalog,
                    &self.engine.config,
                    &mut self.maintenance,
                    job,
                    RefreshRequest {
                        groups: vec![group],
                        application_session: session,
                        operation_id: record.request.operation_id,
                    },
                )?;
                return Ok(());
            };
            let has_more = report.has_more;
            let remaining = 128usize.saturating_sub(progress.applied.len());
            progress
                .applied
                .extend(report.applied.into_iter().take(remaining));
            progress.conflicts.extend(report.conflicts);
            progress.waiting_application = report.waiting_application;
            if !progress.conflicts.is_empty() || progress.waiting_application {
                self.result(job, &progress)?;
                self.authorizations.remove(&job);
                return self.wait(
                    job,
                    if progress.waiting_application {
                        JobState::WaitingApplication
                    } else {
                        JobState::Conflict
                    },
                    "refresh stopped at application boundary; bytes preserved".into(),
                );
            }
            if has_more {
                self.result(job, &progress)?;
                return Ok(());
            }
        }
        progress.staged_groups += 1;
        progress.index += 1;
        progress.cursor = Some(group);
        progress.child = None;
        progress.current_group = None;
        self.result(job, progress)?;
        Ok(())
    }
    fn result(&mut self, job: JobId, value: impl serde::Serialize) -> AppResult<()> {
        self.engine.catalog.db.execute("INSERT INTO job_results(job,result) VALUES(?,?) ON CONFLICT(job) DO UPDATE SET result=excluded.result",params![job.to_string(),serde_json::to_string(&value)?])?;
        Ok(())
    }
    fn complete(&mut self, job: JobId) -> AppResult<()> {
        let record = self.engine.catalog.job(job)?;
        if !matches!(record.state, JobState::Complete | JobState::Cancelled) {
            self.engine.catalog.transition(
                job,
                record.generation,
                if record.cancel_requested {
                    JobTransition::ConfirmCancelled
                } else {
                    JobTransition::Complete
                },
            )?;
        }
        Ok(())
    }
    fn wait(&mut self, job: JobId, state: JobState, reason: String) -> AppResult<()> {
        let record = self.engine.catalog.job(job)?;
        if record.state == state && record.reason.as_deref() == Some(&reason) {
            return Ok(());
        }
        self.engine
            .catalog
            .transition(job, record.generation, JobTransition::Wait(state, reason))
    }
    fn tick(&mut self) -> AppResult<()> {
        let clock = SystemClock::default();
        let timing = self.scheduler.tick(clock.now())?;
        if timing.poll_ratings {
            self.rating_cursor = Some(None);
            self.favorite_cursor = Some(None);
        }
        if timing.poll_ratings {
            self.reader.requested = true;
        }

        if let Some(http) = &self.http
            && let Err(error) = http.pump()
        {
            self.http_reason = Some(error.to_string());
        }

        if let Err(error) = self.cleanup.advance(
            &mut self.engine,
            &self.local,
            &self.state,
            &mut self.maintenance,
        ) {
            self.cleanup.reason = Some(error.to_string());
        }
        self.space.advance(
            &self.engine.config,
            &self.engine.catalog,
            &mut self.maintenance,
            clock.now(),
        )?;
        self.observation.blocked_group = self
            .refresh_application
            .active_group()
            .map(|group| self.engine.catalog.canonical_group(group))
            .transpose()?;
        if let Err(error) = self.observation.advance(
            &mut self.engine,
            &self.local,
            &self.state,
            &clock,
            timing.observe_edits,
            timing.reconcile,
        ) {
            self.observation.status.reason = Some(error.to_string());
        }
        if timing.reconcile {
            self.engine.catalog.queue_physical_checks()?;
        }
        self.startup
            .advance(&mut self.engine, &mut self.maintenance)?;
        self.engine.catalog.finalize_rebuild(
            self.reader.completed_syncs > 0,
            self.observation.status.startup_complete,
        )?;
        self.worker.timeout(Duration::from_secs(5));
        if (self.reader.busy()
            || (self.control.is_none()
                && self.upload.active_job().is_none()
                && self.bootstrap.active_job().is_none()
                && self.fetch.nas_active_job().is_none()
                && !self.shared_publisher.busy()
                && self.worker.available()?))
            && let Err(error) = self.reader.advance(
                &mut self.engine,
                &self.state,
                &mut self.worker,
                &mut self.maintenance,
            )
        {
            self.reader.reason = Some(error.to_string());
        }

        if let Some(control) = self.local_control.take() {
            match self.maintenance.poll(control.ticket)? {
                WorkerState::Active | WorkerState::TimedOutStillAlive => {
                    self.local_control = Some(control)
                }
                WorkerState::Completed(reply) => {
                    if let Err(error) = self.finish_control(control.job, reply) {
                        self.wait(control.job, JobState::Failed, error.to_string())?;
                    }
                }
                WorkerState::Failed(error) => self.wait(
                    control.job,
                    if error.code == ErrorCode::Capacity || error.errno == Some(libc::ENOSPC) {
                        JobState::WaitingCapacity
                    } else {
                        JobState::Failed
                    },
                    error.to_string(),
                )?,
                WorkerState::Reaped => self.wait(
                    control.job,
                    JobState::Failed,
                    "maintenance worker disappeared; partial backup retained".into(),
                )?,
            }
        }
        if let Some(control) = self.control.take() {
            match self.worker.poll(control.ticket)? {
                WorkerState::Active | WorkerState::TimedOutStillAlive => {
                    self.control = Some(control)
                }
                WorkerState::Completed(reply) => {
                    if let NasReply::Marker(marker) = &reply {
                        self.archive_status =
                            serde_json::json!({"availability":"online","identity":marker});
                    }
                    if let Err(error) = self.finish_control(control.job, reply) {
                        self.wait(control.job, JobState::Failed, error.to_string())?;
                    }
                }
                WorkerState::Failed(error) => self.wait(
                    control.job,
                    if matches!(
                        self.engine.catalog.job(control.job)?.request.kind,
                        JobKind::Qualify { .. } | JobKind::ChangeMode { .. }
                    ) {
                        JobState::Failed
                    } else if error.code == ErrorCode::Capacity || error.errno == Some(libc::ENOSPC)
                    {
                        JobState::WaitingCapacity
                    } else {
                        JobState::WaitingNas
                    },
                    error.to_string(),
                )?,
                WorkerState::Reaped => self.wait(
                    control.job,
                    JobState::WaitingNas,
                    "worker disappeared; recovery is required".into(),
                )?,
            }
        }
        let editor_allowed = self.engine.config.local_test_backend
            || self
                .editor_identity
                .as_ref()
                .is_some_and(|identity| identity.decision(&self.engine.config).allowed);
        if editor_allowed
            && let Err(error) = super::metadata_merge::advance(
                &mut self.engine.catalog,
                &self.state,
                &self.engine.config,
                &mut self.merge_cursor,
            )
        {
            if error.code == ErrorCode::Capacity {
                self.pressure_reclaim(None)?;
            }
            self.http_reason = Some(error.to_string());
        }
        let now = clock.now();
        let mut jobs = self.engine.catalog.jobs_page(self.job_cursor, 128, true)?;
        self.job_cursor = jobs.last().map(|job| job.id);
        let active = [
            self.scan.active_job(),
            self.local_transfer.active_parent(),
            self.upload.active_job(),
            self.fetch.active_job(),
            self.bootstrap.active_job(),
            self.reader.active_job(),
            self.refresh_application.active_job(),
            self.local_audit.active_job(),
        ];
        for job in active.into_iter().flatten() {
            if !jobs.iter().any(|record| record.id == job) {
                jobs.push(self.engine.catalog.job(job)?);
            }
        }
        for record in jobs {
            if matches!(
                record.state,
                JobState::Complete | JobState::Cancelled | JobState::Failed
            ) || (record.state == JobState::Conflict
                && !matches!(record.request.kind, JobKind::Import { .. })
                && !active.contains(&Some(record.id)))
            {
                continue;
            }
            let http_ready = matches!(record.request.kind,JobKind::PinUrl{asset,..} if self.http.as_ref().is_some_and(|http|http.has_ready_asset(asset)));
            if !matches!(record.request.kind, JobKind::Import { .. })
                && !active.contains(&Some(record.id))
                && !http_ready
                && !self.engine.catalog.job_is_due(record.id, now)?
            {
                continue;
            }
            let was_due = self.engine.catalog.job_is_due(record.id, now)?;
            if record.cancel_requested {
                if let JobKind::SnapshotCapture { direct, context } = &record.request.kind {
                    let context: CaptureContext = serde_json::from_value(context.clone())?;
                    let pending:bool=self.engine.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM capture_requests WHERE operation=? AND done=0)",[context.operation.to_string()],|r|r.get(0))?;
                    if !direct && pending {
                        continue;
                    }
                    if *direct
                        && self
                            .control
                            .as_ref()
                            .is_none_or(|control| control.job != record.id)
                    {
                        self.engine.catalog.db.execute(
                            "UPDATE capture_requests SET done=1 WHERE operation=?",
                            [context.operation.to_string()],
                        )?;
                        context.cleanup(&self.state)?;
                    }
                }
                if self.refresh_application.active_job() == Some(record.id) {
                    let _ = self.refresh_application.poll(
                        &mut self.engine.catalog,
                        &mut self.maintenance,
                        record.id,
                    )?;
                }
                self.local_audit.cancel(&mut self.maintenance, record.id)?;
                self.scan.cancel(record.id)?;
                self.upload.cancel(&mut self.worker, record.id)?;
                self.bootstrap.cancel(&mut self.worker, record.id)?;
                self.fetch.cancel(
                    &mut self.engine.catalog,
                    &mut self.worker,
                    &mut self.maintenance,
                    record.id,
                )?;
                if self.local_transfer.active_parent() == Some(record.id) {
                    self.local_transfer.advance(&mut self.engine, record.id)?;
                }
                let draining = [
                    self.scan.active_job(),
                    self.upload.active_job(),
                    self.bootstrap.active_job(),
                    self.fetch.active_job(),
                    self.local_transfer.active_parent(),
                    self.reader.active_job(),
                    self.refresh_application.active_job(),
                    self.local_audit.active_job(),
                    self.control.as_ref().map(|c| c.job),
                    self.local_control.as_ref().map(|c| c.job),
                ]
                .contains(&Some(record.id));
                if !draining {
                    self.complete(record.id)?;
                }
                continue;
            }
            let result = self.advance(&record);
            if let Err(error) = result {
                let state = match error.code {
                    ErrorCode::Conflict | ErrorCode::Integrity | ErrorCode::SourceChanged => {
                        JobState::Conflict
                    }
                    ErrorCode::Io if error.errno == Some(libc::ENOSPC) => JobState::WaitingCapacity,
                    ErrorCode::Io
                        if matches!(
                            record.request.kind,
                            JobKind::Backup { .. } | JobKind::Clean | JobKind::Refresh { .. }
                        ) =>
                    {
                        JobState::Failed
                    }
                    ErrorCode::Io
                        if matches!(record.request.kind, JobKind::Import { .. })
                            && error.errno == Some(libc::ENOENT) =>
                    {
                        JobState::WaitingSource
                    }
                    ErrorCode::Unavailable | ErrorCode::NotFound | ErrorCode::Io => {
                        JobState::WaitingNas
                    }
                    ErrorCode::Capacity => JobState::WaitingCapacity,
                    _ => JobState::Failed,
                };
                self.wait(record.id, state.clone(), error.to_string())?;
                if state == JobState::WaitingCapacity
                    && matches!(
                        record.request.kind,
                        JobKind::Pin { .. }
                            | JobKind::PinUrl { .. }
                            | JobKind::MetadataFetch { .. }
                    )
                {
                    self.pressure_reclaim(Some(record.id))?;
                }
            }
            let current = self.engine.catalog.job(record.id)?;
            if was_due
                && !current.reason.as_deref().is_some_and(|reason| {
                    reason.contains("fresh worker capacity measurement pending")
                })
                && matches!(
                    current.state,
                    JobState::WaitingNas
                        | JobState::WaitingImmich
                        | JobState::WaitingSource
                        | JobState::WaitingCapacity
                )
            {
                self.engine.catalog.defer_job(
                    record.id,
                    now,
                    current
                        .reason
                        .as_deref()
                        .unwrap_or("dependency unavailable"),
                )?;
            }
        }
        let editor_allowed = self.engine.config.local_test_backend
            || self
                .editor_identity
                .as_ref()
                .is_some_and(|identity| identity.decision(&self.engine.config).allowed);
        let favorite_allowed = self.engine.config.local_test_backend
            || self
                .editor_identity
                .as_ref()
                .is_some_and(|identity| identity.pick_decision(&self.engine.config).allowed);
        if let Some(http) = &self.http {
            if let Some(after) = self.favorite_cursor.filter(|_| {
                favorite_allowed
                    && self.favorite_active_intent.is_none()
                    && self.rating_active_intent.is_none()
            }) {
                let scope = crate::immich::RatingScope {
                    state_root: self.engine.config.state_root.clone(),
                    editor_version: self.engine.config.rapidraw_version.clone(),
                    after,
                    limit: 1,
                };
                match (crate::immich::FavoriteSynchronizer {
                    catalog: &mut self.engine.catalog,
                    client: http,
                })
                .poll(scope)
                {
                    Ok(report) => {
                        if !report.retry && (report.observed > 0 || !http.in_flight()) {
                            self.favorite_cursor = report.next.map(Some);
                        }
                    }
                    Err(error) => {
                        self.http_reason = Some(error.to_string());
                        if error.code != ErrorCode::Capacity {
                            self.favorite_cursor = None;
                        }
                    }
                }
            }
            if let Some(after) = self.rating_cursor.filter(|_| {
                editor_allowed
                    && self.rating_active_intent.is_none()
                    && self.favorite_active_intent.is_none()
            }) {
                let config = &self.engine.config;
                let scope = crate::immich::RatingScope {
                    state_root: config.state_root.clone(),
                    editor_version: config.rapidraw_version.clone(),
                    after,
                    limit: 1,
                };
                match (crate::immich::RatingSynchronizer {
                    catalog: &mut self.engine.catalog,
                    client: http,
                })
                .poll(scope)
                {
                    Ok(report) => {
                        if report.observed > 0 || !http.in_flight() {
                            self.rating_cursor = report.next.map(Some);
                        }
                    }
                    Err(error) => {
                        self.http_reason = Some(error.to_string());
                        self.rating_cursor = None;
                    }
                }
            }
            if self.rating_active_intent.is_none()
                && self.favorite_active_intent.is_none()
                && !http.in_flight()
            {
                self.favorite_turn = !self.favorite_turn;
                if self.favorite_turn && favorite_allowed {
                    self.favorite_active_intent = self
                        .engine
                        .catalog
                        .next_favorite_intent(self.favorite_intent_cursor)?;
                }
                if self.favorite_active_intent.is_none() {
                    self.rating_active_intent = super::rating_queue::next_intent(
                        &self.engine.catalog,
                        self.rating_intent_cursor,
                    )?;
                }
                if self.rating_active_intent.is_none()
                    && self.favorite_active_intent.is_none()
                    && favorite_allowed
                {
                    self.favorite_active_intent = self
                        .engine
                        .catalog
                        .next_favorite_intent(self.favorite_intent_cursor)?;
                }
            }
            if let Some((row, id)) = self.favorite_active_intent {
                match (crate::immich::FavoriteSynchronizer {
                    catalog: &mut self.engine.catalog,
                    client: http,
                })
                .advance(id)
                {
                    Ok(report) => {
                        if let Some(reason) = report.reason {
                            self.http_reason = Some(reason);
                        }
                    }
                    Err(error) => self.http_reason = Some(error.to_string()),
                }
                if !http.in_flight() {
                    self.favorite_intent_cursor = row;
                    self.favorite_active_intent = None;
                }
            }
            if let Some((row, id)) = self.rating_active_intent {
                match (crate::immich::RatingSynchronizer {
                    catalog: &mut self.engine.catalog,
                    client: http,
                })
                .advance(id)
                {
                    Ok(report) => {
                        if let Some(reason) = report.reason {
                            self.http_reason = Some(reason);
                        }
                    }
                    Err(error) => self.http_reason = Some(error.to_string()),
                }
                if !http.in_flight() {
                    self.rating_intent_cursor = row;
                    self.rating_active_intent = None;
                }
            }
        }
        if (self.shared_publisher.busy()
            || (self.control.is_none()
                && self.upload.active_job().is_none()
                && self.bootstrap.active_job().is_none()
                && self.fetch.nas_active_job().is_none()
                && !self.reader.busy()
                && self.worker.available()?))
            && let Err(error) =
                self.shared_publisher
                    .advance(&mut self.engine, &self.state, &mut self.worker)
        {
            self.shared_publisher.reason = Some(error.to_string());
        }
        Ok(())
    }
    fn nas_job_allowed(&self, job: JobId) -> bool {
        !self.reader.busy()
            && !self.shared_publisher.busy()
            && self.control.as_ref().is_none_or(|c| c.job == job)
            && self.upload.active_job().is_none_or(|id| id == job)
            && self.bootstrap.active_job().is_none_or(|id| id == job)
            && self.fetch.nas_active_job().is_none_or(|id| id == job)
    }
    fn pressure_reclaim(&mut self, parent: Option<JobId>) -> AppResult<bool> {
        let pending:bool=self.engine.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE json_extract(request,'$.kind.type')='evict' AND json_extract(state,'$') NOT IN('Complete','Cancelled','Failed','Conflict'))",[],|r|r.get(0))?;
        let reclaim = if pending {
            true
        } else if let Some(intent) = EvictionPlanner::candidates_with_hold(
            &self.engine.catalog,
            SystemClock::default().now(),
            self.engine.config.activity_hold_seconds,
        )?
        .into_iter()
        .next()
        {
            self.engine.catalog.enqueue(DurableRequest {
                operation_id: intent.operation,
                kind: JobKind::Evict {
                    group: intent.group,
                    revision: intent.revision,
                    members: intent.members,
                    activity: intent.eligibility_activity,
                },
            })?;
            true
        } else {
            false
        };
        if reclaim && let Some(parent) = parent {
            let record = self.engine.catalog.job(parent)?;
            if record.state != JobState::Running {
                self.engine
                    .catalog
                    .transition(parent, record.generation, JobTransition::Resume)?;
            }
            self.engine
                .catalog
                .db
                .execute("DELETE FROM job_retry WHERE job=?", [parent.to_string()])?;
        }
        Ok(reclaim)
    }
    fn pin_shared_fresh(&mut self, job: JobId) -> AppResult<bool> {
        let required: Option<u64> = self
            .engine
            .catalog
            .db
            .query_row(
                "SELECT required FROM pin_sync_epochs WHERE job=?",
                [job.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let required = if let Some(required) = required {
            required
        } else {
            let required = self.reader.request_fresh_sync();
            self.engine.catalog.db.execute(
                "INSERT INTO pin_sync_epochs(job,required) VALUES(?,?)",
                params![job.to_string(), required],
            )?;
            required
        };
        Ok(self.reader.completed_syncs >= required)
    }
    fn advance(&mut self, record: &JobRecord) -> AppResult<()> {
        let job = record.id;
        let config = self.engine.config.clone();
        match &record.request.kind {
            JobKind::SnapshotCapture { context, direct } => {
                if *direct
                    && self.nas_job_allowed(job)
                    && self.control.is_none()
                    && self.worker.available()?
                {
                    let context: CaptureContext = serde_json::from_value(context.clone())?;
                    if let Some(group) = self.refresh_application.active_group() {
                        let blocked: bool = self.engine.catalog.db.query_row(
                            "SELECT EXISTS(SELECT 1 FROM sidecars WHERE id=? AND group_id=?)",
                            params![
                                context.owner.id.to_string(),
                                self.engine.catalog.canonical_group(group)?.to_string()
                            ],
                            |r| r.get(0),
                        )?;
                        if blocked {
                            return Ok(());
                        }
                    }
                    let ticket = self.worker.submit(NasCommand::CaptureHistory {
                        root: config.archive_root.clone(),
                        archive: config
                            .archive_id
                            .ok_or_else(|| AppError::invalid("profile has no archive identity"))?,
                        writer: config.writer_id,
                        local: config.working_root.clone(),
                        context,
                        ceiling: config.json_ceiling,
                    })?;
                    self.control = Some(Control { job, ticket });
                }
            }
            JobKind::LocalTransfer { .. } => {}
            JobKind::Import { .. } => {
                let due = self
                    .engine
                    .catalog
                    .job_is_due(job, SystemClock::default().now())?;
                if due || self.scan.active_job() == Some(job) {
                    self.scan.advance(&mut self.engine, job)?;
                }
                if due || self.local_transfer.active_parent() == Some(job) {
                    self.local_transfer.advance(&mut self.engine, job)?;
                }
                let report = self.engine.report(job)?;
                if report.waiting > 0
                    && self.local_transfer.active_parent().is_none()
                    && self.engine.catalog.job(job)?.state == JobState::WaitingCapacity
                    && self.pressure_reclaim(Some(job))?
                {
                    return Ok(());
                }
                if report.conflicting > 0 {
                    self.wait(
                        job,
                        JobState::Conflict,
                        "one or more selected items have a collision; independent groups continue"
                            .into(),
                    )?;
                }
                if self.engine.scan_complete(job)?
                    && report.archive_committed == report.selected
                    && report.waiting == 0
                    && report.conflicting == 0
                {
                    self.complete(job)?;
                } else if self.upload.active_job() == Some(job)
                    || (due
                        && self.nas_job_allowed(job)
                        && report.selected > 0
                        && self.upload.active_job().is_none()
                        && self.local_transfer.active_parent() != Some(job)
                        && self.control.is_none()
                        && self.worker.available()?)
                {
                    let _ = self
                        .upload
                        .advance(&mut self.engine, &mut self.worker, job)?;
                }
            }
            JobKind::Bootstrap { .. } => {
                if !self.nas_job_allowed(job) {
                    return Ok(());
                }
                let report = BootstrapCoordinator::new(
                    &config,
                    &mut self.engine.catalog,
                    &mut self.bootstrap,
                    &mut self.worker,
                )
                .advance(job)?;
                self.result(job, report)?;
            }
            JobKind::MetadataFetch { .. } => {
                let nas_allowed = self.nas_job_allowed(job);
                self.fetch.advance(
                    &mut self.engine.catalog,
                    &config,
                    &mut self.worker,
                    &mut self.maintenance,
                    job,
                    nas_allowed,
                )?;
                if self.fetch.verified(job) {
                    self.complete(job)?;
                }
            }
            JobKind::Pin { .. } => {
                let fresh = self.pin_shared_fresh(job)?;
                let nas_allowed = self.nas_job_allowed(job);
                let readiness = self.fetch.advance(
                    &mut self.engine.catalog,
                    &config,
                    &mut self.worker,
                    &mut self.maintenance,
                    job,
                    nas_allowed,
                )?;
                self.result(job, &readiness)?;
                if fresh
                    && self.fetch.verified(job)
                    && readiness.originals_ready
                    && readiness.metadata_current
                {
                    self.complete(job)?;
                } else if !fresh && self.reader.reason.is_some() {
                    self.wait(job,JobState::WaitingNas,"originals remain usable; latest metadata waits for a fresh shared namespace read".into())?;
                } else if fresh && self.fetch.verified(job) && readiness.originals_ready {
                    self.wait(job,JobState::WaitingApplication,"originals are resident; retained sidecars require foreground refresh --editor-closed".into())?;
                }
            }
            JobKind::PinUrl { server, asset } => {
                let existing: Option<String> = self
                    .engine
                    .catalog
                    .db
                    .query_row(
                        "SELECT group_id FROM pin_targets WHERE job=?",
                        [job.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if existing.is_none() {
                    use crate::immich::ImmichAccess;
                    let Some(http) = &self.http else {
                        return self.wait(
                            job,
                            JobState::WaitingImmich,
                            self.http_reason
                                .clone()
                                .unwrap_or_else(|| "Immich is not configured".into()),
                        );
                    };
                    match http.asset(*asset) {
                        Ok(asset_info) => {
                            if http.origin().origin().ascii_serialization() != *server {
                                return Err(AppError::new(
                                    ErrorCode::Conflict,
                                    "URL origin changed",
                                ));
                            }
                            let group = crate::immich::resolve_asset(
                                &asset_info,
                                &config.immich_path_mappings,
                                &self.engine.catalog,
                            )?;
                            self.engine
                                .catalog
                                .bind_asset(http.origin(), &asset_info, group)?;
                            self.engine.catalog.pin_url_target(job, group)?;
                            let current = self.engine.catalog.job(job)?;
                            self.engine.catalog.transition(
                                job,
                                current.generation,
                                JobTransition::Start,
                            )?;
                            self.rating_cursor = Some(None);
                            self.favorite_cursor = Some(None);
                        }
                        Err(error) => {
                            return self.wait(job, JobState::WaitingImmich, error.to_string());
                        }
                    }
                }
                let fresh = self.pin_shared_fresh(job)?;
                let nas_allowed = self.nas_job_allowed(job);
                let readiness = self.fetch.advance(
                    &mut self.engine.catalog,
                    &config,
                    &mut self.worker,
                    &mut self.maintenance,
                    job,
                    nas_allowed,
                )?;
                self.result(job, &readiness)?;
                if fresh
                    && self.fetch.verified(job)
                    && readiness.originals_ready
                    && readiness.metadata_current
                {
                    self.complete(job)?;
                } else if !fresh && self.reader.reason.is_some() {
                    self.wait(job,JobState::WaitingNas,"originals remain usable; latest metadata waits for a fresh shared namespace read".into())?;
                } else if fresh && self.fetch.verified(job) && readiness.originals_ready {
                    self.wait(job,JobState::WaitingApplication,"originals are resident; use foreground refresh --editor-closed for pending metadata".into())?;
                }
            }
            JobKind::Unpin { group } => {
                self.engine.catalog.unpin(*group)?;
                self.complete(job)?;
            }
            JobKind::Refresh { groups, stage_only } => {
                self.advance_refresh(record, groups, *stage_only)?;
            }
            JobKind::Clean => {
                self.cleanup.request_clean(&mut self.engine, job)?;
            }
            JobKind::ChangeMode { request } => {
                if self.nas_job_allowed(job)
                    && self.control.is_none()
                    && self.worker.available()?
                    && self.maintenance.available()?
                {
                    let ticket = self.worker.submit(NasCommand::ChangeMode {
                        root: config.archive_root.clone(),
                        archive: config
                            .archive_id
                            .ok_or_else(|| AppError::invalid("missing archive ID"))?,
                        writer: config.writer_id,
                        request: request.clone(),
                    })?;
                    self.control = Some(Control { job, ticket });
                }
            }
            JobKind::ArchiveInit => {
                if self.nas_job_allowed(job) && self.control.is_none() && self.worker.available()? {
                    let ticket = self.worker.submit(NasCommand::Initialize {
                        root: config.archive_root.clone(),
                        request: InitArchive {
                            archive_id: config.archive_id.ok_or_else(|| {
                                AppError::invalid("profile has no archive identity")
                            })?,
                            writer_id: config.writer_id,
                        },
                    })?;
                    self.control = Some(Control { job, ticket });
                }
            }
            JobKind::RecoverLock {
                prior_owner_stopped,
                all_writers_quiescent,
            } => {
                if self.nas_job_allowed(job) && self.control.is_none() && self.worker.available()? {
                    let ticket = self.worker.submit(NasCommand::RecoverArchive {
                        root: config.archive_root.clone(),
                        archive: config
                            .archive_id
                            .ok_or_else(|| AppError::invalid("profile has no archive identity"))?,
                        writer: config.writer_id,
                        prior_owner_stopped: *prior_owner_stopped,
                        all_writers_quiescent: *all_writers_quiescent,
                    })?;
                    self.control = Some(Control { job, ticket });
                }
            }
            JobKind::Backup { destination } => {
                if self.local_control.is_none() && self.maintenance.available()? {
                    let ticket = self.maintenance.submit(NasCommand::BackupCatalog {
                        state: config.state_root.clone(),
                        destination: destination.clone(),
                    })?;
                    self.local_control = Some(Control { job, ticket });
                }
            }
            JobKind::Rebuild { .. } if !self.nas_job_allowed(job) => {}
            JobKind::Rebuild { replacement } => self.reader.start_rebuild(
                &mut self.engine,
                &mut self.maintenance,
                job,
                replacement,
            )?,
            JobKind::Verify { archive: false } => {
                self.local_audit.advance(
                    &mut self.engine.catalog,
                    &config,
                    &mut self.maintenance,
                    job,
                )?;
            }
            JobKind::Verify { archive: true } => {
                if !self.nas_job_allowed(job) {
                    return Ok(());
                }
                self.reader
                    .start_verify(&mut self.engine, &self.state, &mut self.worker, job)?
            }
            JobKind::ResolveAmbiguity { request } => {
                if self.engine.catalog.resolution_applied(request)? {
                    self.complete(job)?;
                    return Ok(());
                }
                let records = self.engine.catalog.resolution_records(request)?;
                self.engine.catalog.db.execute("INSERT INTO pending_shared_records(operation,records) VALUES(?,?) ON CONFLICT DO NOTHING",params![request.operation.to_string(),serde_json::to_string(&records)?])?;
                self.wait(
                    job,
                    JobState::WaitingNas,
                    "validated resolution awaits journal publication".into(),
                )?;
            }
            JobKind::Qualify { request } => {
                let request: crate::qualification::QualificationRequest =
                    serde_json::from_value(request.clone())?;
                crate::qualification::validate_request(&config, &request)?;
                match request {
                    crate::qualification::QualificationRequest::Nas(request) => {
                        if self.nas_job_allowed(job)
                            && self.control.is_none()
                            && self.worker.available()?
                        {
                            let ticket = self.worker.submit(NasCommand::QualifyNas(request))?;
                            self.control = Some(Control { job, ticket });
                        }
                    }
                    crate::qualification::QualificationRequest::Immich(request) => {
                        if self.local_control.is_none() && self.maintenance.available()? {
                            let ticket = self
                                .maintenance
                                .submit(NasCommand::QualifyImmich(request))?;
                            self.local_control = Some(Control { job, ticket });
                        }
                    }
                    crate::qualification::QualificationRequest::Rapidraw(request) => {
                        if self.local_control.is_none() && self.maintenance.available()? {
                            let ticket = self
                                .maintenance
                                .submit(NasCommand::QualifyEditor(request))?;
                            self.local_control = Some(Control { job, ticket });
                        }
                    }
                }
            }
            JobKind::Evict {
                group,
                revision,
                members,
                activity,
            } => {
                self.cleanup.start_eviction(
                    &mut self.engine,
                    &mut self.maintenance,
                    job,
                    EvictionIntent {
                        group: *group,
                        revision: *revision,
                        members: members.clone(),
                        operation: record.request.operation_id,
                        eligibility_activity: *activity,
                    },
                )?;
            }
            JobKind::Resolve {
                conflict, chosen, ..
            } => {
                let version = self.engine.catalog.resolve_conflict(
                    *conflict,
                    &self
                        .engine
                        .catalog
                        .heads(self.engine.catalog.version(*chosen)?.sidecar)?,
                    *chosen,
                )?;
                self.result(job, serde_json::json!({"version":version}))?;
                self.wait(
                    job,
                    JobState::WaitingNas,
                    "resolution awaits shared publication".into(),
                )?;
            }
        }
        Ok(())
    }
}
