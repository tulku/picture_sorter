use super::*;
use crate::{
    archive::*,
    catalog::*,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    transfer::*,
};
use rusqlite::{OptionalExtension, params};

type UploadItemRow = (
    String,
    String,
    String,
    String,
    u64,
    String,
    Option<String>,
    String,
    bool,
    bool,
);
#[derive(Default)]
pub struct UploadDriver {
    active: Option<Active>,
}
struct Active {
    job: JobId,
    item: ImportItem,
    ticket: WorkerTicket,
    phase: Phase,
    session: Option<OperationId>,
    batch: Option<JournalBatch>,
    index: usize,
    plan: Option<PublicationPlan>,
}
#[derive(Clone, Copy)]
enum Phase {
    Stage,
    History,
    Begin,
    List,
    Read,
    Ack,
    Publish,
    End,
}
impl ImportCoordinator {
    pub fn bind_archive(&mut self) -> AppResult<()> {
        if let Some(archive) = self.config.archive_id {
            let tx = self.catalog.db.transaction()?;
            let old: Option<String> = tx
                .query_row("SELECT id FROM archives LIMIT 1", [], |r| r.get(0))
                .optional()?;
            if old.is_some_and(|a| a != archive.to_string()) {
                return Err(AppError::integrity(
                    "profile catalog bound to another archive",
                ));
            }
            tx.execute(
                "INSERT INTO archives(id,writer) VALUES(?,?) ON CONFLICT DO NOTHING",
                params![archive.to_string(), self.config.writer_id.to_string()],
            )?;
            tx.execute(
                "UPDATE members SET archive=? WHERE archive='unbound'",
                [archive.to_string()],
            )?;
            tx.commit()?;
        }
        Ok(())
    }
    fn next_upload(&self, job: JobId) -> AppResult<Option<ImportItem>> {
        let row:Option<UploadItemRow>=self.catalog.db.query_row("SELECT id,source_root,source_path,digest,size,role,group_id,destination,local_ready,archived FROM import_items WHERE parent=? AND archived=0 AND state<>'conflict' ORDER BY rowid LIMIT 1",[job.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?))).optional()?;
        row.map(
            |(id, root, path, digest, size, role, group, destination, local_ready, _archived)| {
                Ok(ImportItem {
                    id: serde_json::from_str(&format!("\"{id}\""))?,
                    source: SourceLocation {
                        root: root.into(),
                        path: ArchivePath::parse(&path)?,
                    },
                    digest: Digest::parse(&digest)?,
                    size,
                    role: serde_json::from_str(&role)?,
                    group: group
                        .map(|g| serde_json::from_str(&format!("\"{g}\"")))
                        .transpose()?,
                    destination: ArchivePath::parse(&destination)?,
                    local_ready,
                })
            },
        )
        .transpose()
    }
}
impl UploadDriver {
    pub fn advance(
        &mut self,
        engine: &mut ImportCoordinator,
        worker: &mut WorkerSupervisor,
        job: JobId,
    ) -> AppResult<ImportReport> {
        engine.bind_archive()?;
        let archive = engine
            .config
            .archive_id
            .ok_or_else(|| AppError::new(ErrorCode::Unavailable, "archive is not bound"))?;
        if self.active.is_none() {
            let Some(item) = engine.next_upload(job)? else {
                return engine.report(job);
            };
            let source = if item.local_ready {
                SourceLocation {
                    root: engine.config.working_root.clone(),
                    path: item.destination.clone(),
                }
            } else {
                item.source.clone()
            };
            let spec = TransferSpec {
                job: item.id,
                generation: 1,
                source,
                destination_root: engine.config.archive_root.clone(),
                staged_path: staging_path(item.id, 1),
                expected_digest: item.digest.clone(),
                expected_size: item.size,
            };
            let ticket = worker.submit(NasCommand::StageArchive {
                archive,
                writer: engine.config.writer_id,
                spec,
            })?;
            self.active = Some(Active {
                job,
                item,
                ticket,
                phase: Phase::Stage,
                session: None,
                batch: None,
                index: 0,
                plan: None,
            });
            return engine.report(job);
        }
        let mut active = self
            .active
            .take()
            .ok_or_else(|| AppError::integrity("upload disappeared"))?;
        match worker.poll(active.ticket)? {
            WorkerState::Active | WorkerState::TimedOutStillAlive => {
                self.active = Some(active);
                return engine.report(job);
            }
            WorkerState::Reaped => {
                return Err(AppError::new(
                    ErrorCode::Unavailable,
                    "worker reaped with uncertain publication; reconcile before retry",
                ));
            }
            WorkerState::Failed(error) => {
                if matches!(active.phase, Phase::Read)
                    && matches!(error.code, ErrorCode::Integrity | ErrorCode::Unsupported)
                    && let Some(entry) = active
                        .batch
                        .as_ref()
                        .and_then(|b| b.entries.get(active.index))
                {
                    engine
                        .catalog
                        .block_record(&entry.id.to_string(), &error.to_string())?;
                }

                let generation = engine.catalog.job(active.job)?.generation;
                engine.catalog.transition(
                    active.job,
                    generation,
                    JobTransition::Wait(
                        if error.code == ErrorCode::Conflict || error.code == ErrorCode::Integrity {
                            JobState::Conflict
                        } else {
                            JobState::WaitingNas
                        },
                        error.to_string(),
                    ),
                )?;
                self.active = Some(active);
                return engine.report(job);
            }
            WorkerState::Completed(reply) => {
                let command = match (active.phase, reply) {
                    (Phase::Stage, NasReply::Staged(_)) => {
                        if matches!(active.item.role, SourceRole::Companion(_)) {
                            active.phase = Phase::History;
                            NasCommand::StoreHistory {
                                root: engine.config.archive_root.clone(),
                                archive,
                                writer: engine.config.writer_id,
                                staged: staging_path(active.item.id, 1),
                                digest: active.item.digest.clone(),
                                size: active.item.size,
                            }
                        } else {
                            active.phase = Phase::Begin;
                            NasCommand::BeginArchive {
                                root: engine.config.archive_root.clone(),
                                archive,
                                writer: engine.config.writer_id,
                            }
                        }
                    }
                    (Phase::History, NasReply::Done) => {
                        active.phase = Phase::Begin;
                        NasCommand::BeginArchive {
                            root: engine.config.archive_root.clone(),
                            archive,
                            writer: engine.config.writer_id,
                        }
                    }
                    (Phase::Begin, NasReply::Session { session, .. }) => {
                        active.session = Some(session);
                        active.phase = Phase::List;
                        NasCommand::JournalNext {
                            session,
                            limit: 128,
                        }
                    }
                    (Phase::List, NasReply::Journal(batch)) => {
                        active.index = 0;
                        active.batch = Some(batch);
                        next_batch_command(engine, &mut active)?
                    }
                    (Phase::Read, NasReply::Commit(commit)) => {
                        engine.catalog.apply_commit(&commit)?;
                        active.index += 1;
                        next_batch_command(engine, &mut active)?
                    }
                    (Phase::Ack, NasReply::Done) => {
                        let session = session(&active)?;
                        if active.batch.as_ref().is_some_and(|b| b.end) {
                            if !matches!(
                                engine.catalog.publication_readiness()?,
                                PublicationReadiness::Ready
                            ) {
                                return Err(AppError::integrity(
                                    "incomplete shared journal blocks publication",
                                ));
                            }
                            let plan = prepare_plan(engine, &active)?;
                            active.plan = Some(plan.clone());
                            active.phase = Phase::Publish;
                            NasCommand::Publish { session, plan }
                        } else {
                            active.phase = Phase::List;
                            NasCommand::JournalNext {
                                session,
                                limit: 128,
                            }
                        }
                    }
                    (Phase::Publish, NasReply::Published(receipt)) => {
                        engine.catalog.apply_commit(&receipt.commit)?;
                        active.phase = Phase::End;
                        NasCommand::EndArchive {
                            session: session(&active)?,
                        }
                    }
                    (Phase::End, NasReply::Done) => {
                        engine.catalog.db.execute(
                            "UPDATE import_items SET archived=1,state='archived' WHERE id=?",
                            [active.item.id.to_string()],
                        )?;
                        if let Some(group) = active.item.group {
                            let group = engine.catalog.canonical_group(group)?;
                            engine.catalog.db.execute("INSERT INTO placements(id,group_id,path,digest,local_ready,archived) VALUES(?,?,?,?,?,1) ON CONFLICT(path,group_id) DO UPDATE SET archived=1",params![OperationId::new().to_string(),group.to_string(),active.item.destination.as_str(),active.item.digest.as_str(),active.item.local_ready])?;
                        }
                        let report = engine.report(active.job)?;
                        if report.archive_committed == report.selected && report.conflicting == 0 {
                            let generation = engine.catalog.job(active.job)?.generation;
                            engine.catalog.transition(
                                active.job,
                                generation,
                                JobTransition::Archived,
                            )?;
                        }
                        return engine.report(job);
                    }
                    _ => {
                        return Err(AppError::integrity(
                            "unexpected worker reply during archive upload",
                        ));
                    }
                };
                active.ticket = worker.submit(command)?;
            }
        }
        self.active = Some(active);
        engine.report(job)
    }
}
fn session(active: &Active) -> AppResult<OperationId> {
    active
        .session
        .ok_or_else(|| AppError::integrity("upload session missing"))
}
fn next_batch_command(engine: &ImportCoordinator, active: &mut Active) -> AppResult<NasCommand> {
    let session = session(active)?;
    let batch = active
        .batch
        .as_ref()
        .ok_or_else(|| AppError::integrity("journal batch missing"))?;
    while active.index < batch.entries.len() {
        let entry = &batch.entries[active.index];
        let known: bool = engine.catalog.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM commits WHERE id=? AND applied=1)",
            [entry.id.to_string()],
            |r| r.get(0),
        )?;
        if known {
            active.index += 1;
            continue;
        }
        active.phase = Phase::Read;
        return Ok(NasCommand::JournalRead {
            session,
            path: entry.path.clone(),
        });
    }
    active.phase = Phase::Ack;
    Ok(NasCommand::JournalAck {
        session,
        token: batch.token,
    })
}
fn prepare_plan(engine: &mut ImportCoordinator, active: &Active) -> AppResult<PublicationPlan> {
    let existing: Option<String> = engine
        .catalog
        .db
        .query_row(
            "SELECT plan FROM upload_plans WHERE item=?",
            [active.item.id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(plan) = existing {
        return Ok(serde_json::from_str(&plan)?);
    }
    let records = if let Some(group) = active.item.group {
        let group = engine.catalog.canonical_group(group)?;
        vec![
            ArchiveRecord::GroupCreated { id: group },
            ArchiveRecord::MemberAttached {
                group,
                member: OriginalMember {
                    digest: active.item.digest.clone(),
                    size: active.item.size,
                    path: active.item.destination.clone(),
                    role: match active.item.role {
                        SourceRole::Original(role) => role,
                        _ => return Err(AppError::integrity("companion group mismatch")),
                    },
                },
                evidence: PairEvidence::ObservedPlacement {
                    path: active.item.destination.clone(),
                    digest: active.item.digest.clone(),
                },
            },
        ]
    } else {
        let sidecar = SidecarId::new();
        let version = Version {
            id: VersionId::new(),
            sidecar,
            parents: vec![],
            body: VersionBody::Content {
                digest: active.item.digest.clone(),
                size: active.item.size,
            },
            origin: VersionOrigin::Observed,
        };
        vec![
            ArchiveRecord::SidecarCreated {
                id: sidecar,
                group: None,
                path: active.item.destination.clone(),
                owner: "preserved-source-companion".into(),
            },
            ArchiveRecord::VersionAdded {
                version: version.clone(),
                operation: OperationId(active.item.id.0),
            },
            ArchiveRecord::HeadsChanged {
                sidecar,
                expected: vec![],
                heads: vec![version.id],
            },
        ]
    };
    let tx = engine.catalog.db.transaction()?;
    let sequence:u64=tx.query_row("INSERT INTO writer_sequences(writer,sequence) VALUES(?,1) ON CONFLICT(writer) DO UPDATE SET sequence=sequence+1 RETURNING sequence",[engine.config.writer_id.to_string()],|r|r.get(0))?;
    let mut dependencies = Vec::new();
    let mut q=tx.prepare("SELECT id FROM commits WHERE applied=1 AND NOT EXISTS(SELECT 1 FROM dependencies d JOIN commits child ON child.id=d.commit_id WHERE d.dependency=commits.id AND child.applied=1) LIMIT 128")?;
    for row in q.query_map([], |r| r.get::<_, String>(0))? {
        dependencies.push(serde_json::from_str(&format!("\"{}\"", row?))?);
    }
    drop(q);
    let commit = CommitEnvelope::new(
        engine
            .config
            .archive_id
            .ok_or_else(|| AppError::invalid("archive missing"))?,
        engine.config.writer_id,
        sequence,
        dependencies,
        records,
    )?;
    let staged = if matches!(active.item.role, SourceRole::Original(_)) {
        vec![StagedMember {
            member: OriginalMember {
                digest: active.item.digest.clone(),
                size: active.item.size,
                path: active.item.destination.clone(),
                role: match active.item.role {
                    SourceRole::Original(role) => role,
                    _ => unreachable!(),
                },
            },
            staged: staging_path(active.item.id, 1),
        }]
    } else {
        vec![]
    };
    let plan = PublicationPlan {
        commit,
        staged,
        attempt: OperationId::new(),
    };
    tx.execute(
        "INSERT INTO upload_plans(item,plan) VALUES(?,?)",
        params![active.item.id.to_string(), serde_json::to_string(&plan)?],
    )?;
    tx.commit()?;
    Ok(plan)
}
