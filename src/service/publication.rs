use crate::{
    archive::*,
    catalog::*,
    config::Config,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::RootHandle,
    import::ImportCoordinator,
    transfer::*,
};
use rusqlite::{OptionalExtension, params};
#[derive(Default)]
pub(crate) struct SharedPublisher {
    active: Option<Active>,
    pub reason: Option<String>,
}
struct Active {
    candidate: Candidate,
    ticket: WorkerTicket,
    phase: Phase,
    session: Option<OperationId>,
    batch: Option<JournalBatch>,
    index: usize,
    plan: Option<VersionPublication>,
}
#[derive(Clone)]
enum Candidate {
    Version(VersionId),
    Records(OperationId),
    Projection(SidecarId, VersionId),
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
    Repair,
    End,
    Abort,
}
impl SharedPublisher {
    pub fn busy(&self) -> bool {
        self.active.is_some()
    }
    pub fn advance(
        &mut self,
        engine: &mut ImportCoordinator,
        state: &RootHandle,
        worker: &mut WorkerSupervisor,
    ) -> AppResult<()> {
        let config = engine.config.clone();
        let Some(archive) = config.archive_id else {
            return Ok(());
        };
        if self.active.is_none() {
            if !worker.available()? {
                return Ok(());
            }
            let Some(candidate) = select(&engine.catalog)? else {
                return Ok(());
            };
            let (phase, command) = if let Candidate::Version(version) = candidate {
                match engine.catalog.version(version)?.body {
                    VersionBody::Content { digest, size } => {
                        let source = crate::edit::snapshot_path(&digest)?;
                        (
                            Phase::Stage,
                            NasCommand::StageArchive {
                                archive,
                                writer: config.writer_id,
                                spec: TransferSpec {
                                    job: JobId(version.0),
                                    generation: 1,
                                    source: SourceLocation {
                                        root: config.state_root.clone(),
                                        path: source,
                                    },
                                    destination_root: config.archive_root.clone(),
                                    staged_path: staging_path(JobId(version.0), 1),
                                    expected_digest: digest,
                                    expected_size: size,
                                },
                            },
                        )
                    }
                    VersionBody::Reset => (Phase::Begin, begin(&config)?),
                }
            } else {
                (Phase::Begin, begin(&config)?)
            };
            let ticket = worker.submit(command)?;
            self.active = Some(Active {
                candidate,
                ticket,
                phase,
                session: None,
                batch: None,
                index: 0,
                plan: None,
            });
            return Ok(());
        }
        let mut active = self
            .active
            .take()
            .ok_or_else(|| AppError::integrity("metadata publication state missing"))?;
        let reply = match worker.poll(active.ticket)? {
            WorkerState::Active | WorkerState::TimedOutStillAlive => {
                self.active = Some(active);
                return Ok(());
            }
            WorkerState::Reaped => {
                self.reason = Some(
                    "publication worker disappeared; abandoned lock requires explicit recovery"
                        .into(),
                );
                self.active = Some(active);
                return Ok(());
            }
            WorkerState::Failed(error) => {
                self.reason = Some(error.to_string());
                if matches!(active.phase, Phase::Read)
                    && let Some(entry) = active
                        .batch
                        .as_ref()
                        .and_then(|b| b.entries.get(active.index))
                {
                    engine
                        .catalog
                        .block_record(&entry.id.to_string(), &error.to_string())?;
                }
                if let Some(session) = active.session
                    && !matches!(active.phase, Phase::Abort)
                {
                    active.phase = Phase::Abort;
                    active.ticket = worker.submit(NasCommand::EndArchive { session })?;
                    self.active = Some(active);
                } else if active.session.is_some() {
                    worker.mark_uncertain();
                    self.active = Some(active);
                } else {
                    defer(&mut engine.catalog, &active.candidate, &error.to_string())?;
                }
                return Ok(());
            }
            WorkerState::Completed(reply) => reply,
        };
        let command = match (active.phase, reply) {
            (Phase::Stage, NasReply::Staged(receipt)) => {
                let Candidate::Version(version) = active.candidate else {
                    return Err(AppError::integrity("version stage has wrong candidate"));
                };
                let expected = engine.catalog.version(version)?.body;
                if expected
                    != (VersionBody::Content {
                        digest: receipt.digest.clone(),
                        size: receipt.size,
                    })
                    || !receipt.matches(JobId(version.0), 1)
                {
                    return Err(AppError::integrity("metadata staging receipt mismatch"));
                }
                active.phase = Phase::History;
                NasCommand::StoreHistory {
                    root: config.archive_root.clone(),
                    archive,
                    writer: config.writer_id,
                    staged: receipt.staged_path,
                    digest: receipt.digest,
                    size: receipt.size,
                }
            }
            (Phase::History, NasReply::Done) => {
                active.phase = Phase::Begin;
                begin(&config)?
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
                active.batch = Some(batch);
                active.index = 0;
                replay_next(&engine.catalog, &mut active)?
            }
            (Phase::Read, NasReply::Commit(commit)) => {
                engine.catalog.apply_commit(&commit)?;
                active.index += 1;
                replay_next(&engine.catalog, &mut active)?
            }
            (Phase::Ack, NasReply::Done) => {
                if active.batch.as_ref().is_some_and(|b| b.end) {
                    let _ = engine.catalog.drain_pending(128)?;
                    if !matches!(
                        engine.catalog.publication_readiness()?,
                        PublicationReadiness::Ready
                    ) {
                        self.reason = Some(
                            "shared publication waits for complete valid journal replay".into(),
                        );
                        active.phase = Phase::Abort;
                        NasCommand::EndArchive {
                            session: session(&active)?,
                        }
                    } else if let Candidate::Projection(sidecar, version) = active.candidate {
                        if engine.catalog.heads(sidecar)? != vec![version] {
                            active.phase = Phase::End;
                            NasCommand::EndArchive {
                                session: session(&active)?,
                            }
                        } else {
                            let (plan,envelope):(String,Vec<u8>)=engine.catalog.db.query_row("SELECT p.committed,c.envelope FROM projections p JOIN projection_commits pc ON pc.sidecar=p.sidecar JOIN commits c ON c.id=pc.commit_id WHERE p.sidecar=? AND pc.version=?",params![sidecar.to_string(),version.to_string()],|r|Ok((r.get(0)?,r.get(1)?)))?;
                            let plan: ProjectionPlan = serde_json::from_str(&plan)?;
                            let commit = CommitEnvelope::decode(&envelope, archive)?;
                            if plan.sidecar != sidecar || plan.version != version {
                                return Err(AppError::integrity(
                                    "projection repair identity changed",
                                ));
                            }
                            active.phase = Phase::Repair;
                            NasCommand::RepairProjection {
                                session: session(&active)?,
                                commit,
                                plan,
                            }
                        }
                    } else {
                        match prepare(engine, &active.candidate) {
                            Ok(plan) => {
                                let command = NasCommand::PublishVersions {
                                    session: session(&active)?,
                                    plan: plan.clone(),
                                };
                                active.plan = Some(plan);
                                active.phase = Phase::Publish;
                                command
                            }
                            Err(error) => {
                                self.reason = Some(error.to_string());
                                active.phase = Phase::Abort;
                                NasCommand::EndArchive {
                                    session: session(&active)?,
                                }
                            }
                        }
                    }
                } else {
                    active.phase = Phase::List;
                    NasCommand::JournalNext {
                        session: session(&active)?,
                        limit: 128,
                    }
                }
            }
            (Phase::Publish, NasReply::Published(receipt)) => {
                engine.catalog.apply_commit(&receipt.commit)?;
                acknowledge(&mut engine.catalog, &active, &receipt, state)?;
                active.phase = Phase::End;
                NasCommand::EndArchive {
                    session: session(&active)?,
                }
            }
            (Phase::Repair, NasReply::Projection(outcome)) => {
                let Candidate::Projection(sidecar, version) = active.candidate else {
                    return Err(AppError::integrity("repair has no projection"));
                };
                if let ProjectionOutcome::Conflict {
                    observed,
                    preserved,
                } = outcome
                {
                    let group: Option<String> = engine.catalog.db.query_row(
                        "SELECT group_id FROM sidecars WHERE id=?",
                        [sidecar.to_string()],
                        |r| r.get(0),
                    )?;
                    let group = group
                        .map(|g| serde_json::from_str(&format!("\"{g}\"")))
                        .transpose()?;
                    engine.catalog.record_ambiguity("nas_projection",group,serde_json::json!({"sidecar":sidecar,"version":version,"observed":observed,"preserved":preserved}))?;
                    self.reason = Some(
                        "NAS sidecar differs from committed predecessor; all bytes retained".into(),
                    );
                    active.phase = Phase::Abort;
                } else {
                    engine.catalog.db.execute(
                        "UPDATE nas_projections SET applied=1 WHERE sidecar=? AND version=?",
                        params![sidecar.to_string(), version.to_string()],
                    )?;
                    active.phase = Phase::End;
                }
                NasCommand::EndArchive {
                    session: session(&active)?,
                }
            }
            (Phase::End, NasReply::Done) => {
                self.reason = None;
                return Ok(());
            }
            (Phase::Abort, NasReply::Done) => {
                defer(
                    &mut engine.catalog,
                    &active.candidate,
                    self.reason
                        .as_deref()
                        .unwrap_or("metadata publication blocked"),
                )?;
                return Ok(());
            }
            _ => {
                return Err(AppError::integrity(
                    "unexpected metadata publication worker reply",
                ));
            }
        };
        active.ticket = worker.submit(command)?;
        self.active = Some(active);
        Ok(())
    }
}
fn candidate_key(candidate: &Candidate) -> String {
    match candidate {
        Candidate::Version(v) => format!("version:{v}"),
        Candidate::Projection(sidecar, version) => format!("projection:{sidecar}:{version}"),
        Candidate::Records(op) => format!("records:{op}"),
    }
}
fn defer(catalog: &mut Catalog, candidate: &Candidate, reason: &str) -> AppResult<()> {
    let now = SystemClock::default().now();
    catalog.db.execute("INSERT INTO publication_failures(source,next_attempt,failures,reason) VALUES(?,?,1,?) ON CONFLICT(source) DO UPDATE SET next_attempt=excluded.next_attempt+min(300,1<<min(failures,8)),failures=failures+1,reason=excluded.reason",params![candidate_key(candidate),now+1,reason])?;
    Ok(())
}
fn select(catalog: &Catalog) -> AppResult<Option<Candidate>> {
    let now = SystemClock::default().now();
    let projection:Option<(String,String)>=catalog.db.query_row("SELECT n.sidecar,n.version FROM nas_projections n WHERE n.applied=0 AND EXISTS(SELECT 1 FROM heads h WHERE h.sidecar=n.sidecar AND h.version=n.version) AND (SELECT count(*) FROM heads h WHERE h.sidecar=n.sidecar)=1 AND NOT EXISTS(SELECT 1 FROM publication_failures f WHERE f.source='projection:'||n.sidecar||':'||n.version AND f.next_attempt>?) ORDER BY n.rowid LIMIT 1",[now],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((sidecar, version)) = projection {
        return Ok(Some(Candidate::Projection(
            serde_json::from_str(&format!("\"{sidecar}\""))?,
            serde_json::from_str(&format!("\"{version}\""))?,
        )));
    }
    let raw:Option<String>=catalog.db.query_row("SELECT operation FROM pending_shared_records p WHERE committed=0 AND NOT EXISTS(SELECT 1 FROM publication_failures f WHERE f.source='records:'||p.operation AND f.next_attempt>?) ORDER BY rowid LIMIT 1",[now],|r|r.get(0)).optional()?;
    if let Some(op) = raw {
        return Ok(Some(Candidate::Records(serde_json::from_str(&format!(
            "\"{op}\""
        ))?)));
    }
    let version:Option<String>=catalog.db.query_row("SELECT v.id FROM versions v JOIN sidecars s ON s.id=v.sidecar WHERE NOT EXISTS(SELECT 1 FROM archived_versions a WHERE a.version=v.id) AND NOT EXISTS(SELECT 1 FROM version_parents p WHERE p.version=v.id AND NOT EXISTS(SELECT 1 FROM archived_versions a WHERE a.version=p.parent)) AND (s.group_id IS NULL OR EXISTS(SELECT 1 FROM members m JOIN placements p ON p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest WHERE m.group_id=s.group_id AND p.archived=1 AND substr(s.path,1,length(m.path))=m.path)) AND NOT EXISTS(SELECT 1 FROM publication_failures f WHERE f.source='version:'||v.id AND f.next_attempt>?) ORDER BY v.rowid LIMIT 1",[now],|r|r.get(0)).optional()?;
    version
        .map(|v| {
            Ok(Candidate::Version(serde_json::from_str(&format!(
                "\"{v}\""
            ))?))
        })
        .transpose()
}
fn begin(config: &Config) -> AppResult<NasCommand> {
    Ok(NasCommand::BeginArchive {
        root: config.archive_root.clone(),
        archive: config
            .archive_id
            .ok_or_else(|| AppError::invalid("archive identity required"))?,
        writer: config.writer_id,
    })
}
fn session(active: &Active) -> AppResult<OperationId> {
    active
        .session
        .ok_or_else(|| AppError::integrity("metadata publication session missing"))
}
fn replay_next(catalog: &Catalog, active: &mut Active) -> AppResult<NasCommand> {
    let batch = active
        .batch
        .as_ref()
        .ok_or_else(|| AppError::integrity("metadata journal batch missing"))?;
    while active.index < batch.entries.len() {
        let entry = &batch.entries[active.index];
        let known: bool = catalog.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM commits WHERE id=? AND applied=1)",
            [entry.id.to_string()],
            |r| r.get(0),
        )?;
        if known {
            active.index += 1;
        } else {
            active.phase = Phase::Read;
            return Ok(NasCommand::JournalRead {
                session: session(active)?,
                path: entry.path.clone(),
            });
        }
    }
    active.phase = Phase::Ack;
    Ok(NasCommand::JournalAck {
        session: session(active)?,
        token: batch.token,
    })
}
fn prepare(engine: &mut ImportCoordinator, candidate: &Candidate) -> AppResult<VersionPublication> {
    let mut projections = vec![];
    let mut records = vec![];
    match candidate {
        Candidate::Projection(..) => {
            return Err(AppError::integrity(
                "projection repair cannot create a new commit",
            ));
        }
        Candidate::Records(operation) => {
            let text: String = engine.catalog.db.query_row(
                "SELECT records FROM pending_shared_records WHERE operation=?",
                [operation.to_string()],
                |r| r.get(0),
            )?;
            records = serde_json::from_str(&text)?;
            for record in records.clone() {
                if let ArchiveRecord::MappingRecorded {
                    path,
                    digest,
                    group,
                    ..
                } = &record
                {
                    let archived:bool=engine.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM placements WHERE group_id=? AND path=? AND digest=? AND archived=1)",params![group.to_string(),path.as_str(),digest.as_str()],|r|r.get(0))?;
                    if !archived {
                        return Err(AppError::new(
                            ErrorCode::Unavailable,
                            "shared mapping waits for exact original archival evidence",
                        ));
                    }
                }
                if let ArchiveRecord::AmbiguityResolved { request } = record {
                    let conflict = engine
                        .catalog
                        .conflicts(true)?
                        .into_iter()
                        .find(|c| c.id == request.conflict)
                        .ok_or_else(|| AppError::invalid("resolution ambiguity disappeared"))?;
                    let mut prefix = vec![];
                    for (group, _) in &request.expected_memberships {
                        if !engine.catalog.group(*group)?.members.is_empty() {
                            let all:bool=engine.catalog.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM members m WHERE group_id=? AND NOT EXISTS(SELECT 1 FROM placements p WHERE p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest AND p.archived=1))",[group.to_string()],|r|r.get(0))?;
                            if !all {
                                return Err(AppError::new(
                                    ErrorCode::Unavailable,
                                    "association waits for original archival evidence",
                                ));
                            }
                        }
                        prefix.push(ArchiveRecord::GroupCreated { id: *group });
                    }
                    let shared: bool = engine.catalog.db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM published_conflicts WHERE id=?)",
                        [conflict.id.to_string()],
                        |r| r.get(0),
                    )?;
                    if !shared {
                        prefix.push(ArchiveRecord::AmbiguityRecorded {
                            id: conflict.id,
                            kind: conflict.kind,
                            group: conflict.group,
                            details: conflict.details,
                        });
                    }
                    prefix.extend(records);
                    records = prefix;
                }
            }
        }
        Candidate::Version(version_id) => {
            let version = engine.catalog.version(*version_id)?;
            let (group,path,owner,operation):(Option<String>,String,String,String)=engine.catalog.db.query_row("SELECT s.group_id,s.path,s.owner,v.operation FROM sidecars s JOIN versions v ON v.sidecar=s.id WHERE v.id=?",[version.id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            let group = group
                .map(|g| serde_json::from_str(&format!("\"{g}\"")))
                .transpose()?
                .map(|g| engine.catalog.canonical_group(g))
                .transpose()?;
            let shared: bool = engine.catalog.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM published_sidecars WHERE id=?)",
                [version.sidecar.to_string()],
                |r| r.get(0),
            )?;
            if !shared {
                records.push(ArchiveRecord::SidecarCreated {
                    id: version.sidecar,
                    group,
                    path: ArchivePath::parse(&path)?,
                    owner: owner.clone(),
                });
            }
            records.push(ArchiveRecord::VersionAdded {
                version: version.clone(),
                operation: serde_json::from_str(&format!("\"{operation}\""))?,
            });
            let is_current = engine.catalog.local_head(version.sidecar)? == Some(version.id);
            let is_pending: bool = engine.catalog.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM rating_baselines WHERE pending_version=?)",
                [version.id.to_string()],
                |r| r.get(0),
            )?;
            if is_current || is_pending {
                let expected = engine.catalog.heads(version.sidecar)?;
                let mut heads = vec![];
                for head in &expected {
                    if !engine.catalog.is_ancestor(*head, version.id)? {
                        heads.push(*head);
                    }
                }
                heads.push(version.id);
                heads.sort();
                heads.dedup();
                records.push(ArchiveRecord::HeadsChanged {
                    sidecar: version.sidecar,
                    expected: expected.clone(),
                    heads: heads.clone(),
                });
                if heads.len() > 1 {
                    let digest = blake3::hash(
                        format!("sidecar-conflict:{}:{heads:?}", version.sidecar).as_bytes(),
                    );
                    let mut bytes = [0u8; 16];
                    bytes.copy_from_slice(&digest.as_bytes()[..16]);
                    records.push(ArchiveRecord::ConflictRecorded {
                        id: ConflictId(uuid::Uuid::from_bytes(bytes)),
                        group,
                        kind: "sidecar".into(),
                        heads,
                    });
                } else if group.is_some()
                    && owner != "jpeg-xmp-external"
                    && !path.starts_with(".photo-sorter/")
                {
                    let previous: Option<String> = engine
                        .catalog
                        .db
                        .query_row(
                            "SELECT committed FROM projections WHERE sidecar=?",
                            [version.sidecar.to_string()],
                            |r| r.get(0),
                        )
                        .optional()?
                        .flatten();
                    let expected = previous
                        .map(|text| serde_json::from_str::<ProjectionPlan>(&text).map(|p| p.body))
                        .transpose()?
                        .unwrap_or(VersionBody::Reset);
                    let plan = ProjectionPlan {
                        sidecar: version.sidecar,
                        path: ArchivePath::parse(&path)?,
                        version: version.id,
                        expected,
                        body: version.body.clone(),
                        owner,
                    };
                    records.push(ArchiveRecord::ProjectionPlanned { plan: plan.clone() });
                    projections.push(plan);
                }
            }
        }
    }
    let source = candidate_key(candidate);
    let previous:Option<(String,String)>=engine.catalog.db.query_row("SELECT r.operation,p.plan FROM publication_revisions r JOIN publication_plans p ON p.operation=r.operation WHERE r.source=? ORDER BY r.revision DESC LIMIT 1",[&source],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let operation = if let Some((operation, plan)) = previous {
        let previous: PublicationPlan = serde_json::from_str(&plan)?;
        if serde_json::to_value(previous.commit.records()?)? == serde_json::to_value(&records)? {
            serde_json::from_str(&format!("\"{operation}\""))?
        } else {
            OperationId::new()
        }
    } else {
        OperationId::new()
    };
    let plan = engine.catalog.prepare_publication(
        engine
            .config
            .archive_id
            .ok_or_else(|| AppError::invalid("archive required"))?,
        engine.config.writer_id,
        operation,
        records,
    )?;
    engine.catalog.db.execute("INSERT INTO publication_revisions(source,revision,operation) SELECT ?,coalesce(max(revision),0)+1,? FROM publication_revisions WHERE source=? ON CONFLICT(operation) DO NOTHING",params![source,operation.to_string(),source])?;
    Ok(VersionPublication {
        commit: plan.commit,
        history: vec![],
        projections,
        attempt: operation,
    })
}
fn acknowledge(
    catalog: &mut Catalog,
    active: &Active,
    receipt: &PublicationReceipt,
    _state: &RootHandle,
) -> AppResult<()> {
    let tx = catalog.db.transaction()?;
    if let Candidate::Records(operation) = active.candidate {
        tx.execute(
            "UPDATE pending_shared_records SET committed=1 WHERE operation=?",
            [operation.to_string()],
        )?;
    }
    if let Some(plan) = &active.plan {
        for projection in &plan.projections {
            if !receipt.projection_pending {
                tx.execute(
                    "UPDATE nas_projections SET applied=1 WHERE sidecar=? AND version=?",
                    params![
                        projection.sidecar.to_string(),
                        projection.version.to_string()
                    ],
                )?;
                let current: Option<String> = tx
                    .query_row(
                        "SELECT version FROM local_heads WHERE sidecar=?",
                        [projection.sidecar.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if current == Some(projection.version.to_string()) {
                    tx.execute(
                        "UPDATE local_heads SET dirty=0 WHERE sidecar=?",
                        [projection.sidecar.to_string()],
                    )?;
                    tx.execute(
                        "UPDATE projections SET pending=0,applied=?,observed=? WHERE sidecar=?",
                        params![
                            serde_json::to_string(projection)?,
                            serde_json::to_string(&projection.body)?,
                            projection.sidecar.to_string()
                        ],
                    )?;
                    tx.execute("UPDATE sidecars SET protected=0 WHERE id=? AND NOT EXISTS(SELECT 1 FROM sidecar_diagnostics WHERE sidecar=?)",params![projection.sidecar.to_string(),projection.sidecar.to_string()])?;
                }
            }
        }
    }
    tx.execute("UPDATE snapshots SET archived=1 WHERE digest IN(SELECT json_extract(v.body,'$.Content.digest') FROM versions v JOIN archived_versions a ON a.version=v.id)",[])?;
    tx.execute(
        "DELETE FROM publication_failures WHERE source=?",
        [candidate_key(&active.candidate)],
    )?;
    tx.commit()?;
    Ok(())
}
