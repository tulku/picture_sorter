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
    pub fn restore(catalog: &mut Catalog) -> AppResult<RecoveryReport> {
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
        catalog.queue_physical_checks()?;
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
    ) -> AppResult<()> {
        use crate::{catalog::*, domain::*, transfer::*};
        use rusqlite::params;
        if let Some((ticket, objects)) = self.active.take() {
            match worker.poll(ticket)? {
                WorkerState::Active | WorkerState::TimedOutStillAlive | WorkerState::Reaped => {
                    self.active = Some((ticket, objects));
                    return Ok(());
                }
                WorkerState::Completed(NasReply::Integrity(report)) => {
                    let mut groups = std::collections::BTreeSet::new();
                    let tx = engine.catalog.db.transaction()?;
                    for object in objects {
                        let mut query=tx.prepare("SELECT DISTINCT p.group_id FROM placements p JOIN startup_checks s ON s.placement=p.id WHERE s.path=? AND s.digest=?")?;
                        for row in query.query_map(
                            params![object.path.as_str(), object.digest.as_str()],
                            |r| r.get::<_, String>(0),
                        )? {
                            groups.insert(row?);
                        }
                        drop(query);
                        if let Some(finding) = report
                            .findings
                            .iter()
                            .find(|finding| finding.path == object.path)
                        {
                            tx.execute(
                                "UPDATE placements SET local_ready=0 WHERE path=? AND digest=?",
                                params![object.path.as_str(), object.digest.as_str()],
                            )?;
                            tx.execute("INSERT INTO physical_findings(path,group_id,digest,size,status,reason) SELECT s.path,p.group_id,s.digest,s.size,?,? FROM startup_checks s JOIN placements p ON p.id=s.placement WHERE s.path=? AND s.digest=? ON CONFLICT(path) DO UPDATE SET group_id=excluded.group_id,digest=excluded.digest,size=excluded.size,status=excluded.status,reason=excluded.reason",params![format!("{:?}",finding.status),finding.reason,object.path.as_str(),object.digest.as_str()])?;
                        } else {
                            tx.execute("UPDATE placements SET local_ready=1 WHERE id IN(SELECT placement FROM startup_checks WHERE path=? AND digest=?) AND path=? AND digest=?",params![object.path.as_str(),object.digest.as_str(),object.path.as_str(),object.digest.as_str()])?;
                            tx.execute(
                                "DELETE FROM physical_findings WHERE path=? AND digest=?",
                                params![object.path.as_str(), object.digest.as_str()],
                            )?;
                        }
                        tx.execute(
                            "DELETE FROM startup_checks WHERE path=? AND digest=?",
                            params![object.path.as_str(), object.digest.as_str()],
                        )?;
                    }
                    tx.commit()?;
                    for group in groups {
                        engine
                            .catalog
                            .refresh_protection(serde_json::from_value(group.into())?)?;
                    }
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
    pub(crate) fn queue_observed_original(
        &mut self,
        path: &crate::domain::ArchivePath,
        present: bool,
    ) -> AppResult<()> {
        let tx = self.db.transaction()?;
        if present {
            tx.execute("INSERT INTO local_residency_expectations SELECT id FROM placements WHERE path=? ON CONFLICT DO NOTHING",[path.as_str()])?;
        }
        tx.execute("INSERT INTO startup_checks(placement,group_id,path,digest,size) SELECT p.id,p.group_id,p.path,p.digest,b.size FROM placements p JOIN blobs b ON b.digest=p.digest WHERE p.path=? AND EXISTS(SELECT 1 FROM local_residency_expectations e WHERE e.placement=p.id) ON CONFLICT DO NOTHING",[path.as_str()])?;
        tx.execute("UPDATE placements SET local_ready=0 WHERE path=? AND id IN(SELECT placement FROM startup_checks)",[path.as_str()])?;
        tx.execute("UPDATE residency SET ready_revision=NULL,protected=1 WHERE group_id IN(SELECT group_id FROM startup_checks WHERE path=?)",[path.as_str()])?;
        tx.commit()?;
        Ok(())
    }
    pub fn queue_physical_checks(&mut self) -> AppResult<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM startup_checks WHERE placement IN(SELECT p.id FROM placements p WHERE p.archived=1 AND p.local_ready=0 AND NOT EXISTS(SELECT 1 FROM local_residency_expectations e WHERE e.placement=p.id))",[])?;
        tx.execute("INSERT INTO startup_checks(placement,group_id,path,digest,size) SELECT p.id,p.group_id,p.path,p.digest,b.size FROM placements p JOIN blobs b ON b.digest=p.digest WHERE p.local_ready=1 OR EXISTS(SELECT 1 FROM local_residency_expectations e WHERE e.placement=p.id) ON CONFLICT DO NOTHING",[])?;
        tx.execute(
            "UPDATE placements SET local_ready=0 WHERE id IN(SELECT placement FROM startup_checks)",
            [],
        )?;
        tx.execute("UPDATE residency SET ready_revision=NULL,protected=1 WHERE group_id IN(SELECT group_id FROM startup_checks)",[])?;
        tx.commit()?;
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{RootHandle, RootPolicy};
    #[test]
    fn startup_checks_preserve_local_expectations_and_skip_remote_only_history() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.db.execute_batch("INSERT INTO blobs(digest,size) VALUES('test',7);
INSERT INTO placements(id,group_id,path,digest,local_ready,archived) VALUES('remote','g','remote.ORF','test',0,1),('expected','g','expected.ORF','test',0,1),('resident','g','resident.ORF','test',1,0);
INSERT INTO local_residency_expectations VALUES('expected');
INSERT INTO startup_checks(placement,group_id,path,digest,size) VALUES('remote','g','remote.ORF','test',7);").unwrap();
        catalog.queue_physical_checks().unwrap();
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
}
