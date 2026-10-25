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
    #[serde(default)]
    pub verified: Vec<(ArchivePath, crate::edit::SnapshotStamp)>,
    pub findings: Vec<IntegrityFinding>,
    #[serde(default)]
    pub verified_absent: Vec<ArchivePath>,
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
                        let before = crate::edit::SnapshotStamp::of(&file.metadata()?);
                        if crate::transfer::digest_file(&mut file)?
                            == (object.digest.clone(), object.size)
                            && before == crate::edit::SnapshotStamp::of(&file.metadata()?)
                        {
                            report.verified.push((object.path.clone(), before));
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
        self.protect_integrity_scoped("local", report)
    }
    pub(crate) fn protect_integrity_scoped(
        &mut self,
        scope: &str,
        report: &IntegrityReport,
    ) -> AppResult<()> {
        let tx = self.db.transaction()?;
        for path in report
            .verified
            .iter()
            .map(|(p, _)| p)
            .chain(&report.verified_absent)
        {
            tx.execute(
                "DELETE FROM blocked_records WHERE id=?",
                [format!("integrity:{scope}:{}", path.as_str())],
            )?;
        }
        for finding in &report.findings {
            tx.execute("INSERT INTO blocked_records(id,reason) VALUES(?,?) ON CONFLICT(id) DO UPDATE SET reason=excluded.reason",rusqlite::params![format!("integrity:{scope}:{}",finding.path.as_str()),finding.reason])?;
        }
        tx.commit()?;
        Ok(())
    }
}

impl Catalog {
    pub(crate) fn block_journal_enumeration(&self, reason: &str) -> AppResult<()> {
        self.db.execute("INSERT INTO blocked_records(id,reason) VALUES('journal-enumeration',?) ON CONFLICT(id) DO UPDATE SET reason=excluded.reason", [reason])?;
        Ok(())
    }
    pub(crate) fn clear_journal_enumeration_block(&self) -> AppResult<()> {
        self.db.execute(
            "DELETE FROM blocked_records WHERE id='journal-enumeration'",
            [],
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationTarget {
    pub path: ArchivePath,
    pub body: VersionBody,
}
impl IntegrityReport {
    pub fn audit_targets(root: &RootHandle, targets: &[VerificationTarget]) -> AppResult<Self> {
        if targets.len() > 128 {
            return Err(AppError::invalid("audit batch exceeds 128"));
        }
        let mut report = Self::default();
        for target in targets {
            match &target.body {
                VersionBody::Content { digest, size } => {
                    let item = Self::audit_objects(
                        root,
                        &[VerifyObject {
                            path: target.path.clone(),
                            digest: digest.clone(),
                            size: *size,
                        }],
                    )?;
                    report.checked += item.checked;
                    report.verified.extend(item.verified);
                    report.findings.extend(item.findings);
                }
                VersionBody::Reset => {
                    report.checked += 1;
                    match root.metadata_relative(&target.path) {
                        Err(e) if e.errno == Some(libc::ENOENT) => {
                            report.verified_absent.push(target.path.clone())
                        }
                        result => report.findings.push(IntegrityFinding {
                            path: target.path.clone(),
                            status: IntegrityStatus::Unknown,
                            reason: match result {
                                Ok(_) => "unexpected live bytes after reset".into(),
                                Err(e) => e.to_string(),
                            },
                        }),
                    }
                }
            }
        }
        root.validate_identity()?;
        Ok(report)
    }
}
impl Catalog {
    pub(crate) fn verification_batch(
        &self,
        scope: &str,
        after: &str,
    ) -> AppResult<Vec<(String, VerificationTarget)>> {
        let sql = match scope {
            "originals" => {
                "SELECT p.path, p.path, json_object('Content',json_object('digest',p.digest,'size',b.size)) FROM placements p JOIN blobs b ON b.digest=p.digest WHERE EXISTS(SELECT 1 FROM local_residency_expectations e WHERE e.placement=p.id) AND p.path>? ORDER BY p.path LIMIT 128"
            }
            "history" => {
                "SELECT digest,path,json_object('Content',json_object('digest',digest,'size',size)) FROM snapshots WHERE digest>? ORDER BY digest LIMIT 128"
            }
            "local_projections" => {
                "SELECT s.id,s.path,v.body FROM sidecars s JOIN local_heads l ON l.sidecar=s.id JOIN versions v ON v.id=l.version WHERE s.owner<>'jpeg-xmp-external' AND s.id>? ORDER BY s.id LIMIT 128"
            }
            "archive_projections" => {
                "SELECT p.sidecar,json_extract(p.committed,'$.path'),json_extract(p.committed,'$.body') FROM projections p WHERE p.committed IS NOT NULL AND p.sidecar>? ORDER BY p.sidecar LIMIT 128"
            }
            _ => return Err(AppError::invalid("unknown verification scope")),
        };
        let mut q = self.db.prepare(sql)?;
        let rows = q
            .query_map([after], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(key, path, body)| {
                Ok((
                    key,
                    VerificationTarget {
                        path: ArchivePath::parse(&path)?,
                        body: serde_json::from_str(&body)?,
                    },
                ))
            })
            .collect()
    }
}
