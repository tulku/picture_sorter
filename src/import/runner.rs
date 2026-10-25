use super::*;
use crate::{
    catalog::*,
    config::{Config, normalized},
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
    transfer::*,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRequest {
    pub source_path: PathBuf,
    pub strict: bool,
    pub dry_run: bool,
    pub operation_id: OperationId,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportReport {
    pub job: Option<JobId>,
    pub selected: u64,
    pub skipped: u64,
    pub recognized: u64,
    pub local_ready: u64,
    pub archive_committed: u64,
    pub pending_archive: u64,
    pub waiting: u64,
    pub conflicting: u64,
    pub association_warnings: u64,
    pub exif_invocations: u64,
    pub transfer_bytes: u64,
    pub hash_bytes: u64,
}
pub type JobProgress = ImportReport;
pub struct ImportCoordinator {
    pub config: Config,
    pub catalog: Catalog,
    extractor: Box<dyn MetadataExtractor>,
}
#[derive(Debug, Clone)]
pub(crate) struct ImportItem {
    pub id: JobId,
    pub source: SourceLocation,
    pub digest: Digest,
    pub size: u64,
    pub role: SourceRole,
    pub group: Option<GroupId>,
    pub destination: ArchivePath,
    pub local_ready: bool,
}
impl ImportCoordinator {
    pub fn new(config: Config, catalog: Catalog) -> Self {
        Self {
            config,
            catalog,
            extractor: Box::new(ExifToolExtractor::default()),
        }
    }
    pub fn with_extractor(mut self, extractor: Box<dyn MetadataExtractor>) -> Self {
        self.extractor = extractor;
        self
    }
    fn initialize_tables(&self) -> AppResult<()> {
        self.catalog
            .db
            .execute_batch(crate::catalog::schema::IMPORT_SCHEMA)?;
        Ok(())
    }
    fn source_path(&self, request: &ImportRequest) -> AppResult<PathBuf> {
        let source = normalized(&request.source_path)?;
        for root in [
            &self.config.working_root,
            &self.config.state_root,
            &self.config.archive_root,
        ] {
            let target = normalized(root)?;
            if source.starts_with(&target) || target.starts_with(&source) {
                return Err(AppError::invalid("source overlaps a managed root"));
            }
        }
        Ok(source)
    }
    pub fn begin_scan(&mut self, request: &ImportRequest) -> AppResult<JobId> {
        if request.dry_run {
            return Err(AppError::invalid("dry-run cannot create a scan"));
        }
        let source = self.source_path(request)?;
        self.initialize_tables()?;
        let job = self.catalog.enqueue(DurableRequest {
            operation_id: request.operation_id,
            kind: JobKind::Import {
                source,
                strict: request.strict,
            },
        })?;
        let tx = self.catalog.db.transaction()?;
        tx.execute(
            "INSERT INTO import_stats(job) VALUES(?) ON CONFLICT DO NOTHING",
            [job.to_string()],
        )?;
        tx.execute(
            "INSERT INTO import_scans(job) VALUES(?) ON CONFLICT DO NOTHING",
            [job.to_string()],
        )?;
        tx.commit()?;
        Ok(job)
    }
    pub fn scan_complete(&self, job: JobId) -> AppResult<bool> {
        Ok(self
            .catalog
            .db
            .query_row(
                "SELECT complete FROM import_scans WHERE job=?",
                [job.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }
    pub fn finish_scan(&mut self, job: JobId, skipped: u64) -> AppResult<ImportReport> {
        self.catalog.db.execute(
            "UPDATE import_scans SET complete=1,skipped=? WHERE job=?",
            params![skipped, job.to_string()],
        )?;
        let report = self.report(job)?;
        if report.waiting == 0 && report.conflicting == 0 {
            let record = self.catalog.job(job)?;
            if !matches!(record.state, JobState::Complete | JobState::Cancelled) {
                self.catalog
                    .transition(job, record.generation, JobTransition::Ingested)?;
            }
        }
        Ok(report)
    }
    pub fn prepare(&mut self, request: ImportRequest) -> AppResult<ImportReport> {
        let source = self.source_path(&request)?;
        if request.dry_run {
            let mut inventory = Inventory::open(RootHandle::open(&source, RootPolicy::Local)?)?;
            let mut report = ImportReport::default();
            loop {
                let entries = inventory.next_batch(128)?;
                if entries.is_empty() {
                    return Ok(report);
                }
                report.selected += entries.len() as u64;
            }
        }
        let job = self.begin_scan(&request)?;
        if self.scan_complete(job)? {
            return self.report(job);
        }
        let mut inventory = Inventory::open(RootHandle::open(&source, RootPolicy::Local)?)?;
        loop {
            let entries = inventory.next_batch(self.config.metadata_batch)?;
            if entries.is_empty() {
                return self.finish_scan(job, inventory.skipped);
            }
            self.accept_scan_batch(job, &entries, None)?;
        }
    }
    pub fn accept_scan_batch(
        &mut self,
        job: JobId,
        entries: &[SourceEntry],
        metadata: Option<Vec<MetadataResult>>,
    ) -> AppResult<ImportReport> {
        let digests = entries
            .iter()
            .map(hash_source)
            .collect::<AppResult<Vec<_>>>()?;
        self.accept_hashed_batch(job, entries, &digests, metadata)
    }
    pub fn accept_hashed_batch(
        &mut self,
        job: JobId,
        entries: &[SourceEntry],
        digests: &[Digest],
        metadata: Option<Vec<MetadataResult>>,
    ) -> AppResult<ImportReport> {
        if entries.len() > 128 || entries.len() != digests.len() || self.scan_complete(job)? {
            return Err(AppError::invalid("invalid or already completed scan batch"));
        }
        let request = self.catalog.job(job)?.request;
        let identity = SourceIdentity {
            incarnation: request.operation_id.to_string(),
            reliable: false,
        };
        let mut recognized = vec![];
        let mut new = vec![];
        for (entry, digest) in entries.iter().zip(digests) {
            entry.validate()?;
            self.catalog.remember_source(entry, &identity, digest)?;
            let cached = self.catalog.metadata(digest)?;
            if cached.is_none() && matches!(entry.role, SourceRole::Original(_)) {
                new.push(entry.clone());
            }
            recognized.push(Recognition {
                digest: digest.clone(),
                metadata: cached,
                shortcut: false,
                hashed_bytes: entry.size,
            });
        }
        let results = if let Some(metadata) = metadata {
            metadata
        } else if !new.is_empty() {
            match self.extractor.extract(&new) {
                Ok(results) => results,
                Err(error) => new
                    .iter()
                    .map(|entry| {
                        let index = entries.iter().position(|e| e.path == entry.path).unwrap();
                        let mut result = MetadataResult::from_json(
                            digests[index].clone(),
                            serde_json::Value::Null,
                        );
                        result.error = Some(error.to_string());
                        result
                    })
                    .collect(),
            }
        } else {
            vec![]
        };
        if !results.is_empty() {
            self.catalog.db.execute(
                "UPDATE import_stats SET exif_calls=exif_calls+1 WHERE job=?",
                [job.to_string()],
            )?;
        }
        for result in results {
            if !digests.contains(&result.digest) {
                return Err(AppError::new(
                    ErrorCode::SourceChanged,
                    "metadata digest changed while scanning",
                ));
            }
            self.catalog.cache_metadata(&result)?;
        }
        for entry in &new {
            let index = entries.iter().position(|e| e.path == entry.path).unwrap();
            if self.catalog.metadata(&digests[index])?.is_none() {
                return Err(AppError::integrity(
                    "metadata worker omitted a requested original",
                ));
            }
        }
        let hash_bytes = entries.iter().try_fold(0u64, |sum, e| {
            sum.checked_add(e.size)
                .ok_or_else(|| AppError::invalid("hash byte overflow"))
        })?;
        self.catalog.db.execute(
            "UPDATE import_stats SET hash_bytes=hash_bytes+? WHERE job=?",
            params![hash_bytes, job.to_string()],
        )?;
        let mut captures = Vec::new();
        for (entry, recognition) in entries.iter().zip(&recognized) {
            if let SourceRole::Original(role) = entry.role {
                let meta = self
                    .catalog
                    .metadata(&recognition.digest)?
                    .unwrap_or_else(|| {
                        MetadataResult::from_json(
                            recognition.digest.clone(),
                            serde_json::Value::Null,
                        )
                    });
                let mut capture = CaptureRecord::new(
                    OriginalMember {
                        digest: recognition.digest.clone(),
                        size: entry.size,
                        path: entry.path.clone(),
                        role,
                    },
                    meta,
                );
                capture.source_mtime = Some(entry.modified);
                captures.push(capture);
            }
        }
        let sequences = self.catalog.classify_import_sequences(&captures)?;
        for (entry, recognition) in entries.iter().zip(recognized) {
            let exists:bool=self.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM import_items WHERE parent=? AND source_path=? AND digest=?)",params![job.to_string(),entry.path.as_str(),recognition.digest.as_str()],|r|r.get(0))?;
            if exists {
                continue;
            }
            let child = JobId::new();
            let mut warning = false;
            let mut conflict = false;
            let (group, destination) = match &entry.role {
                SourceRole::Original(role) => {
                    let capture = captures
                        .iter()
                        .find(|r| r.member.path == entry.path)
                        .ok_or_else(|| AppError::integrity("capture omitted"))?;
                    let facts = capture.facts();
                    let existing_group: Option<String> = if !facts.camera.is_empty()
                        && !facts.capture.is_empty()
                    {
                        self.catalog.db.query_row("SELECT group_id FROM group_facts WHERE context=? AND stem=? AND camera=? AND capture=?",params![facts.context,facts.stem,facts.camera,facts.capture],|r|r.get(0)).optional()?
                    } else {
                        None
                    };
                    let id = match existing_group {
                        Some(id) => serde_json::from_str(&format!("\"{id}\""))?,
                        None => self.catalog.create_group(false)?,
                    };
                    let anchor = sequences
                        .iter()
                        .find(|s| s.members.contains(&recognition.digest))
                        .map(|s| s.anchor.clone());
                    let placement = plan_placements(
                        &[PlannedGroup {
                            id,
                            records: vec![capture.clone()],
                            sequence: anchor,
                        }],
                        &self.catalog,
                        &self.config.import_timezone,
                    )?
                    .remove(0);
                    let member = placement.member;
                    let existing: Option<(String, String)> = self
                        .catalog
                        .db
                        .query_row(
                            "SELECT group_id,digest FROM members WHERE path=? LIMIT 1",
                            [member.path.as_str()],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?;
                    let id = if let Some((existing, digest)) = existing {
                        if digest != recognition.digest.as_str() {
                            conflict = true;
                            id
                        } else {
                            serde_json::from_str(&format!("\"{existing}\""))?
                        }
                    } else {
                        id
                    };
                    if !conflict {
                        self.catalog.attach_member(
                            id,
                            OriginalMember {
                                role: *role,
                                ..member.clone()
                            },
                            PairEvidence::ObservedPlacement {
                                path: member.path.clone(),
                                digest: member.digest.clone(),
                            },
                        )?;
                        if !facts.camera.is_empty() && !facts.capture.is_empty() {
                            self.catalog.db.execute("INSERT INTO group_facts(context,stem,camera,capture,group_id) VALUES(?,?,?,?,?) ON CONFLICT DO NOTHING",params![facts.context,facts.stem,facts.camera,facts.capture,id.to_string()])?;
                        }
                    } else {
                        self.catalog.db.execute("INSERT INTO conflicts(id,kind,group_id,details) VALUES(?,'original_collision',?,?)",params![ConflictId::new().to_string(),id.to_string(),serde_json::to_string(&member)?])?;
                    }
                    (Some(id), member.path)
                }
                SourceRole::Companion(_) => {
                    warning = true;
                    (
                        None,
                        ArchivePath::parse(&format!(
                            ".photo-sorter/companions/{}/{}",
                            &recognition.digest.as_str()[7..],
                            entry.path.as_str().rsplit('/').next().ok_or_else(|| {
                                AppError::invalid("companion filename missing")
                            })?
                        ))?,
                    )
                }
            };
            let placement: Option<(bool, bool)> = self
                .catalog
                .db
                .query_row(
                    "SELECT local_ready,archived FROM placements WHERE path=? AND digest=? LIMIT 1",
                    params![destination.as_str(), recognition.digest.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let local = false;
            let mut archived = placement.is_some_and(|p| p.1);
            if matches!(entry.role, SourceRole::Companion(_)) {
                archived=self.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM sidecars s JOIN heads h ON h.sidecar=s.id JOIN versions v ON v.id=h.version JOIN archived_versions a ON a.version=v.id WHERE s.path=? AND json_extract(v.body,'$.Content.digest')=? AND json_extract(v.body,'$.Content.size')=?)",params![destination.as_str(),recognition.digest.as_str(),entry.size],|r|r.get(0))?;
            }
            let recognized_existing = placement.is_some_and(|p| p.0 || p.1) || archived;
            let state = if conflict {
                "conflict"
            } else if local || archived {
                "accounted"
            } else {
                "pending"
            };
            self.catalog.db.execute("INSERT INTO import_items(id,parent,source_root,source_path,digest,size,role,group_id,destination,state,local_ready,archived,warning) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",params![child.to_string(),job.to_string(),entry.root.path().to_str().ok_or_else(||AppError::invalid("UTF-8 source root required"))?,entry.path.as_str(),recognition.digest.as_str(),entry.size,serde_json::to_string(&entry.role)?,group.map(|g|g.to_string()),destination.as_str(),state,local,archived,warning])?;
            self.catalog.db.execute("UPDATE import_stats SET selected=selected+1,recognized=recognized+?,hash_bytes=hash_bytes WHERE job=?",params![u64::from(recognized_existing),job.to_string()])?;
        }
        self.report(job)
    }
    pub fn report(&self, job: JobId) -> AppResult<ImportReport> {
        let (selected,recognized,exif_invocations,transfer_bytes,hash_bytes)=self.catalog.db.query_row("SELECT selected,recognized,exif_calls,transfer_bytes,hash_bytes FROM import_stats WHERE job=?",[job.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        let(local_ready,archive_committed,waiting,conflicting,association_warnings):(u64,u64,u64,u64,u64)=self.catalog.db.query_row("SELECT coalesce(sum(local_ready),0),coalesce(sum(archived),0),coalesce(sum(local_ready=0 AND archived=0 AND state<>'conflict'),0),coalesce(sum(state='conflict'),0),coalesce(sum(warning),0) FROM import_items WHERE parent=?",[job.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        let pending_archive = selected - archive_committed;
        let skipped = self
            .catalog
            .db
            .query_row(
                "SELECT skipped FROM import_scans WHERE job=?",
                [job.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(ImportReport {
            skipped,
            job: Some(job),
            selected,
            recognized,
            local_ready,
            archive_committed,
            pending_archive,
            waiting,
            conflicting,
            association_warnings,
            exif_invocations,
            transfer_bytes,
            hash_bytes,
        })
    }
    pub fn advance(&mut self, job: JobId) -> AppResult<JobProgress> {
        let Some(attempt) = self.prepare_local_attempt(job)? else {
            return self.report(job);
        };
        match TransferEngine::local_transfer(&attempt.spec, &attempt.destination) {
            Ok(receipt) => self.ack_local_attempt(attempt, receipt),
            Err(error) => {
                self.local_attempt_failed(&attempt, &error)?;
                self.report(job)
            }
        }
    }
}
