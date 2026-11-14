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
#[derive(Clone)]
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
    JpegInspect,
    Stage,
    History,
    Begin,
    List,
    Read,
    Ack,
    Publish,
    End,
    Abort,
    Drain,
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
    fn next_upload(&mut self, job: JobId) -> AppResult<Option<ImportItem>> {
        for _ in 0..128 {
            let row:Option<UploadItemRow>=self.catalog.db.query_row("SELECT id,source_root,source_path,digest,size,role,group_id,destination,local_ready,archived FROM import_items WHERE parent=? AND archived=0 AND state NOT IN('conflict','image_duplicate') AND (group_id IS NOT NULL OR EXISTS(SELECT 1 FROM import_scans WHERE job=parent AND complete=1)) ORDER BY group_id IS NULL,rowid LIMIT 1",[job.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?))).optional()?;
            let Some((id, root, path, digest, size, role, group, destination, local_ready, _)) =
                row
            else {
                return Ok(None);
            };
            let item = ImportItem {
                id: serde_json::from_value(id.into())?,
                source: SourceLocation {
                    root: root.into(),
                    path: ArchivePath::parse(&path)?,
                },
                digest: Digest::parse(&digest)?,
                size,
                role: serde_json::from_str(&role)?,
                group: group
                    .map(|g| serde_json::from_value(g.into()))
                    .transpose()?,
                destination: ArchivePath::parse(&destination)?,
                local_ready,
            };
            if matches!(item.role, SourceRole::Companion(_)) {
                let owner = source_owner(self, &item, job)?;
                let target = owner
                    .as_ref()
                    .map(|(_, path, _)| path)
                    .unwrap_or(&item.destination);
                let archived:bool=self.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM sidecars s JOIN heads h ON h.sidecar=s.id JOIN versions v ON v.id=h.version JOIN archived_versions a ON a.version=v.id WHERE s.path=? AND json_extract(v.body,'$.Content.digest')=? AND json_extract(v.body,'$.Content.size')=?)",params![target.as_str(),item.digest.as_str(),item.size],|r|r.get(0))?;
                if archived {
                    self.catalog.db.execute(
                        "UPDATE import_items SET archived=1,state='accounted' WHERE id=?",
                        [item.id.to_string()],
                    )?;
                    self.catalog.db.execute(
                        "UPDATE import_stats SET recognized=recognized+1 WHERE job=?",
                        [job.to_string()],
                    )?;
                    continue;
                }
            }
            return Ok(Some(item));
        }
        Ok(None)
    }
}
impl UploadDriver {
    pub fn cancel(&mut self, worker: &mut WorkerSupervisor, job: JobId) -> AppResult<()> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        if active.job != job {
            self.active = Some(active);
            return Ok(());
        }
        match worker.poll(active.ticket)? {
            WorkerState::Active | WorkerState::TimedOutStillAlive | WorkerState::Reaped => {
                self.active = Some(active)
            }
            WorkerState::Completed(reply) => {
                if let NasReply::Session { session, .. } = reply {
                    active.session = Some(session);
                }
                if !matches!(active.phase, Phase::End | Phase::Abort)
                    && let Some(session) = active.session
                {
                    active.phase = Phase::Abort;
                    active.ticket = worker.submit(NasCommand::EndArchive { session })?;
                    self.active = Some(active);
                }
            }
            WorkerState::Failed(_) => {
                if matches!(active.phase, Phase::End | Phase::Abort) {
                    worker.mark_uncertain();
                    self.active = Some(active);
                } else if let Some(session) = active.session {
                    active.phase = Phase::Abort;
                    active.ticket = worker.submit(NasCommand::EndArchive { session })?;
                    self.active = Some(active);
                }
            }
        }
        Ok(())
    }
    pub fn active_job(&self) -> Option<JobId> {
        self.active.as_ref().map(|active| active.job)
    }

    pub fn advance(
        &mut self,
        engine: &mut ImportCoordinator,
        worker: &mut WorkerSupervisor,
        job: JobId,
    ) -> AppResult<ImportReport> {
        let backup = self.active.clone();
        let result = self.advance_inner(engine, worker, job);
        if result.is_err()
            && self.active.is_none()
            && let Some(mut active) = backup
            && let Some(ticket) = worker.latest_ticket()
        {
            active.ticket = ticket;
            active.phase = Phase::Drain;
            self.active = Some(active);
        }
        result
    }
    fn advance_inner(
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
            if let Some(check) = engine.pending_jpeg_check(item.id)? {
                let strict = matches!(
                    engine.catalog.job(job)?.request.kind,
                    JobKind::Import { strict: true, .. }
                );
                if !strict
                    && let Some(image) = engine.catalog.jpeg_fingerprint(&check.candidate.digest)?
                {
                    engine.finish_jpeg_check(
                        &item,
                        &JpegInspection {
                            digest: check.candidate.digest,
                            size: check.candidate.size,
                            image,
                        },
                    )?;
                    return engine.report(job);
                }
                let ticket = worker.submit(NasCommand::InspectJpegArchive {
                    root: engine.config.archive_root.clone(),
                    archive,
                    writer: engine.config.writer_id,
                    path: check.candidate.path,
                    expected_digest: check.candidate.digest,
                    expected_size: check.candidate.size,
                })?;
                self.active = Some(Active {
                    job,
                    item,
                    ticket,
                    phase: Phase::JpegInspect,
                    session: None,
                    batch: None,
                    index: 0,
                    plan: None,
                });
                return engine.report(job);
            }
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
                if matches!(active.phase, Phase::List) {
                    engine
                        .catalog
                        .block_journal_enumeration(&error.to_string())?;
                }
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
                if matches!(error.code, ErrorCode::Conflict | ErrorCode::Integrity)
                    && !matches!(
                        active.phase,
                        Phase::Read | Phase::List | Phase::Ack | Phase::Abort
                    )
                {
                    let tx = engine.catalog.db.transaction()?;
                    tx.execute(
                        "UPDATE import_items SET state='conflict' WHERE id=?",
                        [active.item.id.to_string()],
                    )?;
                    tx.execute(
                        "DELETE FROM jpeg_checks WHERE item=?",
                        [active.item.id.to_string()],
                    )?;
                    tx.commit()?;
                    if let SourceRole::Original(role) = active.item.role {
                        engine.catalog.record_ambiguity(
                            "original_collision",
                            active.item.group,
                            serde_json::to_value(OriginalMember {
                                path: active.item.destination.clone(),
                                digest: active.item.digest.clone(),
                                size: active.item.size,
                                role,
                            })?,
                        )?;
                    }
                }
                if let Some(session) = active.session {
                    if matches!(active.phase, Phase::Abort) {
                        worker.mark_uncertain();
                        self.active = Some(active);
                    } else {
                        active.phase = Phase::Abort;
                        active.ticket = worker.submit(NasCommand::EndArchive { session })?;
                        self.active = Some(active);
                    }
                }
                return engine.report(job);
            }
            WorkerState::Completed(reply) => {
                let command = match (active.phase, reply) {
                    (Phase::Drain, reply) => {
                        if let NasReply::Session { session, .. } = reply {
                            active.session = Some(session);
                        }
                        if let Some(session) = active.session {
                            active.phase = Phase::Abort;
                            NasCommand::EndArchive { session }
                        } else {
                            return engine.report(job);
                        }
                    }
                    (Phase::Abort, NasReply::Done) => return engine.report(job),
                    (Phase::JpegInspect, NasReply::JpegInspection(inspection)) => {
                        engine.finish_jpeg_check(&active.item, &inspection)?;
                        return engine.report(job);
                    }
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
                        if let Some(group) = active.item.group {
                            engine.catalog.refresh_protection(group)?;
                        }
                        let report = engine.report(active.job)?;
                        if report.pending_archive == 0
                            && report.conflicting == 0
                            && engine.scan_complete(active.job)?
                        {
                            let generation = engine.catalog.job(active.job)?.generation;
                            engine.catalog.transition(
                                active.job,
                                generation,
                                JobTransition::Archived,
                            )?;
                        }
                        if report.waiting == 0
                            && report.conflicting == 0
                            && engine.scan_complete(active.job)?
                        {
                            let record = engine.catalog.job(active.job)?;
                            engine.catalog.transition(
                                active.job,
                                record.generation,
                                JobTransition::Ingested,
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
    let mut records = if let Some(group) = active.item.group {
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
        let owner = source_owner(engine, &active.item, active.job)?;
        let (group, sidecar_path, owner_name) = owner.unwrap_or((
            None,
            active.item.destination.clone(),
            "preserved-source-companion".into(),
        ));
        let sidecar = engine.catalog.managed_sidecar_id(&sidecar_path)?;
        let expected = engine.catalog.heads(sidecar)?;
        let body = VersionBody::Content {
            digest: active.item.digest.clone(),
            size: active.item.size,
        };
        let equal = expected
            .iter()
            .map(|id| engine.catalog.version(*id))
            .collect::<AppResult<Vec<_>>>()?
            .iter()
            .any(|v| v.body == body);
        let mut records = vec![ArchiveRecord::SidecarCreated {
            id: sidecar,
            group,
            path: sidecar_path.clone(),
            owner: owner_name.clone(),
        }];
        if !equal {
            let version = Version {
                id: VersionId::new(),
                sidecar,
                parents: vec![],
                body,
                origin: VersionOrigin::Observed,
            };
            let mut heads = expected.clone();
            heads.push(version.id);
            heads.sort();
            records.push(ArchiveRecord::VersionAdded {
                version: version.clone(),
                operation: OperationId(active.item.id.0),
            });
            records.push(ArchiveRecord::HeadsChanged {
                sidecar,
                expected,
                heads: heads.clone(),
            });
            if heads.len() > 1 {
                records.push(ArchiveRecord::ConflictRecorded {
                    id: ConflictId::new(),
                    group,
                    kind: "sidecar".into(),
                    heads,
                });
            } else if group.is_some() && crate::archive::owns_projection(&owner_name, &sidecar_path)
            {
                records.push(ArchiveRecord::ProjectionPlanned {
                    plan: ProjectionPlan {
                        sidecar,
                        path: sidecar_path.clone(),
                        version: version.id,
                        expected: VersionBody::Reset,
                        body: version.body,
                        owner: owner_name.clone(),
                    },
                });
            }
        }
        if group.is_none() {
            records.push(ArchiveRecord::AmbiguityRecorded {
                id: ConflictId(active.item.id.0),
                kind: "orphan_sidecar".into(),
                group: None,
                details: serde_json::json!({"sidecar":sidecar,"path":active.item.destination}),
            });
        }
        records
    };
    records.extend(engine.catalog.sequence_records_for(&active.item.digest)?);
    if let Some(result) = engine.catalog.metadata(&active.item.digest)? {
        records.push(ArchiveRecord::MetadataCached { result });
    }
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

fn source_owner(
    engine: &ImportCoordinator,
    item: &ImportItem,
    parent: JobId,
) -> AppResult<Option<(Option<GroupId>, ArchivePath, String)>> {
    use crate::edit::{OwnerDecision, SidecarKind, resolve_owner};
    let mut query=engine.catalog.db.prepare("SELECT i.source_path,coalesce(d.canonical_path,i.destination),coalesce(d.canonical_digest,i.digest),coalesce(d.canonical_size,i.size),i.role,coalesce(d.canonical_group,i.group_id) FROM import_items i LEFT JOIN jpeg_duplicates d ON d.item=i.id WHERE i.parent=? AND i.source_root=? AND coalesce(d.canonical_group,i.group_id) IS NOT NULL AND (i.archived=1 OR (i.state='image_duplicate' AND d.item IS NOT NULL)) AND substr(?,1,length(i.source_path))=i.source_path ORDER BY length(i.source_path) DESC LIMIT 128")?;
    let rows = query
        .query_map(
            params![
                parent.to_string(),
                item.source.root.to_str(),
                item.source.path.as_str()
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, u64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    for (source, destination, digest, size, role, group) in rows {
        let SourceRole::Original(role) = serde_json::from_str(&role)? else {
            continue;
        };
        let member = OriginalMember {
            path: ArchivePath::parse(&source)?,
            digest: Digest::parse(&digest)?,
            size,
            role,
        };
        if let OwnerDecision::Owned {
            kind, virtual_copy, ..
        } = resolve_owner(&item.source.path, &[member])
        {
            let suffix = &item.source.path.as_str()[source.len()..];
            let path = ArchivePath::parse(&format!("{destination}{suffix}"))?;
            let group = engine
                .catalog
                .canonical_group(serde_json::from_str(&format!("\"{group}\""))?)?;
            let owner = match kind {
                SidecarKind::RapidRaw if virtual_copy.is_some() => "rapidraw-virtual",
                SidecarKind::RapidRaw => "rapidraw-base",
                SidecarKind::Rrexif => "rrexif",
                SidecarKind::RawXmp => "raw-xmp",
                SidecarKind::JpegXmpExternal => "jpeg-xmp-external",
                SidecarKind::Opaque => "opaque",
                SidecarKind::Generated => GENERATED_OUTPUT_OWNER,
            };
            return Ok(Some((Some(group), path, owner.into())));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    #[test]
    fn already_archived_companion_receipt_is_reuse_without_an_upload() {
        let temp = tempfile::tempdir().unwrap();
        for directory in ["source", "local", "state", "archive"] {
            std::fs::create_dir(temp.path().join(directory)).unwrap();
        }
        let config = crate::config::Config::new(
            temp.path().join("local"),
            temp.path().join("state"),
            temp.path().join("archive"),
        );
        let state =
            crate::fs::RootHandle::open(&config.state_root, crate::fs::RootPolicy::Local).unwrap();
        let catalog = Catalog::open(&state).unwrap();
        let mut engine = ImportCoordinator::new(config, catalog);
        let source = temp.path().join("source");
        std::fs::write(source.join("orphan.rrdata"), b"sidecar").unwrap();
        let report = engine
            .prepare(ImportRequest {
                source_path: source,
                strict: false,
                dry_run: false,
                operation_id: OperationId::new(),
            })
            .unwrap();
        let job = report.job.unwrap();
        let destination: String = engine
            .catalog
            .db
            .query_row(
                "SELECT destination FROM import_items WHERE parent=?",
                [job.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        let sidecar = SidecarId::new();
        let version = VersionId::new();
        let db = &engine.catalog.db;
        db.execute(
            "INSERT INTO sidecars(id,path,owner) VALUES(?,?,?)",
            params![sidecar.to_string(), destination, "fixture"],
        )
        .unwrap();
        db.execute(
            "INSERT INTO versions(id,sidecar,body,origin,operation) VALUES(?,?,?,?,?)",
            params![
                version.to_string(),
                sidecar.to_string(),
                serde_json::to_string(&VersionBody::Content {
                    digest: Digest::of(b"sidecar"),
                    size: 7
                })
                .unwrap(),
                "fixture",
                OperationId::new().to_string()
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO heads(sidecar,version) VALUES(?,?)",
            params![sidecar.to_string(), version.to_string()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO archived_versions(version) VALUES(?)",
            [version.to_string()],
        )
        .unwrap();
        db.execute(
            "UPDATE import_items SET local_ready=1 WHERE parent=?",
            [job.to_string()],
        )
        .unwrap();
        assert!(engine.next_upload(job).unwrap().is_none());
        let reused = engine.report(job).unwrap();
        assert_eq!(reused.archive_committed, 1);
        assert_eq!(reused.archive_reused, 1);
        assert_eq!(reused.recognized, 1);
        assert_eq!(reused.transfer_bytes, 0);
        assert_eq!(reused.local_ready, 1);
        assert_eq!(serde_json::to_value(&reused).unwrap()["copy_ready"], 0);
    }
}
