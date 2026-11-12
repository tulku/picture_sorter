use super::*;
use crate::{
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeChangeRequest {
    pub operation: OperationId,
    pub expected: ArchiveMarker,
    pub mode: PublicationMode,
    pub new_owner: WriterId,
    pub all_writers_quiescent: bool,
    pub prior_owner_stopped: bool,
    pub no_unresolved_io: bool,
    pub recover: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModeJournal {
    schema_version: u32,
    operation: OperationId,
    previous: ArchiveMarker,
    next: ArchiveMarker,
}
impl ArchiveStore {
    pub fn change_mode(&mut self, request: ModeChangeRequest) -> AppResult<ArchiveMarker> {
        self.change_mode_with_faults(request, &NoFaults)
    }
    pub fn change_mode_with_faults(
        &mut self,
        request: ModeChangeRequest,
        faults: &dyn FaultInjector,
    ) -> AppResult<ArchiveMarker> {
        if !request.all_writers_quiescent
            || !request.prior_owner_stopped
            || !request.no_unresolved_io
        {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "mode administration requires positive publisher shutdown, quiescence and no unresolved old I/O",
            ));
        }
        self.validate()?;
        if request.expected.archive_id != self.marker.archive_id
            || request.expected.schema_version != 1
        {
            return Err(AppError::integrity("mode change archive identity mismatch"));
        }
        if request.mode == PublicationMode::LocalTest && !self.root.is_test_archive() {
            return Err(AppError::invalid(
                "LocalTest mode requires an explicit non-network test root",
            ));
        }
        let next = ArchiveMarker {
            schema_version: 1,
            archive_id: request.expected.archive_id,
            writer_id: request.new_owner,
            mode: request.mode,
            mode_revision: request
                .expected
                .mode_revision
                .checked_add(1)
                .ok_or_else(|| AppError::invalid("mode revision overflow"))?,
        };
        let journal = ModeJournal {
            schema_version: 1,
            operation: request.operation,
            previous: request.expected,
            next: next.clone(),
        };
        let bytes = serde_json::to_vec(&journal)?;
        let pending = path(".photo-sorter/mode-pending.json")?;
        let record = path(&format!(
            ".photo-sorter/modes/{}-{}.json",
            next.mode_revision, request.operation
        ))?;
        if self.marker == next {
            if self.root.read(&record, 65536)? != bytes {
                return Err(AppError::integrity("mode receipt does not match retry"));
            }
            match self.root.read(&pending, 65536) {
                Ok(existing) if existing == bytes && request.recover => {
                    self.retire_mode_lock()?;
                    let guard = self.acquire_admin()?;
                    self.finish_mode_pending(&pending, request.operation)?;
                    self.release(guard)?;
                }
                Ok(_) => {
                    return Err(AppError::new(
                        ErrorCode::Conflict,
                        "pending mode update requires explicit recovery",
                    ));
                }
                Err(e) if e.errno == Some(libc::ENOENT) => {}
                Err(e) => return Err(e),
            }
            return Ok(next);
        }
        if self.marker != journal.previous {
            return Err(AppError::new(ErrorCode::Conflict, "stale mode revision"));
        }
        Self::check_exchange(
            &self.root,
            next.archive_id,
            next.writer_id,
            &next.mode,
            next.mode_revision,
            &self.registry,
        )?;
        match self.root.read(&pending, 65536) {
            Ok(existing) => {
                if existing != bytes || !request.recover {
                    return Err(AppError::new(
                        ErrorCode::Conflict,
                        "uncertain mode update requires matching explicit recovery",
                    ));
                }
            }
            Err(e) if e.errno == Some(libc::ENOENT) => {}
            Err(e) => return Err(e),
        }
        let lock = path(".photo-sorter/locks/publication")?;
        if request.recover {
            match self.root.metadata_relative(&lock) {
                Ok(_) => {
                    let retired = path(&format!(
                        ".photo-sorter/locks/mode-recovery-{}",
                        OperationId::new()
                    ))?;
                    publish_noreplace(&self.root, &lock, &retired)?;
                }
                Err(e) if e.errno == Some(libc::ENOENT) => {}
                Err(e) => return Err(e),
            }
        }
        let guard = self.acquire_admin()?;
        let change = || -> AppResult<()> {
            self.root.mkdir_all(&path(".photo-sorter/modes")?)?;
            match self.root.write_new(&pending, &bytes) {
                Ok(()) => {}
                Err(e)
                    if e.errno == Some(libc::EEXIST)
                        && self.root.read(&pending, 65536)? == bytes => {}
                Err(e) => return Err(e),
            }
            faults.hit(FaultPoint::IntentSaved)?;
            match self.root.write_new(&record, &bytes) {
                Ok(()) => {}
                Err(e)
                    if e.errno == Some(libc::EEXIST)
                        && self.root.read(&record, 65536)? == bytes => {}
                Err(e) => return Err(e),
            }
            faults.hit(FaultPoint::CommitPublished)?;
            let stage = path(&format!(
                ".photo-sorter/staging/mode-marker-{}-{}.json",
                request.operation,
                OperationId::new()
            ))?;
            self.root.write_new(&stage, &serde_json::to_vec(&next)?)?;
            self.root
                .exchange(&stage, &path(".photo-sorter/archive.json")?)?;
            let displaced: ArchiveMarker = serde_json::from_slice(&self.root.read(&stage, 65536)?)?;
            if displaced != journal.previous {
                return Err(AppError::integrity(
                    "mode marker exchange displaced unexpected evidence; recovery required",
                ));
            }
            Ok(())
        };
        change()?;
        self.marker = next.clone();
        faults.hit(FaultPoint::ProjectionPublished)?;
        self.finish_mode_pending(&pending, request.operation)?;
        self.release(guard)?;
        Ok(next)
    }
    fn retire_mode_lock(&self) -> AppResult<()> {
        let lock = path(".photo-sorter/locks/publication")?;
        match self.root.metadata_relative(&lock) {
            Ok(_) => {
                publish_noreplace(
                    &self.root,
                    &lock,
                    &path(&format!(
                        ".photo-sorter/locks/mode-recovery-{}",
                        OperationId::new()
                    ))?,
                )?;
            }
            Err(e) if e.errno == Some(libc::ENOENT) => {}
            Err(e) => return Err(e),
        }
        Ok(())
    }
    fn finish_mode_pending(&self, pending: &ArchivePath, operation: OperationId) -> AppResult<()> {
        let retained = path(&format!(
            ".photo-sorter/modes/completed-{}-{}.json",
            operation,
            OperationId::new()
        ))?;
        publish_noreplace(&self.root, pending, &retained)?;
        Ok(())
    }
}
