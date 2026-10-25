use crate::{
    archive::ProjectionPlan,
    catalog::Catalog,
    domain::*,
    error::{AppError, AppResult},
    fs::*,
};
use rusqlite::params;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Default)]
pub struct ApplicationSessions {
    sessions: Mutex<HashMap<ApplicationSessionId, Weak<Authorization>>>,
    remote: Option<(RootHandle, JobId, ApplicationSessionId)>,
}
pub struct ApplicationSession {
    id: ApplicationSessionId,
    authorized: Arc<Authorization>,
}
struct Authorization {
    flag: AtomicBool,
    peer: Option<std::os::unix::net::UnixStream>,
}
impl ApplicationSession {
    pub fn id(&self) -> ApplicationSessionId {
        self.id
    }
}
impl Drop for ApplicationSession {
    fn drop(&mut self) {
        self.authorized.flag.store(false, Ordering::SeqCst);
    }
}
impl ApplicationSessions {
    pub(crate) fn worker_authority(
        state: RootHandle,
        job: JobId,
        session: ApplicationSessionId,
    ) -> Self {
        Self {
            remote: Some((state, job, session)),
            ..Self::default()
        }
    }
    pub fn authorize(&self, editor_closed: bool, no_wait: bool) -> AppResult<ApplicationSession> {
        self.authorize_bound(editor_closed, no_wait, None)
    }
    pub fn authorize_peer(
        &self,
        editor_closed: bool,
        no_wait: bool,
        peer: &std::os::unix::net::UnixStream,
    ) -> AppResult<ApplicationSession> {
        self.authorize_bound(editor_closed, no_wait, Some(peer.try_clone()?))
    }
    fn authorize_bound(
        &self,
        editor_closed: bool,
        no_wait: bool,
        peer: Option<std::os::unix::net::UnixStream>,
    ) -> AppResult<ApplicationSession> {
        if !editor_closed || no_wait {
            return Err(AppError::invalid(
                "applying refresh requires a foreground editor-closed assertion",
            ));
        }
        let id = ApplicationSessionId::new();
        let authorized = Arc::new(Authorization {
            flag: AtomicBool::new(true),
            peer,
        });
        let mut map = self
            .sessions
            .lock()
            .map_err(|_| AppError::integrity("application session registry unavailable"))?;
        map.retain(|_, v| v.strong_count() > 0);
        map.insert(id, Arc::downgrade(&authorized));
        Ok(ApplicationSession { id, authorized })
    }
    pub fn authorized(&self, id: ApplicationSessionId) -> bool {
        if let Some((state, job, session)) = &self.remote {
            if *session != id {
                return false;
            }
            let check = || -> AppResult<bool> {
                use crate::service::{Client, protocol::*};
                let response = Client::connect_state(state)?.send(Request {
                    protocol_version: PROTOCOL_VERSION,
                    request_id: OperationId::new(),
                    command: Command::CheckApplication {
                        job: *job,
                        session: *session,
                    },
                    dry_run: false,
                    no_wait: false,
                    wait_for: Milestone::Ingested,
                })?;
                Ok(
                    matches!(response.outcome,ResponseOutcome::Data{data} if data["authorized"]==true),
                )
            };
            return check().unwrap_or(false);
        }
        self.sessions
            .lock()
            .ok()
            .and_then(|s| s.get(&id).and_then(Weak::upgrade))
            .is_some_and(|s| {
                if !s.flag.load(Ordering::SeqCst) {
                    return false;
                }
                let Some(peer) = &s.peer else {
                    return true;
                };
                use std::os::fd::AsRawFd;
                let mut byte = 0u8;
                let result = unsafe {
                    libc::recv(
                        peer.as_raw_fd(),
                        (&mut byte as *mut u8).cast(),
                        1,
                        libc::MSG_PEEK | libc::MSG_DONTWAIT,
                    )
                };
                result > 0
                    || (result < 0
                        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock)
            })
    }
    pub fn revoke(&self, id: ApplicationSessionId) {
        if let Ok(sessions) = self.sessions.lock()
            && let Some(state) = sessions.get(&id).and_then(Weak::upgrade)
        {
            state.flag.store(false, Ordering::SeqCst);
        }
    }
}
pub struct RefreshRequest {
    pub groups: Vec<GroupId>,
    pub application_session: ApplicationSessionId,
    pub operation_id: OperationId,
}
#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RefreshReport {
    pub applied: Vec<VersionId>,
    pub conflicts: Vec<SidecarId>,
    pub waiting_application: bool,
    #[serde(default)]
    pub has_more: bool,
}
pub struct RefreshCoordinator<'a> {
    pub catalog: &'a mut Catalog,
    pub local: &'a RootHandle,
    pub state: &'a RootHandle,
    pub sessions: &'a ApplicationSessions,
}
impl RefreshCoordinator<'_> {
    pub fn stage(&mut self, version: VersionId, bytes: &[u8]) -> AppResult<()> {
        let version = self.catalog.version(version)?;
        let expected = VersionBody::Content {
            digest: Digest::of(bytes),
            size: bytes.len() as u64,
        };
        if version.body != expected {
            return Err(AppError::integrity(
                "fetched metadata differs from committed version",
            ));
        }
        let VersionBody::Content { digest, size } = expected else {
            return Err(AppError::integrity("content required"));
        };
        self.state
            .mkdir_all(&ArchivePath::parse(".photo-sorter/snapshots")?)?;
        let target = ArchivePath::parse(&format!(
            ".photo-sorter/snapshots/{}",
            &digest.as_str()[7..]
        ))?;
        if let Err(error) = self.state.write_new(&target, bytes)
            && self.state.read(&target, size)? != bytes
        {
            return Err(error);
        }
        self.catalog.db.execute("INSERT INTO snapshots(digest,path,size,archived) VALUES(?,?,?,1) ON CONFLICT(digest) DO UPDATE SET archived=1",params![digest.as_str(),target.as_str(),size])?;
        Ok(())
    }
    pub fn apply(
        &mut self,
        request: RefreshRequest,
        faults: &dyn FaultInjector,
    ) -> AppResult<RefreshReport> {
        self.apply_batch(request, faults, usize::MAX, None)
    }
    pub(crate) fn apply_batch(
        &mut self,
        request: RefreshRequest,
        faults: &dyn FaultInjector,
        limit: usize,
        job: Option<JobId>,
    ) -> AppResult<RefreshReport> {
        self.local.require_local()?;
        self.state.require_local()?;
        let mut report = RefreshReport::default();
        if !self.sessions.authorized(request.application_session) {
            report.waiting_application = true;
            return Ok(report);
        }
        for group in request.groups {
            let group = self.catalog.canonical_group(group)?;
            let mut q=self.catalog.db.prepare("SELECT p.committed FROM projections p JOIN sidecars s ON s.id=p.sidecar WHERE s.group_id=? AND p.pending=1 AND p.committed IS NOT NULL ORDER BY p.sidecar LIMIT ?")?;
            let plans = q
                .query_map(
                    params![
                        group.to_string(),
                        limit.saturating_add(1).min(i64::MAX as usize) as i64
                    ],
                    |r| r.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            drop(q);
            for encoded in plans {
                if report.applied.len() >= limit {
                    report.has_more = true;
                    return Ok(report);
                }
                if !self.sessions.authorized(request.application_session) {
                    report.waiting_application = true;
                    return Ok(report);
                }
                let plan: ProjectionPlan = serde_json::from_str(&encoded)?;
                if !crate::archive::owns_projection(&plan.owner, &plan.path) {
                    continue;
                }
                let observed = observed_body(self.local, &plan.path)?;
                let local_head = self.catalog.local_head(plan.sidecar)?;
                let expected_local = match local_head {
                    Some(version) => self.catalog.version(version)?.body,
                    None => VersionBody::Reset,
                };
                if local_head.is_some()
                    && self.catalog.snapshot_dirty(plan.sidecar)?
                    && observed != plan.body
                {
                    self.sessions.revoke(request.application_session);
                    report.conflicts.push(plan.sidecar);
                    return Ok(report);
                }

                if observed != expected_local && observed != plan.body {
                    self.sessions.revoke(request.application_session);
                    report.conflicts.push(plan.sidecar);
                    self.catalog.db.execute(
                        "UPDATE residency SET conflict=1,protected=1 WHERE group_id=?",
                        [group.to_string()],
                    )?;
                    return Ok(report);
                }
                if let VersionBody::Content { digest, size } = &observed {
                    let mut file = self.local.open_relative(&plan.path, libc::O_RDONLY, 0)?;
                    let root = self.state;
                    root.mkdir_all(&ArchivePath::parse(".photo-sorter/snapshots")?)?;
                    let target = ArchivePath::parse(&format!(
                        ".photo-sorter/snapshots/{}",
                        &digest.as_str()[7..]
                    ))?;
                    root.mkdir_all(&ArchivePath::parse(".photo-sorter/refresh")?)?;
                    let temporary = ArchivePath::parse(&format!(
                        ".photo-sorter/refresh/preserved-{}",
                        OperationId::new()
                    ))?;
                    copy_exact(&mut file, &mut root.create_new(&temporary)?, digest, *size)?;
                    if publish_noreplace(root, &temporary, &target)? == PublishOutcome::Exists {
                        let mut existing =
                            root.open_relative(&target, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
                        if crate::transfer::digest_file(&mut existing)? != (digest.clone(), *size) {
                            return Err(AppError::integrity(
                                "preserved refresh snapshot differs from its immutable identity",
                            ));
                        }
                        root.unlink(&temporary, false)?;
                    }
                    self.catalog.db.execute("INSERT INTO snapshots(digest,path,size) VALUES(?,?,?) ON CONFLICT DO NOTHING",params![digest.as_str(),target.as_str(),size])?;
                }
                let staged = ArchivePath::parse(&format!(
                    ".photo-sorter/refresh/{}-{}",
                    request.operation_id, plan.sidecar
                ))?;
                self.local
                    .mkdir_all(&ArchivePath::parse(".photo-sorter/refresh")?)?;
                if let VersionBody::Content { digest, size } = &plan.body {
                    let source = ArchivePath::parse(&format!(
                        ".photo-sorter/snapshots/{}",
                        &digest.as_str()[7..]
                    ))?;
                    let mut history = self.state.open_relative(&source, libc::O_RDONLY, 0)?;
                    if crate::transfer::digest_file(&mut history)? != (digest.clone(), *size) {
                        return Err(AppError::integrity("incoming snapshot corrupt"));
                    }
                    use std::io::{Seek, SeekFrom};
                    history.seek(SeekFrom::Start(0))?;
                    let mut file = self.local.create_new(&staged)?;
                    copy_exact(&mut history, &mut file, digest, *size)?;
                }
                self.catalog.db.execute("INSERT INTO observations(path,tool_digest) VALUES(?,?) ON CONFLICT(path) DO UPDATE SET tool_digest=excluded.tool_digest",params![plan.path.as_str(),serde_json::to_string(&plan.body)?])?;
                faults.hit(FaultPoint::BeforeProjection)?;
                if !self.sessions.authorized(request.application_session) {
                    report.waiting_application = true;
                    return Ok(report);
                }
                if observed != plan.body {
                    match &plan.body {
                        VersionBody::Reset => {
                            let displaced = ArchivePath::parse(&format!(
                                ".photo-sorter/refresh/reset-{}",
                                OperationId::new()
                            ))?;
                            publish_noreplace(self.local, &plan.path, &displaced)?;
                            if observed_body(self.local, &displaced)? != expected_local {
                                self.sessions.revoke(request.application_session);
                                report.conflicts.push(plan.sidecar);
                                return Ok(report);
                            }
                        }
                        VersionBody::Content { .. } => {
                            if observed == VersionBody::Reset {
                                if publish_noreplace(self.local, &staged, &plan.path)?
                                    != PublishOutcome::Published
                                {
                                    self.sessions.revoke(request.application_session);
                                    report.conflicts.push(plan.sidecar);
                                    return Ok(report);
                                }
                            } else {
                                self.local.exchange(&staged, &plan.path)?;
                                if observed_body(self.local, &staged)? != expected_local {
                                    self.sessions.revoke(request.application_session);
                                    report.conflicts.push(plan.sidecar);
                                    return Ok(report);
                                }
                            }
                        }
                    }
                }
                let tx = self.catalog.db.transaction()?;
                tx.execute(
                    "UPDATE projections SET pending=0,applied=? WHERE sidecar=? AND committed=?",
                    params![plan.version.to_string(), plan.sidecar.to_string(), encoded],
                )?;
                tx.execute("INSERT INTO local_heads(sidecar,version,dirty) VALUES(?,?,0) ON CONFLICT(sidecar) DO UPDATE SET version=excluded.version,dirty=0",params![plan.sidecar.to_string(),plan.version.to_string()])?;
                tx.execute("UPDATE sidecars SET protected=0 WHERE id=? AND NOT EXISTS(SELECT 1 FROM sidecar_diagnostics WHERE sidecar=?)",params![plan.sidecar.to_string(),plan.sidecar.to_string()])?;
                if let Some(job) = job {
                    tx.execute("INSERT INTO refresh_application_receipts(job,sidecar,version) VALUES(?,?,?) ON CONFLICT(job,sidecar,version) DO NOTHING",params![job.to_string(),plan.sidecar.to_string(),plan.version.to_string()])?;
                }
                tx.commit()?;
                report.applied.push(plan.version);
            }
            self.catalog.refresh_protection(group)?;
            self.catalog.db.execute("UPDATE residency SET metadata_current=NOT EXISTS(SELECT 1 FROM sidecars s LEFT JOIN projections p ON p.sidecar=s.id LEFT JOIN local_heads l ON l.sidecar=s.id WHERE s.group_id=? AND s.owner<>'jpeg-xmp-external' AND (p.pending=1 OR p.sidecar IS NULL OR l.version IS NULL OR l.dirty=1 OR (SELECT count(*) FROM heads h WHERE h.sidecar=s.id)<>1 OR NOT EXISTS(SELECT 1 FROM heads h WHERE h.sidecar=s.id AND h.version=l.version))) WHERE group_id=?",params![group.to_string(),group.to_string()])?;
        }
        Ok(report)
    }
}
fn observed_body(root: &RootHandle, path: &ArchivePath) -> AppResult<VersionBody> {
    match root.open_relative(path, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(mut file) => {
            let (digest, size) = crate::transfer::digest_file(&mut file)?;
            Ok(VersionBody::Content { digest, size })
        }
        Err(e) if e.errno == Some(libc::ENOENT) => Ok(VersionBody::Reset),
        Err(e) => Err(e),
    }
}

fn copy_exact(
    source: &mut std::fs::File,
    destination: &mut std::fs::File,
    digest: &Digest,
    size: u64,
) -> AppResult<()> {
    use std::io::{Read, Write};
    let mut remaining = size
        .checked_add(1)
        .ok_or_else(|| AppError::invalid("refresh copy size overflow"))?;
    let mut hasher = blake3::Hasher::new();
    let mut copied = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    while remaining > 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let n = source.read(&mut buffer[..limit])?;
        if n == 0 {
            break;
        }
        destination.write_all(&buffer[..n])?;
        hasher.update(&buffer[..n]);
        copied += n as u64;
        remaining -= n as u64;
    }
    destination.sync_all()?;
    if copied != size
        || Digest::parse(&format!("blake3:{}", hasher.finalize().to_hex()))? != *digest
    {
        return Err(AppError::integrity(
            "refresh copy changed; retained bytes require inspection",
        ));
    }
    Ok(())
}
