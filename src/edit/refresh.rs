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
    sessions: Mutex<HashMap<ApplicationSessionId, Weak<AtomicBool>>>,
}
pub struct ApplicationSession {
    id: ApplicationSessionId,
    authorized: Arc<AtomicBool>,
}
impl ApplicationSession {
    pub fn id(&self) -> ApplicationSessionId {
        self.id
    }
}
impl Drop for ApplicationSession {
    fn drop(&mut self) {
        self.authorized.store(false, Ordering::SeqCst);
    }
}
impl ApplicationSessions {
    pub fn authorize(&self, editor_closed: bool, no_wait: bool) -> AppResult<ApplicationSession> {
        if !editor_closed || no_wait {
            return Err(AppError::invalid(
                "applying refresh requires a foreground editor-closed assertion",
            ));
        }
        let id = ApplicationSessionId::new();
        let authorized = Arc::new(AtomicBool::new(true));
        let mut map = self
            .sessions
            .lock()
            .map_err(|_| AppError::integrity("application session registry unavailable"))?;
        map.retain(|_, v| v.strong_count() > 0);
        map.insert(id, Arc::downgrade(&authorized));
        Ok(ApplicationSession { id, authorized })
    }
    pub fn authorized(&self, id: ApplicationSessionId) -> bool {
        self.sessions
            .lock()
            .ok()
            .and_then(|s| s.get(&id).and_then(Weak::upgrade))
            .is_some_and(|s| s.load(Ordering::SeqCst))
    }
    pub fn revoke(&self, id: ApplicationSessionId) {
        if let Ok(sessions) = self.sessions.lock() {
            if let Some(state) = sessions.get(&id).and_then(Weak::upgrade) {
                state.store(false, Ordering::SeqCst);
            }
        }
    }
}
pub struct RefreshRequest {
    pub groups: Vec<GroupId>,
    pub application_session: ApplicationSessionId,
    pub operation_id: OperationId,
}
#[derive(Default, Debug)]
pub struct RefreshReport {
    pub applied: Vec<VersionId>,
    pub conflicts: Vec<SidecarId>,
    pub waiting_application: bool,
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
        if let Err(error) = self.state.write_new(&target, bytes) {
            if self.state.read(&target, size)? != bytes {
                return Err(error);
            }
        }
        self.catalog.db.execute("INSERT INTO snapshots(digest,path,size,archived) VALUES(?,?,?,1) ON CONFLICT(digest) DO UPDATE SET archived=1",params![digest.as_str(),target.as_str(),size])?;
        Ok(())
    }
    pub fn apply(
        &mut self,
        request: RefreshRequest,
        faults: &dyn FaultInjector,
    ) -> AppResult<RefreshReport> {
        let mut report = RefreshReport::default();
        if !self.sessions.authorized(request.application_session) {
            report.waiting_application = true;
            return Ok(report);
        }
        for group in request.groups {
            let group = self.catalog.canonical_group(group)?;
            let mut q=self.catalog.db.prepare("SELECT p.committed FROM projections p JOIN sidecars s ON s.id=p.sidecar WHERE s.group_id=? AND p.pending=1 AND p.committed IS NOT NULL")?;
            let plans = q
                .query_map([group.to_string()], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(q);
            for encoded in plans {
                if !self.sessions.authorized(request.application_session) {
                    report.waiting_application = true;
                    return Ok(report);
                }
                let plan: ProjectionPlan = serde_json::from_str(&encoded)?;
                if plan.owner == "jpeg-xmp-external" {
                    continue;
                }
                let observed = observed_body(self.local, &plan.path)?;
                if observed != plan.expected && observed != plan.body {
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
                    match root.create_new(&target) {
                        Ok(mut destination) => {
                            std::io::copy(&mut file, &mut destination)?;
                            destination.sync_all()?;
                        }
                        Err(e) if e.errno == Some(libc::EEXIST) => {}
                        Err(e) => return Err(e),
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
                    std::io::copy(&mut history, &mut file)?;
                    file.sync_all()?;
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
                            if observed_body(self.local, &displaced)? != plan.expected {
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
                                if observed_body(self.local, &staged)? != plan.expected {
                                    self.sessions.revoke(request.application_session);
                                    report.conflicts.push(plan.sidecar);
                                    return Ok(report);
                                }
                            }
                        }
                    }
                }
                self.catalog.db.execute(
                    "UPDATE projections SET pending=0,applied=? WHERE sidecar=? AND committed=?",
                    params![plan.version.to_string(), plan.sidecar.to_string(), encoded],
                )?;
                self.catalog.db.execute("INSERT INTO local_heads(sidecar,version,dirty) VALUES(?,?,0) ON CONFLICT(sidecar) DO UPDATE SET version=excluded.version,dirty=0",params![plan.sidecar.to_string(),plan.version.to_string()])?;
                report.applied.push(plan.version);
            }
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
