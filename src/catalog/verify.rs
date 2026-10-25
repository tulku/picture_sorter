use super::*;
use crate::domain::*;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VerifyScope {
    Originals,
    History,
    Projections,
    All,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntegrityStatus {
    Missing,
    Corrupt,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityFinding {
    pub path: ArchivePath,
    pub status: IntegrityStatus,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IntegrityReport {
    pub checked: u64,
    pub findings: Vec<IntegrityFinding>,
}
pub struct IntegrityVerifier<'a> {
    catalog: &'a Catalog,
    root: &'a RootHandle,
}
impl<'a> IntegrityVerifier<'a> {
    pub fn new(catalog: &'a Catalog, root: &'a RootHandle) -> Self {
        Self { catalog, root }
    }
    pub fn audit(&self, scope: VerifyScope) -> AppResult<IntegrityReport> {
        let mut report = IntegrityReport::default();
        if matches!(scope, VerifyScope::Originals | VerifyScope::All) {
            let mut after = None;
            loop {
                let groups = self.catalog.group_batch(after, 128)?;
                if groups.is_empty() {
                    break;
                }
                for group in groups {
                    after = Some(group.id);
                    for member in group.members {
                        self.check(&member.path, &member.digest, member.size, &mut report)?;
                    }
                }
            }
        }
        if matches!(scope, VerifyScope::History | VerifyScope::All) {
            let mut q = self
                .catalog
                .db
                .prepare("SELECT digest,path,size FROM snapshots ORDER BY digest")?;
            let mut rows = q.query([])?;
            while let Some(row) = rows.next()? {
                self.check(
                    &ArchivePath::parse(&row.get::<_, String>(1)?)?,
                    &Digest::parse(&row.get::<_, String>(0)?)?,
                    row.get(2)?,
                    &mut report,
                )?;
            }
        }
        if matches!(scope, VerifyScope::Projections | VerifyScope::All) {
            let mut q=self.catalog.db.prepare("SELECT s.path,v.body FROM sidecars s JOIN local_heads l ON l.sidecar=s.id JOIN versions v ON v.id=l.version")?;
            let mut rows = q.query([])?;
            while let Some(row) = rows.next()? {
                let path = ArchivePath::parse(&row.get::<_, String>(0)?)?;
                match decode(row.get(1)?)? {
                    VersionBody::Content { digest, size } => {
                        self.check(&path, &digest, size, &mut report)?
                    }
                    VersionBody::Reset => {
                        report.checked += 1;
                        if self.root.open_relative(&path, libc::O_RDONLY, 0).is_ok() {
                            report.findings.push(IntegrityFinding {
                                path,
                                status: IntegrityStatus::Unknown,
                                reason: "live bytes exist after observed reset".into(),
                            });
                        }
                    }
                }
            }
        }
        self.root.validate_identity()?;
        Ok(report)
    }
    fn check(
        &self,
        path: &ArchivePath,
        digest: &Digest,
        size: u64,
        report: &mut IntegrityReport,
    ) -> AppResult<()> {
        report.checked += 1;
        let finding = match self
            .root
            .open_relative(path, libc::O_RDONLY | libc::O_NONBLOCK, 0)
        {
            Ok(mut file) => {
                if crate::transfer::digest_file(&mut file)? == (digest.clone(), size) {
                    None
                } else {
                    Some((IntegrityStatus::Corrupt, "digest or size mismatch"))
                }
            }
            Err(e) if e.errno == Some(libc::ENOENT) => {
                Some((IntegrityStatus::Missing, "expected file is absent"))
            }
            Err(_) => Some((IntegrityStatus::Unknown, "file cannot be safely read")),
        };
        if let Some((status, reason)) = finding {
            report.findings.push(IntegrityFinding {
                path: path.clone(),
                status,
                reason: reason.into(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyObject {
    pub path: ArchivePath,
    pub digest: Digest,
    pub size: u64,
}
impl IntegrityReport {
    pub fn audit_objects(root: &RootHandle, objects: &[VerifyObject]) -> AppResult<Self> {
        if objects.len() > 128 {
            return Err(AppError::invalid("audit object batch must not exceed 128"));
        }
        let mut report = Self::default();
        for object in objects {
            report.checked += 1;
            let failure =
                match root.open_relative(&object.path, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
                    Ok(mut file) => {
                        if crate::transfer::digest_file(&mut file)?
                            == (object.digest.clone(), object.size)
                        {
                            None
                        } else {
                            Some((IntegrityStatus::Corrupt, "digest or size mismatch"))
                        }
                    }
                    Err(e) if e.errno == Some(libc::ENOENT) => Some((
                        IntegrityStatus::Missing,
                        "expected immutable file is absent",
                    )),
                    Err(_) => Some((IntegrityStatus::Unknown, "file cannot be safely read")),
                };
            if let Some((status, reason)) = failure {
                report.findings.push(IntegrityFinding {
                    path: object.path.clone(),
                    status,
                    reason: reason.into(),
                });
            }
        }
        root.validate_identity()?;
        Ok(report)
    }
}
impl Catalog {
    pub fn protect_integrity(&mut self, report: &IntegrityReport) -> AppResult<()> {
        let tx = self.db.transaction()?;
        for finding in &report.findings {
            tx.execute("INSERT INTO blocked_records(id,reason) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET reason=excluded.reason",rusqlite::params![format!("integrity:{}",finding.path.as_str()),finding.reason])?;
            tx.execute("UPDATE residency SET protected=1,conflict=1 WHERE group_id IN(SELECT group_id FROM members WHERE path=?)",[finding.path.as_str()])?;
        }
        tx.commit()?;
        Ok(())
    }
}
