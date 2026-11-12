use super::*;
use crate::archive::*;
use crate::{
    domain::OperationId,
    error::{AppError, AppResult, ErrorCode},
    fs::{RootHandle, RootPolicy},
};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, Read, Write},
    os::{
        fd::{AsRawFd, OwnedFd, RawFd},
        unix::process::CommandExt,
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
const MAX_LINE: usize = 16 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NasCommand {
    MeasureLocalSpace {
        local: PathBuf,
        state: PathBuf,
    },
    CaptureHistory {
        root: PathBuf,
        archive: crate::domain::ArchiveId,
        writer: crate::domain::WriterId,
        local: PathBuf,
        context: crate::edit::CaptureContext,
        ceiling: u64,
    },
    ChangeMode {
        root: PathBuf,
        archive: crate::domain::ArchiveId,
        writer: crate::domain::WriterId,
        request: crate::archive::ModeChangeRequest,
    },
    QualifyNas(crate::qualification::NasProbeRequest),
    QualifyImmich(crate::qualification::ImmichProbeRequest),
    QualifyEditor(crate::qualification::EditorProbeRequest),
    PublishLocal {
        root: PathBuf,
        receipt: TransferReceipt,
        destination: crate::domain::ArchivePath,
    },
    AuditTargets {
        root: PathBuf,
        archive: Option<crate::domain::ArchiveId>,
        writer: crate::domain::WriterId,
        targets: Vec<crate::catalog::VerificationTarget>,
    },
    ApplyRefresh {
        local: PathBuf,
        state: PathBuf,
        group: crate::domain::GroupId,
        job: crate::domain::JobId,
        session: crate::domain::ApplicationSessionId,
        operation: OperationId,
    },
    AuditLocal {
        root: PathBuf,
        objects: Vec<crate::catalog::VerifyObject>,
    },
    CaptureOwned {
        local: PathBuf,
        state: PathBuf,
        context: crate::edit::CaptureContext,
        ceiling: u64,
    },
    RecoverCapture {
        state: PathBuf,
        context: crate::edit::CaptureContext,
        ceiling: u64,
    },
    BackupCatalog {
        state: PathBuf,
        destination: PathBuf,
    },
    LocalForensicStart {
        root: PathBuf,
    },
    LocalInventoryHashNext {
        limit: usize,
    },
    LocalInventoryStart {
        root: PathBuf,
    },
    LocalInventoryNext {
        limit: usize,
    },
    WatchLocal {
        root: PathBuf,
    },
    LocalEvents,
    CaptureLocal {
        local: PathBuf,
        state: PathBuf,
        path: crate::domain::ArchivePath,
        kind: crate::edit::SidecarKind,
        ceiling: u64,
    },
    LocalTransfer {
        spec: TransferSpec,
        destination: crate::domain::ArchivePath,
        reuse_only: bool,
    },
    RecoverArchive {
        root: PathBuf,
        archive: crate::domain::ArchiveId,
        writer: crate::domain::WriterId,
        prior_owner_stopped: bool,
        all_writers_quiescent: bool,
    },
    ReadJournalStart {
        root: PathBuf,
        archive: crate::domain::ArchiveId,
        writer: crate::domain::WriterId,
    },
    ReadJournalNext {
        session: OperationId,
        limit: usize,
    },
    ReadJournalCommit {
        session: OperationId,
        path: crate::domain::ArchivePath,
    },
    ReadJournalEnd {
        session: OperationId,
    },
    AuditArchive {
        root: PathBuf,
        archive: crate::domain::ArchiveId,
        writer: crate::domain::WriterId,
        objects: Vec<crate::catalog::VerifyObject>,
    },
    InventoryStart {
        session: OperationId,
        scope: Option<crate::domain::ArchivePath>,
    },
    InventoryNext {
        session: OperationId,
        limit: usize,
    },
    InventoryMetadata {
        session: OperationId,
        paths: Vec<crate::domain::ArchivePath>,
    },
    Inspect {
        root: PathBuf,
    },
    Stage(TransferSpec),
    FetchArchive {
        archive: crate::domain::ArchiveId,
        writer: crate::domain::WriterId,
        spec: TransferSpec,
    },
    StageArchive {
        archive: crate::domain::ArchiveId,
        writer: crate::domain::WriterId,
        spec: TransferSpec,
    },
    StoreHistory {
        root: PathBuf,
        archive: crate::domain::ArchiveId,
        writer: crate::domain::WriterId,
        staged: crate::domain::ArchivePath,
        digest: crate::domain::Digest,
        size: u64,
    },
    Initialize {
        root: PathBuf,
        request: InitArchive,
    },
    BeginArchive {
        root: PathBuf,
        archive: crate::domain::ArchiveId,
        writer: crate::domain::WriterId,
    },
    JournalNext {
        session: OperationId,
        limit: usize,
    },
    JournalRead {
        session: OperationId,
        path: crate::domain::ArchivePath,
    },
    JournalAck {
        session: OperationId,
        token: OperationId,
    },
    Publish {
        session: OperationId,
        plan: PublicationPlan,
    },
    PublishVersions {
        session: OperationId,
        plan: VersionPublication,
    },
    RepairProjection {
        session: OperationId,
        commit: CommitEnvelope,
        plan: ProjectionPlan,
    },
    EndArchive {
        session: OperationId,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NasReply {
    Refreshed(crate::edit::RefreshReport),
    LocalSpace(crate::fs::ManagedSpaceMeasurement),
    Capability(crate::qualification::CapabilityReport),
    LocalObserved {
        entries: Vec<ArchiveObservation>,
        end: bool,
        skipped: u64,
    },
    LocalFiles {
        entries: Vec<LocalFile>,
        end: bool,
        skipped: u64,
    },
    LocalEvents(Vec<crate::edit::LocalEvent>),
    Captured(crate::edit::SnapshotReceipt),
    ReadSession(OperationId),
    Integrity(crate::catalog::IntegrityReport),
    Inventory(ArchiveInventoryBatch),
    Metadata(Vec<crate::import::MetadataResult>),
    Root {
        device: u64,
        free: u64,
    },
    Staged(TransferReceipt),
    Marker(ArchiveMarker),
    Session {
        session: OperationId,
        marker: ArchiveMarker,
    },
    Journal(JournalBatch),
    Commit(CommitEnvelope),
    Published(PublicationReceipt),
    Projection(ProjectionOutcome),
    Done,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalFile {
    pub path: crate::domain::ArchivePath,
    pub size: u64,
    pub role: crate::import::SourceRole,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerTicket(pub OperationId);
#[derive(Serialize, Deserialize)]
struct Message {
    ticket: WorkerTicket,
    command: NasCommand,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Reply {
    #[serde(default)]
    metrics: crate::metrics::Metrics,
    #[serde(default)]
    nas_decision: Option<crate::qualification::CapabilityDecision>,
    #[serde(default)]
    nas_exchange_decision: Option<crate::qualification::CapabilityDecision>,
    ticket: WorkerTicket,
    result: AppResult<NasReply>,
}
#[derive(Debug, Clone)]
pub enum WorkerState {
    Active,
    Completed(NasReply),
    Failed(AppError),
    TimedOutStillAlive,
    Reaped,
}
#[derive(Default, Clone, Serialize, Deserialize)]
struct WorkerSettings {
    #[serde(default)]
    profile_roots: Vec<ProfileRoot>,
    local_test_backend: bool,
    state_root: Option<PathBuf>,
    capability_evidence: Vec<PathBuf>,
}
#[derive(Clone, Serialize, Deserialize)]
struct ProfileRoot {
    path: PathBuf,
    device: u64,
    inode: u64,
    mount: u64,
}
impl WorkerSettings {
    fn configured(config: &crate::config::Config) -> AppResult<Self> {
        config.validate()?;
        let mut roots = Vec::new();
        for path in [&config.working_root, &config.state_root] {
            let root = RootHandle::open(path, RootPolicy::Local)?;
            roots.push(ProfileRoot {
                path: path.clone(),
                device: root.device(),
                inode: root.inode(),
                mount: root.mount_number(),
            });
        }
        Ok(Self {
            profile_roots: roots,
            local_test_backend: config.local_test_backend,
            state_root: Some(config.state_root.clone()),
            capability_evidence: config.capability_evidence.clone(),
        })
    }
    fn bound(
        config: &crate::config::Config,
        local: &RootHandle,
        state: &RootHandle,
    ) -> AppResult<Self> {
        local.validate_identity()?;
        state.validate_identity()?;
        if local.path() != config.working_root || state.path() != config.state_root {
            return Err(AppError::invalid("worker roots differ from profile"));
        }
        Ok(Self {
            profile_roots: [local, state]
                .into_iter()
                .map(|root| ProfileRoot {
                    path: root.path().to_owned(),
                    device: root.device(),
                    inode: root.inode(),
                    mount: root.mount_number(),
                })
                .collect(),
            local_test_backend: config.local_test_backend,
            state_root: Some(config.state_root.clone()),
            capability_evidence: config.capability_evidence.clone(),
        })
    }
    fn local_root(&self, path: &std::path::Path) -> AppResult<RootHandle> {
        let root = RootHandle::open(path, RootPolicy::Local)?;
        if let Some(expected) = self
            .profile_roots
            .iter()
            .find(|expected| expected.path == path)
            && (root.device() != expected.device
                || root.inode() != expected.inode
                || root.mount_number() != expected.mount)
        {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "profile root identity changed; management stopped before effects",
            ));
        }
        Ok(root)
    }
    fn validate_roots(&self) -> AppResult<()> {
        for expected in &self.profile_roots {
            self.local_root(&expected.path)?;
        }
        Ok(())
    }

    fn registry(&self) -> crate::qualification::CapabilityRegistry {
        let Some(state) = &self.state_root else {
            return crate::qualification::CapabilityRegistry::default();
        };
        if self.local_root(state).is_err() {
            return crate::qualification::CapabilityRegistry::default();
        }
        let config = crate::config::Config {
            state_root: state.clone(),
            capability_evidence: self.capability_evidence.clone(),
            ..Default::default()
        };
        crate::qualification::load_registry(&config).unwrap_or_default()
    }
    fn root(&self, path: &std::path::Path) -> AppResult<RootHandle> {
        let root = if self.local_test_backend {
            RootHandle::open(path, RootPolicy::TestArchive)
        } else {
            use std::os::unix::fs::MetadataExt;
            RootHandle::open(
                path,
                RootPolicy::Archive {
                    expected_device: std::fs::metadata(path)?.dev(),
                },
            )
        }?;
        for expected in &self.profile_roots {
            crate::fs::ensure_disjoint_roots(&root, &self.local_root(&expected.path)?)?;
        }
        Ok(root)
    }
}
pub struct WorkerSupervisor {
    nas_decision: Option<crate::qualification::CapabilityDecision>,
    nas_exchange_decision: Option<crate::qualification::CapabilityDecision>,
    metrics: crate::metrics::Metrics,
    child: Child,
    input: std::process::ChildStdin,
    output: std::process::ChildStdout,
    pending: Option<WorkerTicket>,
    send: Vec<u8>,
    received: Vec<u8>,
    started: Instant,
    timed_out: bool,
    _locks: Vec<OwnedFd>,
    completed: Option<(WorkerTicket, WorkerState)>,
}
fn nonblocking(fd: RawFd) -> AppResult<()> {
    // fcntl updates flags only on a valid process-owned pipe descriptor.
    let old = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if old < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // Preserve existing flags while making pipe operations bounded.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, old | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
impl WorkerSupervisor {
    pub fn new(executable: PathBuf, locks: Vec<OwnedFd>) -> AppResult<Self> {
        Self::spawn(executable, locks, "--nas-worker", WorkerSettings::default())
    }
    pub fn new_local(executable: PathBuf, locks: Vec<OwnedFd>) -> AppResult<Self> {
        Self::spawn(
            executable,
            locks,
            "--local-worker",
            WorkerSettings::default(),
        )
    }
    pub fn new_test(executable: PathBuf, locks: Vec<OwnedFd>) -> AppResult<Self> {
        Self::spawn(
            executable,
            locks,
            "--nas-worker",
            WorkerSettings {
                local_test_backend: true,
                ..Default::default()
            },
        )
    }
    pub fn new_local_configured(
        executable: PathBuf,
        locks: Vec<OwnedFd>,
        config: &crate::config::Config,
    ) -> AppResult<Self> {
        Self::spawn(
            executable,
            locks,
            "--local-worker",
            WorkerSettings::configured(config)?,
        )
    }
    pub fn new_configured(
        executable: PathBuf,
        locks: Vec<OwnedFd>,
        config: &crate::config::Config,
    ) -> AppResult<Self> {
        Self::spawn(
            executable,
            locks,
            "--nas-worker",
            WorkerSettings::configured(config)?,
        )
    }
    pub(crate) fn new_local_bound(
        executable: PathBuf,
        locks: Vec<OwnedFd>,
        config: &crate::config::Config,
        local: &RootHandle,
        state: &RootHandle,
    ) -> AppResult<Self> {
        Self::spawn(
            executable,
            locks,
            "--local-worker",
            WorkerSettings::bound(config, local, state)?,
        )
    }
    pub(crate) fn new_bound(
        executable: PathBuf,
        locks: Vec<OwnedFd>,
        config: &crate::config::Config,
        local: &RootHandle,
        state: &RootHandle,
    ) -> AppResult<Self> {
        Self::spawn(
            executable,
            locks,
            "--nas-worker",
            WorkerSettings::bound(config, local, state)?,
        )
    }
    fn spawn(
        executable: PathBuf,
        locks: Vec<OwnedFd>,
        mode: &str,
        settings: WorkerSettings,
    ) -> AppResult<Self> {
        let inherited = locks.iter().map(AsRawFd::as_raw_fd).collect::<Vec<_>>();
        let mut command = Command::new(executable);
        command
            .arg(mode)
            .arg(serde_json::to_string(&settings)?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        // pre_exec uses only async-signal-safe fcntl and does not allocate or take locks.
        unsafe {
            command.pre_exec(move || {
                for fd in &inherited {
                    if libc::fcntl(*fd, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| AppError::integrity("worker input missing"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| AppError::integrity("worker output missing"))?;
        nonblocking(input.as_raw_fd())?;
        nonblocking(output.as_raw_fd())?;
        Ok(Self {
            nas_decision: None,
            nas_exchange_decision: None,
            metrics: crate::metrics::Metrics::default(),
            child,
            input,
            output,
            pending: None,
            send: vec![],
            received: vec![],
            started: Instant::now(),
            timed_out: false,
            _locks: locks,
            completed: None,
        })
    }
    pub fn nas_decision(&self) -> Option<crate::qualification::CapabilityDecision> {
        self.nas_decision.clone()
    }
    pub fn nas_exchange_decision(&self) -> Option<crate::qualification::CapabilityDecision> {
        self.nas_exchange_decision.clone()
    }
    pub fn latest_ticket(&self) -> Option<WorkerTicket> {
        self.pending
            .or(self.completed.as_ref().map(|(ticket, _)| *ticket))
    }
    pub fn metrics(&self) -> crate::metrics::Metrics {
        self.metrics.clone()
    }
    pub fn available(&mut self) -> AppResult<bool> {
        Ok(self.pending.is_none() && !self.timed_out && self.child.try_wait()?.is_none())
    }
    pub fn mark_uncertain(&mut self) {
        self.timed_out = true;
    }
    pub fn is_uncertain(&self) -> bool {
        self.timed_out
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn submit(&mut self, command: NasCommand) -> AppResult<WorkerTicket> {
        if self.pending.is_some() || self.timed_out || self.child.try_wait()?.is_some() {
            return Err(AppError::new(
                ErrorCode::Unavailable,
                "worker busy, uncertain or reaped; no replacement publisher allowed",
            ));
        }
        let ticket = WorkerTicket(OperationId::new());
        self.send = serde_json::to_vec(&Message { ticket, command })?;
        if self.send.len() > MAX_LINE {
            return Err(AppError::invalid("worker request too large"));
        }
        self.send.push(b'\n');
        self.pending = Some(ticket);
        self.started = Instant::now();
        self.completed = None;
        if self.flush().is_err() {
            self.timed_out = true;
        }
        Ok(ticket)
    }
    fn flush(&mut self) -> AppResult<()> {
        while !self.send.is_empty() {
            match self.input.write(&self.send) {
                Ok(0) => return Err(AppError::new(ErrorCode::Unavailable, "worker pipe closed")),
                Ok(n) => {
                    self.send.drain(..n);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    pub fn timeout(&mut self, deadline: Duration) {
        if self.pending.is_some() && self.started.elapsed() >= deadline {
            self.timed_out = true;
        }
    }
    pub fn cancel(&mut self) -> AppResult<()> {
        self.timed_out = true;
        // The worker's dedicated process group includes its transfer descendants.
        if unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) } != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ESRCH) {
                return Err(e.into());
            }
        }
        Ok(())
    }
    pub fn poll(&mut self, ticket: WorkerTicket) -> AppResult<WorkerState> {
        if let Some((id, state)) = &self.completed
            && *id == ticket
        {
            return Ok(state.clone());
        }
        if self.child.try_wait()?.is_some() {
            return Ok(WorkerState::Reaped);
        }
        if self.pending != Some(ticket) {
            return Err(AppError::invalid("obsolete worker ticket"));
        }
        if self.flush().is_err() {
            self.timed_out = true;
            return Ok(WorkerState::TimedOutStillAlive);
        }
        let mut buffer = [0u8; 8192];
        loop {
            match self.output.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    self.received.extend_from_slice(&buffer[..n]);
                    if self.received.len() > MAX_LINE {
                        self.cancel()?;
                        return Err(AppError::integrity("worker response exceeds bound"));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(end) = self.received.iter().position(|b| *b == b'\n') {
            let reply: Reply = serde_json::from_slice(&self.received[..end])?;
            self.received.drain(..=end);
            if reply.ticket != ticket {
                self.cancel()?;
                return Err(AppError::integrity("worker ticket mismatch"));
            }
            self.metrics.add(&reply.metrics);
            self.nas_decision = reply.nas_decision;
            self.nas_exchange_decision = reply.nas_exchange_decision;
            let state = match reply.result {
                Ok(value) => WorkerState::Completed(value),
                Err(e) => WorkerState::Failed(e),
            };
            self.pending = None;
            self.timed_out = false;
            self.completed = Some((ticket, state.clone()));
            return Ok(state);
        }
        Ok(if self.timed_out {
            WorkerState::TimedOutStillAlive
        } else {
            WorkerState::Active
        })
    }
}
impl Drop for WorkerSupervisor {
    fn drop(&mut self) {
        let _ = self.cancel();
        let _ = self.child.try_wait();
    }
}
pub fn run_worker() -> AppResult<()> {
    run_worker_mode(false)
}
pub fn run_local_worker() -> AppResult<()> {
    run_worker_mode(true)
}
fn run_worker_mode(local_only: bool) -> AppResult<()> {
    let settings = std::env::args()
        .nth(2)
        .map(|text| serde_json::from_str::<WorkerSettings>(&text))
        .transpose()?
        .unwrap_or_default();
    let mut runtime = ArchiveRuntime {
        settings: settings.clone(),
        ..Default::default()
    };
    let mut local_runtime = LocalRuntime {
        settings,
        ..Default::default()
    };
    let input = std::io::stdin();
    let mut input = input.lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut bytes = Vec::new();
        let n = (&mut input)
            .take((MAX_LINE + 1) as u64)
            .read_until(b'\n', &mut bytes)?;
        if n == 0 {
            return Ok(());
        }
        if bytes.len() > MAX_LINE || bytes.last() != Some(&b'\n') {
            return Err(AppError::invalid("oversized/incomplete worker command"));
        }
        let message: Message = serde_json::from_slice(&bytes)?;
        let before_metrics = crate::metrics::snapshot();
        let profile_error = runtime.settings.validate_roots().err();
        let cleanup = matches!(
            &message.command,
            NasCommand::EndArchive { .. } | NasCommand::ReadJournalEnd { .. }
        );
        let result = if profile_error.is_some() && !cleanup {
            Err(profile_error.clone().unwrap())
        } else if local_only {
            local_runtime.execute(message.command)
        } else {
            match message.command {
                NasCommand::Inspect { root } => {
                    let device = std::fs::metadata(&root).map(|m| {
                        use std::os::unix::fs::MetadataExt;
                        m.dev()
                    });
                    device.map_err(Into::into).and_then(|device| {
                        let root = RootHandle::open(
                            &root,
                            RootPolicy::Archive {
                                expected_device: device,
                            },
                        )?;
                        Ok(NasReply::Root {
                            device,
                            free: crate::fs::available_bytes(&root)?,
                        })
                    })
                }
                NasCommand::Stage(spec) if runtime.settings.local_test_backend => {
                    let source = runtime.settings.local_root(&spec.source.root)?;
                    let destination = runtime.settings.root(&spec.destination_root)?;
                    TransferEngine::stage_confined(&spec, &source, &destination)
                        .map(NasReply::Staged)
                }
                NasCommand::Stage(_) => Err(AppError::new(
                    ErrorCode::Unsupported,
                    "raw worker staging requires the explicit local test backend; production uses archive-qualified staging",
                )),
                command => runtime.execute(command),
            }
        };
        if let Some(error) = profile_error {
            runtime.last_decision = Some(crate::qualification::CapabilityDecision {
                allowed: false,
                reason: error.to_string(),
            });
        }
        let nas_decision = if local_only {
            None
        } else {
            runtime.last_decision.clone().or_else(|| {
                result
                    .as_ref()
                    .err()
                    .map(|error| crate::qualification::CapabilityDecision {
                        allowed: false,
                        reason: error.to_string(),
                    })
            })
        };
        let nas_exchange_decision = match nas_decision.as_ref() {
            Some(decision) if !decision.allowed => Some(decision.clone()),
            _ if local_only => None,
            _ => runtime.last_exchange_decision.clone(),
        };
        let reply = Reply {
            nas_decision,
            nas_exchange_decision,
            metrics: crate::metrics::snapshot().difference(&before_metrics),
            ticket: message.ticket,
            result,
        };
        let mut bytes = serde_json::to_vec(&reply)?;
        bytes.push(b'\n');
        output.write_all(&bytes)?;
        output.flush()?;
    }
}

#[derive(Default)]
struct ArchiveRuntime {
    last_decision: Option<crate::qualification::CapabilityDecision>,
    last_exchange_decision: Option<crate::qualification::CapabilityDecision>,
    settings: WorkerSettings,
    readonly: Option<ReadOnlyJournal>,
    inventory: Option<ArchiveInventory>,
    store: Option<ArchiveStore>,
    guard: Option<PublicationGuard>,
}
impl ArchiveRuntime {
    fn open_store(
        &mut self,
        root: &std::path::Path,
        archive: crate::domain::ArchiveId,
        writer: crate::domain::WriterId,
    ) -> AppResult<ArchiveStore> {
        let store = ArchiveStore::open(self.settings.root(root)?, archive, writer)?
            .with_registry(self.settings.registry());
        self.last_decision = Some(store.write_decision());
        self.last_exchange_decision = Some(store.exchange_decision());
        Ok(store)
    }
    fn execute(&mut self, command: NasCommand) -> AppResult<NasReply> {
        self.last_decision = None;
        self.last_exchange_decision = None;
        let registry = self.settings.registry();
        if let Some(store) = self.store.as_mut() {
            store.set_registry(registry);
            self.last_decision = Some(store.write_decision());
            self.last_exchange_decision = Some(store.exchange_decision());
        }
        match command {
            NasCommand::CaptureHistory {
                root,
                archive,
                writer,
                local,
                context,
                ceiling,
            } => {
                let store = self.open_store(&root, archive, writer)?;
                store.require_writable()?;
                let local = self.settings.local_root(&local)?;
                let receipt = crate::edit::capture_history(&local, &store.root, &context, ceiling)?;
                store.validate()?;
                Ok(NasReply::Captured(receipt))
            }
            NasCommand::ChangeMode {
                root,
                archive,
                writer,
                request,
            } => {
                let mut store = self.open_store(&root, archive, writer)?;
                store.change_mode(request).map(NasReply::Marker)
            }
            NasCommand::QualifyNas(request) => {
                if request.local_test_backend != self.settings.local_test_backend {
                    return Err(AppError::invalid(
                        "probe backend differs from worker backend",
                    ));
                }
                crate::qualification::qualify_nas(request).map(NasReply::Capability)
            }
            NasCommand::RecoverArchive {
                root,
                archive,
                writer,
                prior_owner_stopped,
                all_writers_quiescent,
            } => {
                let mut store = self.open_store(&root, archive, writer)?;
                store.recover_lock(prior_owner_stopped, all_writers_quiescent)?;
                Ok(NasReply::Done)
            }
            NasCommand::ReadJournalStart {
                root,
                archive,
                writer,
            } => {
                let store = self.open_store(&root, archive, writer)?;
                let readonly = store.read_only_journal()?;
                let id = readonly.id;
                self.readonly = Some(readonly);
                Ok(NasReply::ReadSession(id))
            }
            NasCommand::ReadJournalNext { session, limit } => Ok(NasReply::Journal(
                self.read_session(session)?.next_batch(limit)?,
            )),
            NasCommand::ReadJournalCommit { session, path } => {
                Ok(NasReply::Commit(self.read_session(session)?.read(&path)?))
            }
            NasCommand::ReadJournalEnd { session } => {
                self.read_session(session)?;
                self.readonly = None;
                Ok(NasReply::Done)
            }
            NasCommand::AuditTargets {
                root,
                archive,
                writer,
                targets,
            } => {
                let report = if let Some(archive) = archive {
                    let store = self.open_store(&root, archive, writer)?;
                    let report =
                        crate::catalog::IntegrityReport::audit_targets(&store.root, &targets)?;
                    store.validate()?;
                    report
                } else {
                    let root = self.settings.local_root(&root)?;
                    crate::catalog::IntegrityReport::audit_targets(&root, &targets)?
                };
                Ok(NasReply::Integrity(report))
            }
            NasCommand::AuditArchive {
                root,
                archive,
                writer,
                objects,
            } => {
                let store = self.open_store(&root, archive, writer)?;
                let report = crate::catalog::IntegrityReport::audit_objects(&store.root, &objects)?;
                store.validate()?;
                Ok(NasReply::Integrity(report))
            }
            NasCommand::InventoryStart { session, scope } => {
                let (store, guard) = self.session(session)?;
                if !guard.replayed() {
                    return Err(AppError::integrity(
                        "bootstrap requires fresh journal replay",
                    ));
                }
                let inventory = ArchiveInventory::open(store, scope.as_ref())?;
                self.inventory = Some(inventory);
                Ok(NasReply::Done)
            }
            NasCommand::InventoryNext { session, limit } => {
                self.session(session)?;
                let store = self
                    .store
                    .as_ref()
                    .ok_or_else(|| AppError::integrity("no archive"))?;
                let inventory = self
                    .inventory
                    .as_mut()
                    .ok_or_else(|| AppError::invalid("inventory not started"))?;
                Ok(NasReply::Inventory(inventory.next(store, limit)?))
            }
            NasCommand::InventoryMetadata { session, paths } => {
                self.session(session)?;
                let inventory = self
                    .inventory
                    .as_ref()
                    .ok_or_else(|| AppError::invalid("inventory not started"))?;
                Ok(NasReply::Metadata(inventory.metadata(&paths)?))
            }
            NasCommand::FetchArchive {
                archive,
                writer,
                spec,
            } => {
                let store = self.open_store(&spec.source.root, archive, writer)?;
                let dest = self.settings.local_root(&spec.destination_root)?;
                let result = TransferEngine::stage_confined(&spec, &store.root, &dest)?;
                store.validate()?;
                Ok(NasReply::Staged(result))
            }
            NasCommand::StageArchive {
                archive,
                writer,
                spec,
            } => {
                let store = self.open_store(&spec.destination_root, archive, writer)?;
                store.require_writable()?;
                let source = self.settings.local_root(&spec.source.root)?;
                let result = TransferEngine::stage_confined(&spec, &source, &store.root)?;
                store.validate()?;
                Ok(NasReply::Staged(result))
            }
            NasCommand::StoreHistory {
                root,
                archive,
                writer,
                staged,
                digest,
                size,
            } => {
                let store = self.open_store(&root, archive, writer)?;
                store.store_history(&staged, &digest, size)?;
                Ok(NasReply::Done)
            }
            NasCommand::Initialize { root, request } => {
                let root = self.settings.root(&root)?;
                let registry = self.settings.registry();
                let marker = ArchiveStore::initialize_qualified(&root, request, &registry)?;
                let store = ArchiveStore::open(root, marker.archive_id, marker.writer_id)?
                    .with_registry(registry);
                self.last_decision = Some(store.write_decision());
                self.last_exchange_decision = Some(store.exchange_decision());
                Ok(NasReply::Marker(marker))
            }
            NasCommand::BeginArchive {
                root,
                archive,
                writer,
            } => {
                if self.guard.is_some() {
                    return Err(AppError::new(
                        ErrorCode::Conflict,
                        "worker already holds a publication session",
                    ));
                }
                let store = self.open_store(&root, archive, writer)?;
                let guard = store.acquire_publication()?;
                let session = guard.owner.session;
                let marker = store.marker().clone();
                self.store = Some(store);
                self.guard = Some(guard);
                Ok(NasReply::Session { session, marker })
            }
            NasCommand::JournalNext { session, limit } => {
                let (store, guard) = self.session(session)?;
                Ok(NasReply::Journal(store.journal_batch(guard, limit)?))
            }
            NasCommand::JournalRead { session, path } => {
                let (store, guard) = self.session(session)?;
                Ok(NasReply::Commit(store.read_commit(guard, &path)?))
            }
            NasCommand::JournalAck { session, token } => {
                let (store, guard) = self.session(session)?;
                store.ack_journal(guard, token)?;
                Ok(NasReply::Done)
            }
            NasCommand::Publish { session, plan } => {
                let (store, guard) = self.session(session)?;
                Ok(NasReply::Published(store.publish_originals(
                    guard,
                    plan,
                    &crate::domain::NoFaults,
                )?))
            }
            NasCommand::PublishVersions { session, plan } => {
                let (store, guard) = self.session(session)?;
                Ok(NasReply::Published(store.publish_versions(
                    guard,
                    plan,
                    &crate::domain::NoFaults,
                )?))
            }
            NasCommand::RepairProjection {
                session,
                commit,
                plan,
            } => {
                let (store, _) = self.session(session)?;
                let proof = store.committed_projection(&commit, &plan)?;
                Ok(NasReply::Projection(store.apply_projection(&proof)?))
            }
            NasCommand::EndArchive { session } => {
                self.session(session)?;
                let guard = self
                    .guard
                    .take()
                    .ok_or_else(|| AppError::integrity("session disappeared"))?;
                self.store
                    .as_ref()
                    .ok_or_else(|| AppError::integrity("archive disappeared"))?
                    .release(guard)?;
                Ok(NasReply::Done)
            }
            _ => Err(AppError::invalid("not an archive-session command")),
        }
    }
    fn read_session(&mut self, id: OperationId) -> AppResult<&mut ReadOnlyJournal> {
        self.readonly
            .as_mut()
            .filter(|journal| journal.id == id)
            .ok_or_else(|| AppError::invalid("obsolete read-only journal session"))
    }
    fn session(&mut self, id: OperationId) -> AppResult<(&ArchiveStore, &mut PublicationGuard)> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| AppError::new(ErrorCode::Conflict, "no open archive"))?;
        let guard = self
            .guard
            .as_mut()
            .ok_or_else(|| AppError::new(ErrorCode::Conflict, "no live publication session"))?;
        if guard.owner.session != id {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "retired publication session",
            ));
        }
        Ok((store, guard))
    }
}

#[derive(Default)]
struct LocalRuntime {
    settings: WorkerSettings,
    inventory: Option<crate::import::Inventory>,
    watcher: Option<crate::edit::LocalWatcher>,
}
impl LocalRuntime {
    fn execute(&mut self, command: NasCommand) -> AppResult<NasReply> {
        match command {
            NasCommand::ApplyRefresh {
                local,
                state,
                group,
                job,
                session,
                operation,
            } => {
                crate::metrics::refresh_application();
                let local = self.settings.local_root(&local)?;
                let state = self.settings.local_root(&state)?;
                let mut catalog = crate::catalog::Catalog::open(&state)?;
                let sessions = crate::edit::ApplicationSessions::worker_authority(
                    state.try_clone()?,
                    job,
                    session,
                );
                let report = crate::edit::RefreshCoordinator {
                    catalog: &mut catalog,
                    local: &local,
                    state: &state,
                    sessions: &sessions,
                }
                .apply_batch(
                    crate::edit::RefreshRequest {
                        groups: vec![group],
                        application_session: session,
                        operation_id: operation,
                    },
                    &crate::domain::NoFaults,
                    128,
                    Some(job),
                )?;
                Ok(NasReply::Refreshed(report))
            }
            NasCommand::MeasureLocalSpace { local, state } => crate::fs::measure_managed_space(
                &self.settings.local_root(&local)?,
                &self.settings.local_root(&state)?,
            )
            .map(NasReply::LocalSpace),
            NasCommand::QualifyImmich(request) => {
                crate::qualification::qualify_immich(request).map(NasReply::Capability)
            }
            NasCommand::QualifyEditor(request) => {
                crate::qualification::qualify_rapidraw(request).map(NasReply::Capability)
            }
            NasCommand::BackupCatalog { state, destination } => {
                let state = self.settings.local_root(&state)?;
                crate::catalog::backup_readonly(&state, &destination)?;
                Ok(NasReply::Done)
            }
            NasCommand::LocalForensicStart { root } => {
                self.inventory = Some(crate::import::Inventory::open_forensic(
                    self.settings.local_root(&root)?,
                )?);
                Ok(NasReply::Done)
            }
            NasCommand::LocalInventoryHashNext { limit } => {
                let inventory = self
                    .inventory
                    .as_mut()
                    .ok_or_else(|| AppError::invalid("local inventory not started"))?;
                let files = inventory.next_batch(limit)?;
                let end = files.is_empty();
                let entries = files
                    .into_iter()
                    .map(|entry| {
                        Ok(ArchiveObservation {
                            digest: crate::import::hash_source(&entry)?,
                            path: entry.path,
                            size: entry.size,
                            role: entry.role,
                        })
                    })
                    .collect::<AppResult<Vec<_>>>()?;
                Ok(NasReply::LocalObserved {
                    entries,
                    end,
                    skipped: inventory.skipped,
                })
            }
            NasCommand::PublishLocal {
                root,
                receipt,
                destination,
            } => {
                let root = self.settings.local_root(&root)?;
                if let Some((parent, _)) = destination.as_str().rsplit_once('/') {
                    root.mkdir_all(&crate::domain::ArchivePath::parse(parent)?)?;
                }
                crate::transfer::publish_local(&root, &receipt, &destination)?;
                Ok(NasReply::Staged(receipt))
            }
            NasCommand::AuditTargets {
                root,
                archive: None,
                targets,
                ..
            } => {
                let root = self.settings.local_root(&root)?;
                Ok(NasReply::Integrity(
                    crate::catalog::IntegrityReport::audit_targets(&root, &targets)?,
                ))
            }
            NasCommand::AuditLocal { root, objects } => {
                let root = self.settings.local_root(&root)?;
                Ok(NasReply::Integrity(
                    crate::catalog::IntegrityReport::audit_objects(&root, &objects)?,
                ))
            }
            NasCommand::CaptureOwned {
                local,
                state,
                context,
                ceiling,
            } => {
                let local = self.settings.local_root(&local)?;
                let state = self.settings.local_root(&state)?;
                Ok(NasReply::Captured(crate::edit::capture_owned(
                    &local, &state, &context, ceiling,
                )?))
            }
            NasCommand::RecoverCapture {
                state,
                context,
                ceiling,
            } => {
                let state = self.settings.local_root(&state)?;
                Ok(NasReply::Captured(crate::edit::recover_capture(
                    &state, &context, ceiling,
                )?))
            }
            NasCommand::CaptureLocal {
                local,
                state,
                path,
                kind,
                ceiling,
            } => {
                let local = self.settings.local_root(&local)?;
                let state = self.settings.local_root(&state)?;
                Ok(NasReply::Captured(crate::edit::capture_local(
                    &local, &state, &path, kind, ceiling,
                )?))
            }
            NasCommand::LocalTransfer {
                spec,
                destination,
                reuse_only,
            } => {
                let root = self.settings.local_root(&spec.destination_root)?;
                if let Some(receipt) =
                    TransferEngine::reuse_local_destination(&spec, &root, &destination)?
                {
                    return Ok(NasReply::Staged(receipt));
                }
                if reuse_only {
                    return Err(AppError::new(
                        ErrorCode::SourceChanged,
                        "reuse destination vanished; no copy was reserved",
                    ));
                }
                let source = self.settings.local_root(&spec.source.root)?;
                let receipt = TransferEngine::stage_confined(&spec, &source, &root)?;
                if let Some((parent, _)) = destination.as_str().rsplit_once('/') {
                    root.mkdir_all(&crate::domain::ArchivePath::parse(parent)?)?;
                }
                crate::transfer::publish_local(&root, &receipt, &destination)?;
                Ok(NasReply::Staged(receipt))
            }
            NasCommand::LocalInventoryStart { root } => {
                self.inventory = Some(crate::import::Inventory::open_all(
                    self.settings.local_root(&root)?,
                    None,
                )?);
                Ok(NasReply::Done)
            }
            NasCommand::LocalInventoryNext { limit } => {
                let inventory = self
                    .inventory
                    .as_mut()
                    .ok_or_else(|| AppError::invalid("local inventory not started"))?;
                let entries = inventory.next_batch(limit)?;
                let end = entries.is_empty();
                let entries = entries
                    .into_iter()
                    .map(|e| LocalFile {
                        path: e.path,
                        size: e.size,
                        role: e.role,
                    })
                    .collect();
                Ok(NasReply::LocalFiles {
                    entries,
                    end,
                    skipped: inventory.skipped,
                })
            }
            NasCommand::WatchLocal { root } => {
                let root = self.settings.local_root(&root)?;
                self.watcher = Some(crate::edit::LocalWatcher::open(&root)?);
                Ok(NasReply::Done)
            }
            NasCommand::LocalEvents => Ok(NasReply::LocalEvents(
                self.watcher
                    .as_mut()
                    .ok_or_else(|| AppError::invalid("local watcher not started"))?
                    .poll()?,
            )),
            _ => Err(AppError::invalid(
                "local worker refuses archive/network commands",
            )),
        }
    }
}
