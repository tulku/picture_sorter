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
        let tx = catalog.db.transaction()?;
        tx.execute("INSERT INTO startup_checks(placement,group_id,path,digest,size) SELECT p.id,p.group_id,p.path,p.digest,b.size FROM placements p JOIN blobs b ON b.digest=p.digest WHERE p.local_ready=1 ON CONFLICT DO NOTHING", [])?;
        tx.execute("UPDATE placements SET local_ready=0", [])?;
        tx.execute("DELETE FROM pin_checks", [])?;
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
                    let tx = engine.catalog.db.transaction()?;
                    for object in objects {
                        if !report.findings.iter().any(|f| f.path == object.path) {
                            tx.execute("UPDATE placements SET local_ready=1 WHERE id IN(SELECT placement FROM startup_checks WHERE path=? AND digest=?) AND path=? AND digest=?", params![object.path.as_str(), object.digest.as_str(), object.path.as_str(), object.digest.as_str()])?;
                        } else {
                            tx.execute("UPDATE residency SET protected=1 WHERE group_id IN(SELECT group_id FROM startup_checks WHERE path=?)", [object.path.as_str()])?;
                        }
                        tx.execute(
                            "DELETE FROM startup_checks WHERE path=? AND digest=?",
                            params![object.path.as_str(), object.digest.as_str()],
                        )?;
                    }
                    tx.commit()?;
                }
                WorkerState::Failed(error) => return Err(error),
                _ => {
                    return Err(crate::error::AppError::integrity(
                        "unexpected startup verification reply",
                    ));
                }
            }
        }
        if !worker.available()? {
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
