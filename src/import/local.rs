use super::*;
use crate::{
    catalog::*,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
    storage::*,
    transfer::*,
};
use rusqlite::{OptionalExtension, params};
type PendingLocalRow = (
    String,
    String,
    String,
    String,
    u64,
    String,
    Option<String>,
    String,
);
#[derive(Debug, Clone)]
pub struct LocalImportAttempt {
    pub spec: TransferSpec,
    pub destination: ArchivePath,
    pub parent: JobId,
    pub item: JobId,
    pub reservation: ReservationId,
    pub reuse_only: bool,
    group: Option<GroupId>,
    role: SourceRole,
}
impl ImportCoordinator {
    pub fn prepare_local_attempt(&mut self, job: JobId) -> AppResult<Option<LocalImportAttempt>> {
        let record = self.catalog.job(job)?;
        if record.cancel_requested
            || matches!(record.state, JobState::Complete | JobState::Cancelled)
        {
            return Ok(None);
        }
        let row:Option<PendingLocalRow>=self.catalog.db.query_row("SELECT id,source_root,source_path,digest,size,role,group_id,destination FROM import_items WHERE parent=? AND local_ready=0 AND archived=0 AND state<>'conflict' ORDER BY rowid LIMIT 1",[job.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional()?;
        let Some((id, source_root, source_path, digest, size, role, group, destination)) = row
        else {
            return Ok(None);
        };
        let item: JobId = serde_json::from_str(&format!("\"{id}\""))?;
        let transfer = self.catalog.enqueue(DurableRequest {
            operation_id: OperationId(item.0),
            kind: JobKind::LocalTransfer { parent: job, item },
        })?;
        let child = self.catalog.job(transfer)?;
        if child.state == JobState::Queued {
            self.catalog
                .transition(transfer, child.generation, JobTransition::Start)?;
        }
        let generation = self.catalog.job(transfer)?.generation;
        let retained: Option<String> = self.catalog.db.query_row("SELECT json_extract(previous,'$.destination') FROM retired_import_plans WHERE replacement=? AND json_extract(previous,'$.local_ready')=1",[item.to_string()],|r|r.get(0)).optional()?;
        let source = match retained {
            Some(path) => SourceLocation {
                root: self.config.working_root.clone(),
                path: ArchivePath::parse(&path)?,
            },
            None => SourceLocation {
                root: source_root.into(),
                path: ArchivePath::parse(&source_path)?,
            },
        };
        let spec = TransferSpec {
            job: transfer,
            generation,
            source,
            destination_root: self.config.working_root.clone(),
            staged_path: staging_path(transfer, generation),
            expected_digest: Digest::parse(&digest)?,
            expected_size: size,
        };
        let local = RootHandle::open(&self.config.working_root, RootPolicy::Local)?;
        let state = RootHandle::open(&self.config.state_root, RootPolicy::Local)?;
        let partial =
            match local.open_relative(&spec.staged_path, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
                Ok(file) => {
                    let m = file.metadata()?;
                    if !m.is_file() || m.len() > size {
                        return Err(AppError::integrity(
                            "owned partial has unexpected type or size",
                        ));
                    }
                    m.len()
                }
                Err(e) if e.errno == Some(libc::ENOENT) => 0,
                Err(e) => return Err(e),
            };
        let mut existing: Option<(String, u64)> = self
            .catalog
            .db
            .query_row(
                "SELECT id,remaining FROM reservations WHERE job=?",
                [transfer.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, bytes)) = &mut existing {
            *bytes = self
                .catalog
                .reservation_charge(serde_json::from_value(id.clone().into())?)?;
        }
        let reserved = self
            .catalog
            .remaining_reservations(FilesystemId(local.device()))?
            .checked_sub(existing.as_ref().map_or(0, |r| r.1))
            .ok_or_else(|| AppError::integrity("reservation accounting mismatch"))?;
        let measurement = self.catalog.admission_measurement(&local, &state)?;
        let occupied = measurement
            .local
            .total
            .checked_add(measurement.state.total)
            .ok_or_else(|| AppError::invalid("occupancy overflow"))?;
        let input = AdmissionInput {
            budget: self.config.budget,
            occupied,
            reserved,
            available: measurement.filesystem_free,
            reserve: self.config.reserve,
            required: size,
            existing: 0,
            partial,
            overhead: 0,
            nas_available: false,
            allow_direct: true,
        };
        let destination_exists = match local.open_relative(
            &ArchivePath::parse(&destination)?,
            libc::O_RDONLY | libc::O_NONBLOCK,
            0,
        ) {
            Ok(file) => {
                if !file.metadata()?.is_file() {
                    return Err(AppError::integrity(
                        "existing local destination is not a regular file",
                    ));
                }
                true
            }
            Err(error) if error.errno == Some(libc::ENOENT) => false,
            Err(error) => return Err(error),
        };
        let decision = if destination_exists {
            AdmissionDecision::Local {
                additional_bytes: 0,
            }
        } else {
            admit_with_budget_reservations(
                &input,
                self.catalog
                    .total_remaining_reservations()?
                    .checked_sub(existing.as_ref().map_or(0, |r| r.1))
                    .ok_or_else(|| AppError::integrity("budget reservation mismatch"))?,
            )
        };
        let AdmissionDecision::Local { additional_bytes } = decision else {
            self.catalog.transition(job,record.generation,JobTransition::Wait(JobState::WaitingCapacity,"local budget or filesystem reserve unavailable; source retained while direct archive publication is considered".into()))?;
            return Ok(None);
        };
        if state.device() != local.device()
            && measurement.state_filesystem_free < self.config.reserve
        {
            return Ok(None);
        }
        let reservation = if let Some((id, _)) = existing {
            let id = serde_json::from_str(&format!("\"{id}\""))?;
            self.catalog.update_reservation(id, additional_bytes)?;
            id
        } else {
            self.catalog
                .reserve(transfer, FilesystemId(local.device()), additional_bytes)?
        };
        self.catalog
            .transition(job, record.generation, JobTransition::Resume)?;
        self.catalog
            .db
            .execute("DELETE FROM job_retry WHERE job=?", [job.to_string()])?;
        Ok(Some(LocalImportAttempt {
            spec,
            destination: ArchivePath::parse(&destination)?,
            parent: job,
            item,
            reservation,
            reuse_only: destination_exists,
            group: group
                .map(|g| serde_json::from_str(&format!("\"{g}\"")))
                .transpose()?,
            role: serde_json::from_str(&role)?,
        }))
    }
    pub fn reconcile_local_reservation(&mut self, attempt: &LocalImportAttempt) -> AppResult<()> {
        let root = RootHandle::open(&attempt.spec.destination_root, RootPolicy::Local)?;
        if attempt.reuse_only {
            return Ok(());
        }
        let size = match root.open_relative(
            &attempt.spec.staged_path,
            libc::O_RDONLY | libc::O_NONBLOCK,
            0,
        ) {
            Ok(file) => file.metadata()?.len(),
            Err(e) if e.errno == Some(libc::ENOENT) => 0,
            Err(e) => return Err(e),
        };
        let remaining = attempt
            .spec
            .expected_size
            .checked_sub(size)
            .ok_or_else(|| AppError::integrity("partial exceeds reserved object"))?;
        self.catalog
            .update_reservation(attempt.reservation, remaining)
    }
    pub fn finish_local_attempt(
        &mut self,
        attempt: LocalImportAttempt,
        receipt: TransferReceipt,
    ) -> AppResult<ImportReport> {
        let root = RootHandle::open(&attempt.spec.destination_root, RootPolicy::Local)?;
        crate::transfer::publish_local(&root, &receipt, &attempt.destination)?;
        self.ack_local_attempt(attempt, receipt)
    }
    pub fn ack_local_attempt(
        &mut self,
        attempt: LocalImportAttempt,
        receipt: TransferReceipt,
    ) -> AppResult<ImportReport> {
        if !receipt.matches(attempt.spec.job, attempt.spec.generation)
            || receipt.digest != attempt.spec.expected_digest
            || receipt.size != attempt.spec.expected_size
            || receipt.staged_path != attempt.spec.staged_path
            || self.catalog.job(attempt.spec.job)?.generation != attempt.spec.generation
        {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "obsolete or mismatched local transfer receipt",
            ));
        }
        if let Some(group) = attempt.group {
            let SourceRole::Original(role) = attempt.role else {
                return Err(AppError::integrity(
                    "companion cannot be an original member",
                ));
            };
            self.catalog.record_materialization(
                group,
                &OriginalMember {
                    digest: receipt.digest,
                    size: receipt.size,
                    path: attempt.destination,
                    role,
                },
                false,
            )?;
        }
        let tx = self.catalog.db.transaction()?;
        tx.execute(
            "UPDATE attempts SET state=? WHERE job=? AND generation=?",
            params![
                serde_json::to_string("quiescent")?,
                attempt.spec.job.to_string(),
                attempt.spec.generation
            ],
        )?;
        tx.execute(
            "UPDATE import_items SET local_ready=1,state='ingested' WHERE id=? AND parent=?",
            params![attempt.item.to_string(), attempt.parent.to_string()],
        )?;
        tx.execute(
            "UPDATE import_stats SET transfer_bytes=transfer_bytes+? WHERE job=?",
            params![receipt.transferred_bytes, attempt.parent.to_string()],
        )?;
        tx.execute(
            "DELETE FROM reservations WHERE id=? AND job=?",
            params![
                attempt.reservation.to_string(),
                attempt.spec.job.to_string()
            ],
        )?;
        tx.commit()?;
        let child = self.catalog.job(attempt.spec.job)?;
        self.catalog
            .transition(attempt.spec.job, child.generation, JobTransition::Complete)?;
        let report = self.report(attempt.parent)?;
        if report.waiting == 0
            && report.conflicting == 0
            && self.scan_complete(attempt.parent)?
            && !self.catalog.job(attempt.parent)?.cancel_requested
        {
            let parent = self.catalog.job(attempt.parent)?;
            self.catalog
                .transition(attempt.parent, parent.generation, JobTransition::Ingested)?;
        }
        Ok(report)
    }
    pub fn local_attempt_failed(
        &mut self,
        attempt: &LocalImportAttempt,
        error: &AppError,
    ) -> AppResult<()> {
        self.catalog.update_reservation(attempt.reservation, 0)?;
        self.catalog.invalidate_admission()?;
        self.catalog.db.execute(
            "UPDATE attempts SET state=? WHERE job=? AND generation=?",
            params![
                serde_json::to_string("quiescent")?,
                attempt.spec.job.to_string(),
                attempt.spec.generation
            ],
        )?;
        let record = self.catalog.job(attempt.parent)?;
        let state = match error.code {
            ErrorCode::SourceChanged => JobState::WaitingSource,
            ErrorCode::Conflict | ErrorCode::Integrity => JobState::Conflict,
            _ => JobState::Failed,
        };
        if state == JobState::Conflict {
            self.catalog.db.execute(
                "UPDATE import_items SET state='conflict' WHERE id=?",
                [attempt.item.to_string()],
            )?;
            if let SourceRole::Original(role) = attempt.role {
                self.catalog.record_ambiguity(
                    "original_collision",
                    attempt.group,
                    serde_json::to_value(OriginalMember {
                        digest: attempt.spec.expected_digest.clone(),
                        size: attempt.spec.expected_size,
                        path: attempt.destination.clone(),
                        role,
                    })?,
                )?;
            }
        }
        if record.cancel_requested {
            return Ok(());
        }
        self.catalog.transition(
            attempt.parent,
            record.generation,
            JobTransition::Wait(state, error.to_string()),
        )?;
        Ok(())
    }
}
