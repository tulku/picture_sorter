use crate::{
    catalog::JobKind,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::{RootHandle, RootPolicy},
    import::*,
};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
enum ScanCommand {
    Start {
        job: JobId,
        source: std::path::PathBuf,
        limit: usize,
    },
    Next,
    Metadata {
        paths: Vec<ArchivePath>,
    },
}
enum ScanReply {
    Batch {
        entries: Vec<SourceEntry>,
        digests: Vec<Digest>,
        jpegs: Vec<JpegInspection>,
        skipped: u64,
    },
    Metadata(Vec<MetadataResult>),
}
struct Worker {
    send: SyncSender<ScanCommand>,
    receive: Receiver<AppResult<ScanReply>>,
}
impl Worker {
    fn new() -> Self {
        let (send, commands) = mpsc::sync_channel(1);
        let (results, receive) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut inventory: Option<Inventory> = None;
            let mut entries: Vec<SourceEntry> = vec![];
            let mut digests: Vec<Digest> = vec![];
            let mut limit = 128;
            while let Ok(command) = commands.recv() {
                let reply = (|| match command {
                    ScanCommand::Start {
                        job,
                        source,
                        limit: batch_limit,
                    } => {
                        let _ = job;
                        limit = batch_limit;
                        inventory = Some(Inventory::open(RootHandle::open(
                            &source,
                            RootPolicy::Local,
                        )?)?);
                        next(&mut inventory, &mut entries, &mut digests, limit)
                    }
                    ScanCommand::Next => next(&mut inventory, &mut entries, &mut digests, limit),
                    ScanCommand::Metadata { paths } => {
                        let selected = paths
                            .iter()
                            .map(|path| {
                                entries
                                    .iter()
                                    .find(|entry| entry.path == *path)
                                    .cloned()
                                    .ok_or_else(|| {
                                        AppError::integrity("scan metadata selector left its batch")
                                    })
                            })
                            .collect::<AppResult<Vec<_>>>()?;
                        let metadata = match ExifToolExtractor::default().extract(&selected) {
                            Ok(results) => results,
                            Err(error) => selected
                                .iter()
                                .map(|entry| {
                                    let index =
                                        entries.iter().position(|e| e.path == entry.path).unwrap();
                                    let mut result = MetadataResult::from_json(
                                        digests[index].clone(),
                                        serde_json::Value::Null,
                                    );
                                    result.error = Some(error.to_string());
                                    result
                                })
                                .collect(),
                        };
                        Ok(ScanReply::Metadata(metadata))
                    }
                })();
                if results.send(reply).is_err() {
                    break;
                }
            }
        });
        Self { send, receive }
    }
    fn submit(&self, command: ScanCommand) -> AppResult<()> {
        self.send.try_send(command).map_err(|_| {
            AppError::new(
                ErrorCode::Unavailable,
                "local inventory worker busy or stopped",
            )
        })
    }
}
fn next(
    inventory: &mut Option<Inventory>,
    entries: &mut Vec<SourceEntry>,
    digests: &mut Vec<Digest>,
    limit: usize,
) -> AppResult<ScanReply> {
    let inventory = inventory
        .as_mut()
        .ok_or_else(|| AppError::integrity("scan not started"))?;
    *entries = inventory.next_batch(limit)?;
    let mut jpegs = Vec::new();
    *digests = entries
        .iter()
        .map(|entry| {
            if matches!(entry.role, SourceRole::Original(MediaRole::Jpeg)) {
                let inspection = inspect_jpeg_source(entry)?;
                let digest = inspection.digest.clone();
                jpegs.push(inspection);
                Ok(digest)
            } else {
                hash_source(entry)
            }
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(ScanReply::Batch {
        entries: entries.clone(),
        digests: digests.clone(),
        jpegs,
        skipped: inventory.skipped,
    })
}
struct Pending {
    job: JobId,
    entries: Vec<SourceEntry>,
    digests: Vec<Digest>,
}
pub(crate) struct ScanDriver {
    worker: Worker,
    active: Option<JobId>,
    pending: Option<Pending>,
}
impl Default for ScanDriver {
    fn default() -> Self {
        Self {
            worker: Worker::new(),
            active: None,
            pending: None,
        }
    }
}
impl ScanDriver {
    pub fn cancel(&mut self, job: JobId) -> AppResult<()> {
        if self.active != Some(job) {
            return Ok(());
        }
        match self.worker.receive.try_recv() {
            Ok(_) | Err(TryRecvError::Disconnected) => {
                self.active = None;
                self.pending = None;
            }
            Err(TryRecvError::Empty) => {}
        }
        Ok(())
    }
    pub fn active_job(&self) -> Option<JobId> {
        self.active
    }

    pub fn advance(&mut self, engine: &mut ImportCoordinator, job: JobId) -> AppResult<()> {
        if engine.scan_complete(job)? {
            return Ok(());
        }
        if let Some(active) = self.active {
            if active != job {
                return Ok(());
            }
        } else {
            let record = engine.catalog.job(job)?;
            let JobKind::Import { source, strict } = record.request.kind else {
                return Err(AppError::invalid("inventory needs import job"));
            };
            engine.begin_scan(&ImportRequest {
                source_path: source.clone(),
                strict,
                dry_run: false,
                operation_id: record.request.operation_id,
            })?;
            self.worker.submit(ScanCommand::Start {
                job,
                source,
                limit: engine.config.metadata_batch,
            })?;
            self.active = Some(job);
            return Ok(());
        }
        let reply = match self.worker.receive.try_recv() {
            Ok(Ok(reply)) => reply,
            Ok(Err(error)) => {
                self.active = None;
                self.pending = None;
                return Err(error);
            }
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => {
                return Err(AppError::new(
                    ErrorCode::Unavailable,
                    "inventory worker stopped",
                ));
            }
        };
        match reply {
            ScanReply::Batch {
                entries,
                digests,
                jpegs,
                skipped,
            } => {
                if entries.is_empty() {
                    engine.finish_scan(job, skipped)?;
                    self.active = None;
                    return Ok(());
                }
                for inspection in jpegs {
                    let entry = entries
                        .iter()
                        .zip(&digests)
                        .find(|(entry, digest)| {
                            matches!(entry.role, SourceRole::Original(MediaRole::Jpeg))
                                && **digest == inspection.digest
                                && entry.size == inspection.size
                        })
                        .map(|(entry, _)| entry)
                        .ok_or_else(|| {
                            AppError::integrity("JPEG inspection left its scan batch")
                        })?;
                    entry.validate()?;
                    engine.catalog.cache_jpeg_inspection(&inspection)?;
                }
                let mut paths = vec![];
                for (entry, digest) in entries.iter().zip(&digests) {
                    if matches!(entry.role, SourceRole::Original(_))
                        && engine.catalog.metadata(digest)?.is_none()
                    {
                        paths.push(entry.path.clone());
                    }
                }
                if paths.is_empty() {
                    engine.accept_hashed_batch(job, &entries, &digests, Some(vec![]))?;
                    self.worker.submit(ScanCommand::Next)?;
                } else {
                    self.pending = Some(Pending {
                        job,
                        entries,
                        digests,
                    });
                    self.worker.submit(ScanCommand::Metadata { paths })?;
                }
            }
            ScanReply::Metadata(metadata) => {
                let pending = self
                    .pending
                    .take()
                    .ok_or_else(|| AppError::integrity("metadata reply has no batch"))?;
                if pending.job != job {
                    return Err(AppError::integrity("metadata reply has obsolete job"));
                }
                engine.accept_hashed_batch(
                    job,
                    &pending.entries,
                    &pending.digests,
                    Some(metadata),
                )?;
                self.worker.submit(ScanCommand::Next)?;
            }
        }
        Ok(())
    }
}
