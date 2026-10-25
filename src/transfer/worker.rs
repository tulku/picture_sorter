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
    Inspect {
        root: PathBuf,
    },
    Stage(TransferSpec),
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
    EndArchive {
        session: OperationId,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NasReply {
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
    Done,
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
pub struct WorkerSupervisor {
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
        let inherited = locks.iter().map(AsRawFd::as_raw_fd).collect::<Vec<_>>();
        let mut command = Command::new(executable);
        command
            .arg("--nas-worker")
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
        self.flush()?;
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
        if let Some((id, state)) = &self.completed {
            if *id == ticket {
                return Ok(state.clone());
            }
        }
        if self.child.try_wait()?.is_some() {
            return Ok(WorkerState::Reaped);
        }
        if self.pending != Some(ticket) {
            return Err(AppError::invalid("obsolete worker ticket"));
        }
        if self.timed_out {
            return Ok(WorkerState::TimedOutStillAlive);
        }
        self.flush()?;
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
            let state = match reply.result {
                Ok(value) => WorkerState::Completed(value),
                Err(e) => WorkerState::Failed(e),
            };
            self.pending = None;
            self.completed = Some((ticket, state.clone()));
            return Ok(state);
        }
        Ok(WorkerState::Active)
    }
}
impl Drop for WorkerSupervisor {
    fn drop(&mut self) {
        let _ = self.cancel();
        let _ = self.child.try_wait();
    }
}
pub fn run_worker() -> AppResult<()> {
    let mut runtime = ArchiveRuntime::default();
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
        let result = match message.command {
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
            NasCommand::Stage(spec) => TransferEngine::stage(&spec).map(NasReply::Staged),
            command => runtime.execute(command),
        };
        let reply = Reply {
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
    store: Option<ArchiveStore>,
    guard: Option<PublicationGuard>,
}
impl ArchiveRuntime {
    fn execute(&mut self, command: NasCommand) -> AppResult<NasReply> {
        match command {
            NasCommand::StageArchive {
                archive,
                writer,
                spec,
            } => {
                use std::os::unix::fs::MetadataExt;
                let device = std::fs::metadata(&spec.destination_root)?.dev();
                let store = ArchiveStore::open(
                    RootHandle::open(
                        &spec.destination_root,
                        RootPolicy::Archive {
                            expected_device: device,
                        },
                    )?,
                    archive,
                    writer,
                )?;
                let result = TransferEngine::stage(&spec)?;
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
                use std::os::unix::fs::MetadataExt;
                let device = std::fs::metadata(&root)?.dev();
                let store = ArchiveStore::open(
                    RootHandle::open(
                        &root,
                        RootPolicy::Archive {
                            expected_device: device,
                        },
                    )?,
                    archive,
                    writer,
                )?;
                store.store_history(&staged, &digest, size)?;
                Ok(NasReply::Done)
            }
            NasCommand::Initialize { root, request } => {
                let device = std::fs::metadata(&root).map(|m| {
                    use std::os::unix::fs::MetadataExt;
                    m.dev()
                })?;
                let root = RootHandle::open(
                    &root,
                    RootPolicy::Archive {
                        expected_device: device,
                    },
                )?;
                Ok(NasReply::Marker(ArchiveStore::initialize(&root, request)?))
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
                use std::os::unix::fs::MetadataExt;
                let device = std::fs::metadata(&root)?.dev();
                let store = ArchiveStore::open(
                    RootHandle::open(
                        &root,
                        RootPolicy::Archive {
                            expected_device: device,
                        },
                    )?,
                    archive,
                    writer,
                )?;
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
