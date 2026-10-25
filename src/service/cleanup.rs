use crate::{
    catalog::*,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::RootHandle,
    import::ImportCoordinator,
    storage::*,
    transfer::*,
};
use rusqlite::{OptionalExtension, params};
#[derive(Default)]
pub(crate) struct CleanupDriver {
    active: Option<Active>,
    pub reason: Option<String>,
}
enum Active {
    Evict {
        job: JobId,
        intent: EvictionIntent,
        ticket: WorkerTicket,
    },
    Sweep {
        kind: String,
        object: VerifyObject,
        ticket: WorkerTicket,
    },
}
impl CleanupDriver {
    pub fn start_eviction(
        &mut self,
        engine: &mut ImportCoordinator,
        worker: &mut WorkerSupervisor,
        job: JobId,
        intent: EvictionIntent,
    ) -> AppResult<()> {
        if self.active.is_some() || !worker.available()? {
            return Ok(());
        }
        if !eviction_still_allowed(&engine.catalog, &intent)? {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "eviction invalidated before verification",
            ));
        }
        engine.catalog.db.execute(
            "INSERT INTO cleanup_evictions(job,intent) VALUES(?,?) ON CONFLICT DO NOTHING",
            params![job.to_string(), serde_json::to_string(&intent)?],
        )?;
        let objects = intent
            .members
            .iter()
            .map(|m| VerifyObject {
                path: m.path.clone(),
                digest: m.digest.clone(),
                size: m.size,
            })
            .collect();
        let ticket = worker.submit(NasCommand::AuditLocal {
            root: engine.config.working_root.clone(),
            objects,
        })?;
        self.active = Some(Active::Evict {
            job,
            intent,
            ticket,
        });
        Ok(())
    }
    pub fn request_clean(&mut self, engine: &mut ImportCoordinator, job: JobId) -> AppResult<()> {
        let stored: Option<(String, bool)> = engine
            .catalog
            .db
            .query_row(
                "SELECT plan,scheduled FROM cleanup_requests WHERE job=?",
                [job.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let intents: Vec<EvictionIntent> = if let Some((plan, scheduled)) = stored {
            if scheduled {
                return Ok(());
            }
            serde_json::from_str(&plan)?
        } else {
            let intents = EvictionPlanner::candidates_with_hold(
                &engine.catalog,
                SystemClock::default().now(),
                engine.config.activity_hold_seconds,
            )?;
            engine.catalog.db.execute(
                "INSERT INTO cleanup_requests(job,done,plan) VALUES(?,0,?)",
                params![job.to_string(), serde_json::to_string(&intents)?],
            )?;
            intents
        };
        for intent in intents {
            let child = engine.catalog.enqueue(DurableRequest {
                operation_id: intent.operation,
                kind: JobKind::Evict {
                    group: intent.group,
                    revision: intent.revision,
                    members: intent.members,
                    activity: intent.eligibility_activity,
                },
            })?;
            engine.catalog.db.execute(
                "INSERT INTO cleanup_children(parent,child) VALUES(?,?) ON CONFLICT DO NOTHING",
                params![job.to_string(), child.to_string()],
            )?;
        }
        engine.catalog.db.execute(
            "UPDATE cleanup_requests SET scheduled=1 WHERE job=?",
            [job.to_string()],
        )?;
        Ok(())
    }
    pub fn advance(
        &mut self,
        engine: &mut ImportCoordinator,
        local: &RootHandle,
        state: &RootHandle,
        worker: &mut WorkerSupervisor,
    ) -> AppResult<()> {
        if let Some(active) = self.active.take() {
            let ticket = match &active {
                Active::Evict { ticket, .. } | Active::Sweep { ticket, .. } => *ticket,
            };
            match worker.poll(ticket)? {
                WorkerState::Active | WorkerState::TimedOutStillAlive | WorkerState::Reaped => {
                    self.active = Some(active);
                    return Ok(());
                }
                WorkerState::Completed(NasReply::Integrity(proof)) => match active {
                    Active::Evict { job, intent, .. } => {
                        if engine.catalog.job(job)?.cancel_requested {
                            return Ok(());
                        }
                        let result = Evictor {
                            catalog: &mut engine.catalog,
                            local,
                        }
                        .execute_verified(intent, &proof);
                        if let Err(error) = result {
                            let record = engine.catalog.job(job)?;
                            engine.catalog.transition(
                                job,
                                record.generation,
                                JobTransition::Wait(JobState::Conflict, error.to_string()),
                            )?;
                            self.reason = Some(error.to_string());
                        }
                    }
                    Active::Sweep { kind, object, .. } => self.finish_sweep(
                        engine,
                        if kind == "snapshot" { state } else { local },
                        &kind,
                        &object,
                        &proof,
                    )?,
                },
                WorkerState::Failed(error) => {
                    self.reason = Some(error.to_string());
                    match active {
                        Active::Evict { job, .. } => {
                            let record = engine.catalog.job(job)?;
                            engine.catalog.transition(
                                job,
                                record.generation,
                                JobTransition::Wait(JobState::Failed, error.to_string()),
                            )?;
                        }
                        Active::Sweep { kind, object, .. } => {
                            engine.catalog.db.execute("UPDATE cleanup_receipts SET state='retained',reason=? WHERE kind=? AND path=? AND digest=?",params![error.to_string(),kind,object.path.as_str(),object.digest.as_str()])?;
                        }
                    }
                }
                WorkerState::Completed(_) => {
                    return Err(AppError::integrity("unexpected cleanup proof"));
                }
            }
        }
        let requested: bool = engine.catalog.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM cleanup_requests r JOIN jobs j ON j.id=r.job WHERE r.done=0 AND r.scheduled=1 AND j.cancel_requested=0)",
            [],
            |r| r.get(0),
        )?;
        if !requested || !worker.available()? {
            return Ok(());
        }
        if let Some((kind, object)) = select_sweep(&mut engine.catalog)? {
            engine.catalog.db.execute("INSERT INTO cleanup_receipts(kind,path,digest,state) VALUES(?,?,?,'intent') ON CONFLICT(kind,path,digest) DO UPDATE SET state='intent',reason=NULL WHERE cleanup_receipts.state='removed'",params![kind,object.path.as_str(),object.digest.as_str()])?;
            if kind == "snapshot" {
                engine.catalog.db.execute(
                    "UPDATE snapshots SET cleanup_intent=1 WHERE digest=?",
                    [object.digest.as_str()],
                )?;
            }
            let root = if kind == "snapshot" {
                engine.config.state_root.clone()
            } else {
                engine.config.working_root.clone()
            };
            let ticket = worker.submit(NasCommand::AuditLocal {
                root,
                objects: vec![object.clone()],
            })?;
            self.active = Some(Active::Sweep {
                kind,
                object,
                ticket,
            });
            return Ok(());
        }
        let mut query=engine.catalog.db.prepare("SELECT r.job FROM cleanup_requests r WHERE done=0 AND scheduled=1 AND NOT EXISTS(SELECT 1 FROM cleanup_children c JOIN jobs j ON j.id=c.child WHERE c.parent=r.job AND json_extract(j.state,'$') NOT IN('Complete','Cancelled','Conflict','Failed')) LIMIT 128")?;
        let jobs = query
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(query);
        for job in jobs {
            let job: JobId = serde_json::from_value(job.into())?;
            let record = engine.catalog.job(job)?;
            if !record.cancel_requested {
                engine
                    .catalog
                    .transition(job, record.generation, JobTransition::Complete)?;
            }
            engine.catalog.db.execute(
                "UPDATE cleanup_requests SET done=1 WHERE job=?",
                [job.to_string()],
            )?;
        }
        Ok(())
    }
    fn finish_sweep(
        &mut self,
        engine: &mut ImportCoordinator,
        root: &RootHandle,
        kind: &str,
        object: &VerifyObject,
        proof: &IntegrityReport,
    ) -> AppResult<()> {
        let authorized:bool=engine.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM cleanup_requests r JOIN jobs j ON j.id=r.job WHERE r.done=0 AND r.scheduled=1 AND j.cancel_requested=0)",[],|r|r.get(0))?;
        if !authorized {
            engine.catalog.db.execute(
                "DELETE FROM cleanup_receipts WHERE kind=? AND path=? AND digest=?",
                params![kind, object.path.as_str(), object.digest.as_str()],
            )?;
            return Ok(());
        }
        SnapshotGc::refresh_references(&mut engine.catalog)?;
        let eligible = if kind == "snapshot" {
            engine.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM snapshots WHERE digest=? AND archived=1 AND refs=0) AND NOT EXISTS(SELECT 1 FROM capture_requests WHERE done=0)",[object.digest.as_str()],|r|r.get::<_,bool>(0))?
        } else {
            if kind == "partial" {
                partial_eligible(&engine.catalog, object)?
            } else {
                companion_eligible(&engine.catalog, object)?
            }
        };
        if !eligible {
            engine.catalog.db.execute(
                "DELETE FROM cleanup_receipts WHERE kind=? AND path=? AND digest=?",
                params![kind, object.path.as_str(), object.digest.as_str()],
            )?;
            return Ok(());
        }
        root.validate_identity()?;
        let safe = match root.open_relative(&object.path, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
            Ok(file) => proof.verified.iter().any(|(path, stamp)| {
                path == &object.path
                    && file
                        .metadata()
                        .is_ok_and(|m| crate::edit::SnapshotStamp::of(&m) == *stamp)
            }),
            Err(error) if error.errno == Some(libc::ENOENT) => true,
            Err(_) => false,
        };
        if !safe {
            engine.catalog.db.execute("UPDATE cleanup_receipts SET state='retained',reason='changed or unverified bytes retained' WHERE kind=? AND path=? AND digest=?",params![kind,object.path.as_str(),object.digest.as_str()])?;
            return Ok(());
        }
        match root.unlink(&object.path, false) {
            Ok(()) => {}
            Err(error) if error.errno == Some(libc::ENOENT) => {}
            Err(error) => return Err(error),
        }
        engine.catalog.invalidate_admission()?;
        if kind == "snapshot" {
            engine.catalog.db.execute(
                "DELETE FROM snapshots WHERE digest=? AND refs=0",
                [object.digest.as_str()],
            )?;
        } else if kind == "companion" {
            engine.catalog.db.execute(
                "UPDATE import_items SET local_ready=0 WHERE destination=? AND digest=?",
                params![object.path.as_str(), object.digest.as_str()],
            )?;
        }
        engine.catalog.db.execute(
            "UPDATE cleanup_receipts SET state='removed' WHERE kind=? AND path=? AND digest=?",
            params![kind, object.path.as_str(), object.digest.as_str()],
        )?;
        Ok(())
    }
}
fn companion_eligible(catalog: &Catalog, object: &VerifyObject) -> AppResult<bool> {
    if !object
        .path
        .as_str()
        .starts_with(".photo-sorter/companions/")
    {
        return Ok(false);
    }
    Ok(catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM import_items i WHERE i.destination=? AND i.digest=? AND i.size=? AND i.archived=1 AND i.local_ready=1) AND EXISTS(SELECT 1 FROM versions v JOIN archived_versions a ON a.version=v.id JOIN sidecars s ON s.id=v.sidecar WHERE json_extract(v.body,'$.Content.digest')=? AND json_extract(v.body,'$.Content.size')=? AND (s.group_id IS NOT NULL OR EXISTS(SELECT 1 FROM retained_companions r WHERE r.sidecar=s.id)))",params![object.path.as_str(),object.digest.as_str(),object.size,object.digest.as_str(),object.size],|r|r.get(0))?)
}
fn partial_candidates(catalog: &Catalog) -> AppResult<Vec<VerifyObject>> {
    let mut query=catalog.db.prepare("SELECT j.id,a.generation,i.digest,i.size FROM jobs j JOIN attempts a ON a.job=j.id JOIN import_items i ON i.id=json_extract(j.request,'$.kind.item') JOIN jobs parent ON parent.id=json_extract(j.request,'$.kind.parent') WHERE json_extract(j.request,'$.kind.type')='local_transfer' AND json_extract(a.state,'$')='quiescent' AND i.archived=1 AND (json_extract(j.state,'$')='Complete' OR json_extract(parent.state,'$')='Cancelled') AND NOT EXISTS(SELECT 1 FROM reservations r WHERE r.job=j.id AND remaining>0) AND NOT EXISTS(SELECT 1 FROM cleanup_receipts c WHERE c.kind='partial' AND c.path='.photo-sorter/staging/'||j.id||'/'||a.generation||'/payload' AND c.digest=i.digest AND c.state IN('removed','retained')) ORDER BY j.id,a.generation LIMIT 128")?;
    query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u64>(3)?,
            ))
        })?
        .map(|row| {
            let (id, generation, digest, size) = row?;
            Ok(VerifyObject {
                path: staging_path(serde_json::from_value(id.into())?, generation),
                digest: Digest::parse(&digest)?,
                size,
            })
        })
        .collect()
}
fn partial_eligible(catalog: &Catalog, object: &VerifyObject) -> AppResult<bool> {
    Ok(partial_candidates(catalog)?.iter().any(|candidate| {
        candidate.path == object.path
            && candidate.digest == object.digest
            && candidate.size == object.size
    }))
}
fn select_sweep(catalog: &mut Catalog) -> AppResult<Option<(String, VerifyObject)>> {
    for (digest, path, size) in SnapshotGc::candidates(catalog)? {
        let attempted:bool=catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM cleanup_receipts WHERE kind='snapshot' AND path=? AND digest=? AND state='retained' )",params![path,digest],|r|r.get(0))?;
        if ArchivePath::parse(&path)? != crate::edit::snapshot_path(&Digest::parse(&digest)?)? {
            return Err(AppError::integrity("invalid snapshot cleanup ownership"));
        }
        if !attempted {
            return Ok(Some((
                "snapshot".into(),
                VerifyObject {
                    path: ArchivePath::parse(&path)?,
                    digest: Digest::parse(&digest)?,
                    size,
                },
            )));
        }
    }
    let mut query=catalog.db.prepare("SELECT DISTINCT destination,digest,size FROM import_items i WHERE archived=1 AND local_ready=1 AND destination LIKE '.photo-sorter/companions/%' AND NOT EXISTS(SELECT 1 FROM cleanup_receipts c WHERE c.kind='companion' AND c.path=i.destination AND c.digest=i.digest AND c.state='retained' ) ORDER BY destination LIMIT 128")?;
    let objects = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(query);
    for (path, digest, size) in objects {
        let object = VerifyObject {
            path: ArchivePath::parse(&path)?,
            digest: Digest::parse(&digest)?,
            size,
        };
        if companion_eligible(catalog, &object)? {
            return Ok(Some(("companion".into(), object)));
        }
    }
    for object in partial_candidates(catalog)? {
        let attempted:bool=catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM cleanup_receipts WHERE kind='partial' AND path=? AND digest=? AND state IN('removed','retained'))",params![object.path.as_str(),object.digest.as_str()],|r|r.get(0))?;
        if !attempted {
            return Ok(Some(("partial".into(), object)));
        }
    }
    Ok(None)
}
