use super::*;
use crate::{
    domain::*,
    error::{AppError, AppResult, ErrorCode},
};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockOwner {
    pub archive: ArchiveId,
    pub writer: WriterId,
    pub session: OperationId,
}
pub struct PublicationGuard {
    pub owner: LockOwner,
    pub(crate) cursor: JournalCursor,
    pub(crate) pending: Option<OperationId>,
    pub(crate) end: bool,
    pub(crate) replayed: bool,
}
impl PublicationGuard {
    pub fn replayed(&self) -> bool {
        self.replayed
    }
}
impl ArchiveStore {
    pub fn acquire_publication(&self) -> AppResult<PublicationGuard> {
        self.require_writable()?;
        self.acquire_admin()
    }
    pub(crate) fn acquire_admin(&self) -> AppResult<PublicationGuard> {
        self.validate()?;
        let lock = path(".photo-sorter/locks/publication")?;
        self.root.mkdir_exclusive(&lock).map_err(|e| {
            AppError::new(
                ErrorCode::Unavailable,
                format!("publication lock held or unavailable: {e}; no timeout takeover"),
            )
        })?;
        let owner = LockOwner {
            archive: self.marker.archive_id,
            writer: self.writer,
            session: OperationId::new(),
        };
        self.root.write_new(
            &path(".photo-sorter/locks/publication/owner.json")?,
            &serde_json::to_vec(&owner)?,
        )?;
        let cursor = JournalCursor::open(&self.root)?;
        Ok(PublicationGuard {
            owner,
            cursor,
            pending: None,
            end: false,
            replayed: false,
        })
    }
    pub(crate) fn validate_guard(&self, guard: &PublicationGuard) -> AppResult<()> {
        self.validate()?;
        let owner: LockOwner = serde_json::from_slice(
            &self
                .root
                .read(&path(".photo-sorter/locks/publication/owner.json")?, 65536)?,
        )?;
        if owner != guard.owner
            || owner.writer != self.writer
            || owner.archive != self.marker.archive_id
        {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "retired or foreign publication session",
            ));
        }
        Ok(())
    }
    pub fn release(&self, guard: PublicationGuard) -> AppResult<()> {
        self.validate_guard(&guard)?;
        self.root
            .unlink(&path(".photo-sorter/locks/publication/owner.json")?, false)?;
        self.root
            .unlink(&path(".photo-sorter/locks/publication")?, true)?;
        Ok(())
    }
    pub fn recover_lock(
        &mut self,
        prior_owner_stopped: bool,
        all_writers_quiescent: bool,
    ) -> AppResult<()> {
        if !prior_owner_stopped || !all_writers_quiescent {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "recovery requires operator-verified prior owner shutdown and all writers quiescent; timeout is insufficient",
            ));
        }
        self.require_writable()?;
        let retired = path(&format!(
            ".photo-sorter/locks/retired-{}",
            OperationId::new()
        ))?;
        crate::fs::publish_noreplace(
            &self.root,
            &path(".photo-sorter/locks/publication")?,
            &retired,
        )?;
        Ok(())
    }
}
