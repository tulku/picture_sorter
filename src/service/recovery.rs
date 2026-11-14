use crate::{catalog::Catalog, error::AppResult};
use serde::Serialize;
#[derive(Debug, Default, Serialize)]
pub struct RecoveryReport {
    pub retained_reservations: u64,
    pub unfinished_jobs: u64,
    pub uncertain_attempts: u64,
}
pub struct ServiceRecovery;
impl ServiceRecovery {
    pub fn restore(
        catalog: &mut Catalog,
        local: &crate::fs::RootHandle,
    ) -> AppResult<RecoveryReport> {
        let retained_reservations =
            catalog
                .db
                .query_row("SELECT count(*) FROM reservations", [], |r| r.get(0))?;
        let unfinished_jobs=catalog.db.query_row("SELECT count(*) FROM jobs WHERE json_extract(state,'$') NOT IN('Complete','Cancelled')",[],|r|r.get(0))?;
        let uncertain_attempts = catalog.db.query_row(
            "SELECT count(*) FROM attempts WHERE json_extract(state,'$')='Running'",
            [],
            |r| r.get(0),
        )?;
        catalog.queue_physical_checks(local)?;
        let tx = catalog.db.transaction()?;

        tx.execute("DELETE FROM pin_checks", [])?;
        tx.execute("DELETE FROM pin_sync_epochs", [])?;
        tx.commit()?;
        catalog.db.execute("UPDATE residency SET ready_revision=NULL,metadata_current=0,protected=1 WHERE group_id IN(SELECT group_id FROM startup_checks)",[])?;
        Ok(RecoveryReport {
            retained_reservations,
            unfinished_jobs,
            uncertain_attempts,
        })
    }
}

#[derive(Default)]
pub(crate) struct StartupVerifier {
    pub reason: Option<String>,
    retry_at: crate::domain::Timestamp,
    active: Option<(
        crate::transfer::WorkerTicket,
        Vec<crate::catalog::VerifyObject>,
    )>,
}
impl StartupVerifier {
    pub fn advance(
        &mut self,
        engine: &mut crate::import::ImportCoordinator,
        worker: &mut crate::transfer::WorkerSupervisor,
        local: &crate::fs::RootHandle,
    ) -> AppResult<()> {
        use crate::{catalog::*, domain::*, transfer::*};
        if let Some((ticket, objects)) = self.active.take() {
            match worker.poll(ticket)? {
                WorkerState::Active | WorkerState::TimedOutStillAlive | WorkerState::Reaped => {
                    self.active = Some((ticket, objects));
                    return Ok(());
                }
                WorkerState::Completed(NasReply::Integrity(report)) => {
                    engine
                        .catalog
                        .complete_physical_checks(local, objects, report)?;
                    self.reason = None;
                }
                WorkerState::Failed(error) => {
                    self.reason = Some(error.to_string());
                    self.retry_at = crate::domain::SystemClock::default().now() + 2;
                    return Ok(());
                }
                _ => {
                    return Err(crate::error::AppError::integrity(
                        "unexpected startup verification reply",
                    ));
                }
            }
        }
        if crate::domain::SystemClock::default().now() < self.retry_at || !worker.available()? {
            return Ok(());
        }
        let mut query = engine.catalog.db.prepare(
            "SELECT DISTINCT path,digest,size FROM startup_checks ORDER BY path LIMIT 128",
        )?;
        let objects = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u64>(2)?,
                ))
            })?
            .map(|r| {
                let (path, digest, size) = r?;
                Ok(VerifyObject {
                    path: ArchivePath::parse(&path)?,
                    digest: Digest::parse(&digest)?,
                    size,
                })
            })
            .collect::<AppResult<Vec<_>>>()?;
        drop(query);
        if !objects.is_empty() {
            let ticket = worker.submit(NasCommand::AuditLocal {
                root: engine.config.working_root.clone(),
                objects: objects.clone(),
            })?;
            self.active = Some((ticket, objects));
        }
        Ok(())
    }
}

impl Catalog {
    fn complete_physical_checks(
        &mut self,
        local: &crate::fs::RootHandle,
        objects: Vec<crate::catalog::VerifyObject>,
        report: crate::catalog::IntegrityReport,
    ) -> AppResult<()> {
        use rusqlite::params;
        let mut groups = std::collections::BTreeSet::new();
        let tx = self.db.transaction()?;
        for object in objects {
            let mut query=tx.prepare("SELECT DISTINCT p.group_id FROM placements p JOIN startup_checks s ON s.placement=p.id WHERE s.path=? AND s.digest=?")?;
            for row in query
                .query_map(params![object.path.as_str(), object.digest.as_str()], |r| {
                    r.get::<_, String>(0)
                })?
            {
                groups.insert(row?);
            }
            drop(query);
            if let Some(finding) = report
                .findings
                .iter()
                .find(|finding| finding.path == object.path)
            {
                tx.execute(
                    "DELETE FROM local_verifications WHERE path=?",
                    [object.path.as_str()],
                )?;
                tx.execute(
                    "UPDATE placements SET local_ready=0 WHERE path=? AND digest=?",
                    params![object.path.as_str(), object.digest.as_str()],
                )?;
                tx.execute("INSERT INTO physical_findings(path,group_id,digest,size,status,reason) SELECT s.path,p.group_id,s.digest,s.size,?,? FROM startup_checks s JOIN placements p ON p.id=s.placement WHERE s.path=? AND s.digest=? ON CONFLICT(path) DO UPDATE SET group_id=excluded.group_id,digest=excluded.digest,size=excluded.size,status=excluded.status,reason=excluded.reason",params![format!("{:?}",finding.status),finding.reason,object.path.as_str(),object.digest.as_str()])?;
            } else if let Some((_, stamp)) = report.verified.iter().find(|(path, stamp)| {
                *path == object.path
                    && current_stamp(local, path).is_ok_and(|current| current == *stamp)
            }) {
                tx.execute("INSERT INTO local_verifications(path,digest,size,stamp) VALUES(?,?,?,?) ON CONFLICT(path) DO UPDATE SET digest=excluded.digest,size=excluded.size,stamp=excluded.stamp", params![object.path.as_str(),object.digest.as_str(),object.size,serde_json::to_string(stamp)?])?;
                tx.execute("UPDATE placements SET local_ready=1 WHERE id IN(SELECT placement FROM startup_checks WHERE path=? AND digest=?) AND path=? AND digest=?",params![object.path.as_str(),object.digest.as_str(),object.path.as_str(),object.digest.as_str()])?;
                tx.execute(
                    "DELETE FROM physical_findings WHERE path=? AND digest=?",
                    params![object.path.as_str(), object.digest.as_str()],
                )?;
            } else {
                continue;
            }
            tx.execute(
                "DELETE FROM startup_checks WHERE path=? AND digest=?",
                params![object.path.as_str(), object.digest.as_str()],
            )?;
        }
        tx.commit()?;
        for group in groups {
            self.refresh_protection(serde_json::from_value(group.into())?)?;
        }
        Ok(())
    }
    pub(crate) fn queue_observed_original(
        &mut self,
        path: &crate::domain::ArchivePath,
        present: bool,
        local: &crate::fs::RootHandle,
    ) -> AppResult<()> {
        if self.original_verification_current(local, path)? {
            return Ok(());
        }
        let tx = self.db.transaction()?;
        tx.execute(
            "DELETE FROM local_verifications WHERE path=?",
            [path.as_str()],
        )?;
        if present {
            tx.execute("INSERT INTO local_residency_expectations SELECT id FROM placements WHERE path=? ON CONFLICT DO NOTHING",[path.as_str()])?;
        }
        tx.execute("INSERT INTO startup_checks(placement,group_id,path,digest,size) SELECT p.id,p.group_id,p.path,p.digest,b.size FROM placements p JOIN blobs b ON b.digest=p.digest WHERE p.path=? AND EXISTS(SELECT 1 FROM local_residency_expectations e WHERE e.placement=p.id) ON CONFLICT DO NOTHING",[path.as_str()])?;
        tx.execute("UPDATE placements SET local_ready=0 WHERE path=? AND id IN(SELECT placement FROM startup_checks)",[path.as_str()])?;
        tx.execute("UPDATE residency SET ready_revision=NULL,protected=1 WHERE group_id IN(SELECT group_id FROM startup_checks WHERE path=?)",[path.as_str()])?;
        tx.commit()?;
        Ok(())
    }
    pub fn queue_physical_checks(&mut self, local: &crate::fs::RootHandle) -> AppResult<()> {
        let mut query = self.db.prepare("SELECT DISTINCT path FROM placements WHERE local_ready=1 OR id IN(SELECT placement FROM local_residency_expectations)")?;
        let paths = query
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(query);
        let mut changed = Vec::new();
        for path in paths {
            let path = crate::domain::ArchivePath::parse(&path)?;
            if !self.original_verification_current(local, &path)? {
                changed.push(path);
            }
        }
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM startup_checks WHERE placement IN(SELECT p.id FROM placements p WHERE p.archived=1 AND p.local_ready=0 AND NOT EXISTS(SELECT 1 FROM local_residency_expectations e WHERE e.placement=p.id))",[])?;
        for path in changed {
            tx.execute(
                "DELETE FROM local_verifications WHERE path=?",
                [path.as_str()],
            )?;
            tx.execute("INSERT INTO startup_checks(placement,group_id,path,digest,size) SELECT p.id,p.group_id,p.path,p.digest,b.size FROM placements p JOIN blobs b ON b.digest=p.digest WHERE p.path=? AND (p.local_ready=1 OR EXISTS(SELECT 1 FROM local_residency_expectations e WHERE e.placement=p.id)) ON CONFLICT DO NOTHING",[path.as_str()])?;
        }
        tx.execute(
            "UPDATE placements SET local_ready=0 WHERE id IN(SELECT placement FROM startup_checks)",
            [],
        )?;
        tx.execute("UPDATE residency SET ready_revision=NULL,protected=1 WHERE group_id IN(SELECT group_id FROM startup_checks)",[])?;
        tx.commit()?;
        Ok(())
    }
    fn original_verification_current(
        &self,
        local: &crate::fs::RootHandle,
        path: &crate::domain::ArchivePath,
    ) -> AppResult<bool> {
        use rusqlite::OptionalExtension;
        let stamp: Option<String> = self.db.query_row("SELECT v.stamp FROM local_verifications v WHERE v.path=? AND NOT EXISTS(SELECT 1 FROM placements p JOIN blobs b ON b.digest=p.digest WHERE p.path=v.path AND (p.digest<>v.digest OR b.size<>v.size))", [path.as_str()], |row| row.get(0)).optional()?;
        Ok(stamp
            .and_then(|stamp| serde_json::from_str::<crate::edit::SnapshotStamp>(&stamp).ok())
            .is_some_and(|stamp| current_stamp(local, path).is_ok_and(|current| current == stamp)))
    }
    pub fn finalize_rebuild(
        &mut self,
        replay_current: bool,
        local_complete: bool,
    ) -> AppResult<bool> {
        if !replay_current
            || !local_complete
            || !matches!(
                self.journal_readiness()?,
                crate::catalog::PublicationReadiness::Ready
            )
        {
            return Ok(false);
        }
        let incomplete:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM startup_checks) OR EXISTS(SELECT 1 FROM capture_requests WHERE done=0) OR EXISTS(SELECT 1 FROM observations WHERE readable=0)",[],|r|r.get(0))?;
        if incomplete {
            return Ok(false);
        }
        let tx = self.db.transaction()?;
        let cleared = tx.execute(
            "DELETE FROM blocked_records WHERE id='rebuild-incomplete'",
            [],
        )?;
        if cleared > 0 {
            use crate::domain::Clock;
            tx.execute("INSERT INTO recovery_finalizations(id,completed_at) VALUES('rebuild-incomplete',?) ON CONFLICT(id) DO UPDATE SET completed_at=excluded.completed_at",[crate::domain::SystemClock::default().now()])?;
        }
        tx.commit()?;
        Ok(cleared > 0)
    }
}

fn current_stamp(
    local: &crate::fs::RootHandle,
    path: &crate::domain::ArchivePath,
) -> AppResult<crate::edit::SnapshotStamp> {
    let file = local.open_relative(path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
    Ok(crate::edit::SnapshotStamp::of(&file.metadata()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{RootHandle, RootPolicy};
    #[test]
    fn unchanged_verified_original_survives_reconciliation_and_restart() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        std::fs::write(temp.path().join("resident.ORF"), b"fixture").unwrap();
        let path = crate::domain::ArchivePath::parse("resident.ORF").unwrap();
        let stamp = crate::edit::SnapshotStamp::of(
            &root
                .open_relative(&path, libc::O_RDONLY, 0)
                .unwrap()
                .metadata()
                .unwrap(),
        );
        let catalog = Catalog::open(&root).unwrap();
        catalog.db.execute_batch("INSERT INTO blobs(digest,size) VALUES('test',7);
INSERT INTO placements(id,group_id,path,digest,local_ready) VALUES('resident','g','resident.ORF','test',1);").unwrap();
        catalog
            .db
            .execute(
                "INSERT INTO local_verifications VALUES('resident.ORF','test',7,?)",
                [serde_json::to_string(&stamp).unwrap()],
            )
            .unwrap();
        drop(catalog);
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.queue_physical_checks(&root).unwrap();
        catalog.queue_observed_original(&path, true, &root).unwrap();
        let pending: u64 = catalog
            .db
            .query_row("SELECT count(*) FROM startup_checks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pending, 0, "unchanged verified bytes must not be rehashed");
    }
    #[test]
    fn startup_checks_preserve_local_expectations_and_skip_remote_only_history() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.db.execute_batch("INSERT INTO blobs(digest,size) VALUES('test',7);
INSERT INTO placements(id,group_id,path,digest,local_ready,archived) VALUES('remote','g','remote.ORF','test',0,1),('expected','g','expected.ORF','test',0,1),('resident','g','resident.ORF','test',1,0);
INSERT INTO local_residency_expectations VALUES('expected');
INSERT INTO startup_checks(placement,group_id,path,digest,size) VALUES('remote','g','remote.ORF','test',7);").unwrap();
        catalog.queue_physical_checks(&root).unwrap();
        let paths = catalog
            .db
            .prepare("SELECT path FROM startup_checks ORDER BY path")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(paths, vec!["expected.ORF", "resident.ORF"]);
    }
    #[test]
    fn verified_cache_invalidates_edits_replacements_and_deletions() {
        use crate::{catalog::IntegrityReport, domain::*};
        for change in ["edit", "replace", "delete"] {
            let temp = tempfile::tempdir().unwrap();
            let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
            let mut catalog = Catalog::open(&root).unwrap();
            let group = catalog.create_group(false).unwrap();
            let path = ArchivePath::parse("resident.ORF").unwrap();
            let digest = Digest::of(b"fixture");
            let object = crate::catalog::VerifyObject {
                path: path.clone(),
                digest: digest.clone(),
                size: 7,
            };
            std::fs::write(temp.path().join(path.as_str()), b"fixture").unwrap();
            catalog
                .db
                .execute(
                    "INSERT INTO blobs(digest,size) VALUES(?,7)",
                    [digest.as_str()],
                )
                .unwrap();
            catalog.db.execute("INSERT INTO placements(id,group_id,path,digest,local_ready) VALUES('p',?,'resident.ORF',?,1)", rusqlite::params![group.to_string(),digest.as_str()]).unwrap();
            catalog.queue_physical_checks(&root).unwrap();
            let report =
                IntegrityReport::audit_objects(&root, std::slice::from_ref(&object)).unwrap();
            catalog
                .complete_physical_checks(&root, vec![object.clone()], report)
                .unwrap();
            drop(catalog);
            let mut catalog = Catalog::open(&root).unwrap();
            catalog.queue_physical_checks(&root).unwrap();
            assert_eq!(pending(&catalog), 0);
            assert_eq!(
                IntegrityReport::audit_objects(&root, std::slice::from_ref(&object))
                    .unwrap()
                    .checked,
                1
            );
            match change {
                "edit" => std::fs::write(temp.path().join(path.as_str()), b"changed").unwrap(),
                "replace" => {
                    std::fs::write(temp.path().join("replacement"), b"fixture").unwrap();
                    std::fs::rename(
                        temp.path().join("replacement"),
                        temp.path().join(path.as_str()),
                    )
                    .unwrap();
                }
                _ => std::fs::remove_file(temp.path().join(path.as_str())).unwrap(),
            }
            catalog
                .queue_observed_original(&path, change != "delete", &root)
                .unwrap();
            assert_eq!(pending(&catalog), 1, "{change}");
            catalog.queue_physical_checks(&root).unwrap();
            assert_eq!(pending(&catalog), 1, "{change}");
            assert!(!catalog.readiness(group).unwrap().originals_ready);
        }
    }
    #[test]
    fn changed_worker_proof_keeps_check_pending() {
        use crate::{catalog::IntegrityReport, domain::*};
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let group = catalog.create_group(false).unwrap();
        let path = ArchivePath::parse("resident.ORF").unwrap();
        let digest = Digest::of(b"fixture");
        let object = crate::catalog::VerifyObject {
            path: path.clone(),
            digest: digest.clone(),
            size: 7,
        };
        std::fs::write(temp.path().join(path.as_str()), b"fixture").unwrap();
        catalog
            .db
            .execute(
                "INSERT INTO blobs(digest,size) VALUES(?,7)",
                [digest.as_str()],
            )
            .unwrap();
        catalog.db.execute("INSERT INTO placements(id,group_id,path,digest,local_ready) VALUES('p',?,'resident.ORF',?,1)", rusqlite::params![group.to_string(),digest.as_str()]).unwrap();
        catalog.queue_physical_checks(&root).unwrap();
        let report = IntegrityReport::audit_objects(&root, std::slice::from_ref(&object)).unwrap();
        std::fs::write(temp.path().join(path.as_str()), b"changed").unwrap();
        catalog
            .complete_physical_checks(&root, vec![object], report)
            .unwrap();
        assert_eq!(pending(&catalog), 1);
        assert!(!catalog.readiness(group).unwrap().originals_ready);
    }
    fn pending(catalog: &Catalog) -> u64 {
        catalog
            .db
            .query_row("SELECT count(*) FROM startup_checks", [], |r| r.get(0))
            .unwrap()
    }
}
