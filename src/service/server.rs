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
    let maintenance_worker = WorkerSupervisor::new_local(
        std::env::current_exe()?,
        vec![profile.inherit()?, work.inherit()?],
    )?;
    let observation_worker = WorkerSupervisor::new_local(
        std::env::current_exe()?,
        vec![profile.inherit()?, work.inherit()?],
    )?;
    let local_worker = WorkerSupervisor::new_local(
        std::env::current_exe()?,
        vec![profile.inherit()?, work.inherit()?],
    )?;
    let worker = WorkerSupervisor::new(
        std::env::current_exe()?,
        vec![profile.inherit()?, work.inherit()?],
    )?;
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let _socket_owner = SocketOwner {
        inode: std::fs::symlink_metadata(&socket)?.ino(),
        path: socket,
    };
    let (http, http_reason) = match super::http::AsyncImmich::new(&config) {
        Ok(http) => (http, None),
        Err(error) => (None, Some(error.to_string())),
    };
    let mut runtime = Runtime {
        http,
        maintenance: maintenance_worker,
        startup: super::recovery::StartupVerifier::default(),
        reader: super::archive_reader::ArchiveReader::default(),
        http_reason,
        rating_cursor: None,
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
        archive_status: serde_json::json!({"availability":"unobserved","identity":null}),
        last_tick: Instant::now(),
    };
    runtime.engine.bind_archive()?;
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
                                    match runtime.sessions.authorize(true, request.no_wait) {
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
            if peer.opened.elapsed() > Duration::from_secs(25)
                && peer.waiting.is_some()
                && let (Some(job), Some(request)) = (peer.waiting, peer.request.as_ref())
            {
                let response = runtime.job_response(request.request_id, job)?;
                peer.respond(response)?;
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
struct Control {
    job: JobId,
    ticket: WorkerTicket,
}
struct Runtime {
    engine: ImportCoordinator,
    maintenance: WorkerSupervisor,
    startup: super::recovery::StartupVerifier,
    reader: super::archive_reader::ArchiveReader,
    http: Option<super::http::AsyncImmich>,
    http_reason: Option<String>,
    rating_cursor: Option<Option<uuid::Uuid>>,
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
                serde_json::json!({"dry_run":true,"command":request.command,"effect":"no jobs, pins, reservations or archive markers created"}),
            );
        }
        match &request.command {
            Command::Status => {
                let mut status = StatusSnapshot::collect(
                    &self.engine,
                    &self.local,
                    &self.state,
                    serde_json::json!({"pid":self.worker.pid(),"local_pid":self.local_transfer.worker.pid(),"observation_pid":self.observation.worker.pid(),"maintenance_pid":self.maintenance.pid(),"available":self.worker.available()?}),
                    self.archive_status.clone(),
                )?;
                status.watcher = serde_json::to_value(&self.observation.status)?;
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
                let groups = if groups.is_empty() {
                    self.engine
                        .catalog
                        .group_batch(None, 128)?
                        .into_iter()
                        .filter(|g| {
                            self.engine
                                .catalog
                                .readiness(g.id)
                                .is_ok_and(|r| r.originals_ready)
                        })
                        .map(|g| g.id)
                        .collect()
                } else {
                    groups
                        .iter()
                        .map(|g| self.engine.catalog.canonical_group(*g))
                        .collect::<AppResult<Vec<_>>>()?
                };
                let outcome = self.enqueue(request, JobKind::Refresh { groups })?;
                if let (ResponseOutcome::Accepted { job }, Some(session)) =
                    (&outcome, authorization)
                {
                    self.authorizations.insert(*job, session);
                }
                Ok(outcome)
            }
            Command::Clean => self.enqueue(request, JobKind::Clean),
            Command::JobShow { job } => {
                let response = self.job_response(request.request_id, *job)?;
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
            Command::ConflictsList => {
                data(serde_json::to_value(self.engine.catalog.conflicts(false)?)?)
            }
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
            Command::Doctor => data(
                serde_json::json!({"protocol_version":1,"profile":self.engine.config.profile_id,"archive_probe":"unrun: doctor performs no archive writes or implicit probes","tools":{"rsync":tool_version("rsync","--version"),"exiftool":tool_version("exiftool","-ver")},"personal_integrations":"unverified"}),
            ),
            Command::Qualify { request: probe } => self.enqueue(
                request,
                JobKind::Qualify {
                    request: probe.clone(),
                },
            ),
        }
    }
    fn job_response(&self, id: OperationId, job: JobId) -> AppResult<Response> {
        let record = self.engine.catalog.job(job)?;
        let detail = match record.request.kind {
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
        }
        if timing.poll_ratings {
            self.reader.requested = true;
        }

        if let Some(http) = &self.http
            && let Err(error) = http.pump()
        {
            self.http_reason = Some(error.to_string());
        }

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
        self.startup
            .advance(&mut self.engine, &mut self.maintenance)?;
        self.worker.timeout(Duration::from_secs(5));
        if (self.reader.busy()
            || (self.control.is_none()
                && self.upload.active_job().is_none()
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
                    self.result(control.job, serde_json::to_value(reply)?)?;
                    self.complete(control.job)?;
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
                    self.result(control.job, serde_json::to_value(&reply)?)?;
                    self.complete(control.job)?;
                }
                WorkerState::Failed(error) => {
                    self.wait(control.job, JobState::WaitingNas, error.to_string())?
                }
                WorkerState::Reaped => self.wait(
                    control.job,
                    JobState::WaitingNas,
                    "worker disappeared; recovery is required".into(),
                )?,
            }
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
                self.wait(record.id, state, error.to_string())?;
            }
            let current = self.engine.catalog.job(record.id)?;
            if was_due
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
        if let Some(http) = &self.http {
            if let Some(after) = self.rating_cursor {
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
            let intent:Option<String>=self.engine.catalog.db.query_row("SELECT id FROM rating_intents WHERE state IN('prepared','sent') ORDER BY rowid LIMIT 1",[],|r|r.get(0)).optional()?;
            if let Some(id) = intent {
                let id = serde_json::from_str(&format!("\"{id}\""))?;
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
            }
        }
        if self.shared_publisher.busy()
            || (self.control.is_none()
                && self.upload.active_job().is_none()
                && self.worker.available()?)
        {
            self.shared_publisher
                .advance(&mut self.engine, &self.state, &mut self.worker)?;
        }
        Ok(())
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
                let report = BootstrapCoordinator::new(
                    &config,
                    &mut self.engine.catalog,
                    &mut self.bootstrap,
                    &mut self.worker,
                )
                .advance(job)?;
                self.result(job, report)?;
            }
            JobKind::Pin { .. } => {
                let fresh = self.pin_shared_fresh(job)?;
                let readiness = self.fetch.advance(
                    &mut self.engine.catalog,
                    &config,
                    &mut self.worker,
                    &mut self.maintenance,
                    job,
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
                        }
                        Err(error) => {
                            return self.wait(job, JobState::WaitingImmich, error.to_string());
                        }
                    }
                }
                let fresh = self.pin_shared_fresh(job)?;
                let readiness = self.fetch.advance(
                    &mut self.engine.catalog,
                    &config,
                    &mut self.worker,
                    &mut self.maintenance,
                    job,
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
            JobKind::Refresh { groups } => {
                let Some(session) = self
                    .authorizations
                    .get(&job)
                    .copied()
                    .filter(|s| self.sessions.authorized(*s))
                else {
                    return self.wait(job,JobState::WaitingApplication,"a fresh foreground refresh --editor-closed is required before live metadata application".into());
                };
                let report = RefreshCoordinator {
                    catalog: &mut self.engine.catalog,
                    local: &self.local,
                    state: &self.state,
                    sessions: &self.sessions,
                }
                .apply(
                    RefreshRequest {
                        groups: groups.clone(),
                        application_session: session,
                        operation_id: record.request.operation_id,
                    },
                    &NoFaults,
                )?;
                self.result(job,serde_json::json!({"applied":report.applied,"conflicts":report.conflicts,"waiting_application":report.waiting_application}))?;
                if !report.conflicts.is_empty() {
                    self.authorizations.remove(&job);
                    self.wait(
                        job,
                        JobState::Conflict,
                        "local edits diverged during refresh; bytes preserved".into(),
                    )?;
                } else if report.waiting_application {
                    self.authorizations.remove(&job);
                    self.wait(
                        job,
                        JobState::WaitingApplication,
                        "refresh authorization expired".into(),
                    )?;
                } else {
                    self.complete(job)?;
                    self.authorizations.remove(&job);
                }
            }
            JobKind::Clean => {
                let intents = EvictionPlanner::candidates(
                    &self.engine.catalog,
                    SystemClock::default().now(),
                )?;
                let mut removed = 0;
                for intent in intents {
                    removed += Evictor {
                        catalog: &mut self.engine.catalog,
                        local: &self.local,
                    }
                    .execute(intent, &NoFaults)?
                    .removed;
                }
                self.result(job, serde_json::json!({"originals_removed":removed}))?;
                self.complete(job)?;
                SnapshotGc { state: &self.state }.collect(&mut self.engine.catalog)?;
            }
            JobKind::ArchiveInit => {
                if self.control.is_none() && self.worker.available()? {
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
                if self.control.is_none() && self.worker.available()? {
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
            JobKind::Rebuild { replacement } => self.reader.start_rebuild(
                &mut self.engine,
                &mut self.maintenance,
                job,
                replacement,
            )?,
            JobKind::Verify { archive: false } => {
                let report = IntegrityVerifier::new(&self.engine.catalog, &self.local)
                    .audit(VerifyScope::Originals)?;
                self.engine.catalog.protect_integrity(&report)?;
                let bad = !report.findings.is_empty();
                self.result(job, report)?;
                if bad {
                    self.wait(
                        job,
                        JobState::Conflict,
                        "integrity findings require review; no repairs performed".into(),
                    )?;
                } else {
                    self.complete(job)?;
                }
            }
            JobKind::Verify { archive: true } => {
                self.reader
                    .start_verify(&mut self.engine, &self.state, &mut self.worker, job)?
            }
            JobKind::ResolveAmbiguity { request } => {
                let records = self.engine.catalog.resolution_records(request)?;
                self.engine.catalog.db.execute("INSERT INTO pending_shared_records(operation,records) VALUES(?,?) ON CONFLICT DO NOTHING",params![request.operation.to_string(),serde_json::to_string(&records)?])?;
                self.wait(
                    job,
                    JobState::WaitingNas,
                    "validated resolution awaits journal publication".into(),
                )?;
            }
            JobKind::Qualify { .. } => {
                return Err(AppError::new(
                    ErrorCode::Unavailable,
                    "explicit qualification request awaits probe execution",
                ));
            }
            JobKind::Evict {
                group,
                revision,
                members,
                activity,
            } => {
                Evictor {
                    catalog: &mut self.engine.catalog,
                    local: &self.local,
                }
                .execute(
                    EvictionIntent {
                        group: *group,
                        revision: *revision,
                        members: members.clone(),
                        operation: record.request.operation_id,
                        eligibility_activity: *activity,
                    },
                    &NoFaults,
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
fn tool_version(name: &str, flag: &str) -> serde_json::Value {
    match std::process::Command::new(name).arg(flag).output() {
        Ok(out) => {
            serde_json::json!({"available":out.status.success(),"version":String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("")})
        }
        Err(_) => serde_json::json!({"available":false}),
    }
}
