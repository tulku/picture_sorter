use super::*;
use crate::{
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    import::*,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{Seek, SeekFrom},
    os::unix::fs::MetadataExt,
};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveObservation {
    pub path: ArchivePath,
    pub digest: Digest,
    pub size: u64,
    pub role: SourceRole,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveInventoryBatch {
    pub entries: Vec<ArchiveObservation>,
    pub end: bool,
    pub skipped: u64,
}
pub(crate) struct ArchiveInventory {
    cursor: Inventory,
    pub entries: Vec<SourceEntry>,
}
impl ArchiveInventory {
    pub fn open(store: &ArchiveStore, scope: Option<&ArchivePath>) -> AppResult<Self> {
        let root = store.root.try_clone()?;
        Ok(Self {
            cursor: Inventory::open_all(root, scope)?,
            entries: vec![],
        })
    }
    pub fn next(&mut self, store: &ArchiveStore, limit: usize) -> AppResult<ArchiveInventoryBatch> {
        self.entries = self.cursor.next_batch(limit)?;
        let mut entries = vec![];
        for entry in &self.entries {
            let digest = hash_source(entry)?;
            if matches!(entry.role, SourceRole::Companion(_)) {
                let staged = ArchivePath::parse(&format!(
                    ".photo-sorter/staging/adopt-{}",
                    OperationId::new()
                ))?;
                let mut source = entry.root.open_relative(&entry.path, libc::O_RDONLY, 0)?;
                let before = source.metadata()?;
                let mut copy = store.root.create_new(&staged)?;
                std::io::copy(&mut source, &mut copy)?;
                copy.sync_all()?;
                source.seek(SeekFrom::Start(0))?;
                if crate::transfer::digest_file(&mut source)? != (digest.clone(), entry.size)
                    || before.ctime() != source.metadata()?.ctime()
                {
                    return Err(AppError::new(
                        ErrorCode::SourceChanged,
                        "sidecar changed during bootstrap snapshot",
                    ));
                }
                entry.validate()?;
                store.store_history(&staged, &digest, entry.size)?;
            }
            entries.push(ArchiveObservation {
                path: entry.path.clone(),
                digest,
                size: entry.size,
                role: entry.role.clone(),
            });
        }
        store.validate()?;
        Ok(ArchiveInventoryBatch {
            end: entries.is_empty(),
            entries,
            skipped: self.cursor.skipped,
        })
    }
    pub fn metadata(&self, paths: &[ArchivePath]) -> AppResult<Vec<MetadataResult>> {
        if paths.len() > 128 {
            return Err(AppError::invalid("metadata batch exceeds 128"));
        }
        let mut entries = vec![];
        for path in paths {
            let entry = self
                .entries
                .iter()
                .find(|entry| entry.path == *path && matches!(entry.role, SourceRole::Original(_)))
                .ok_or_else(|| {
                    AppError::invalid("metadata path is not in current original batch")
                })?;
            entries.push(entry.clone());
        }
        ExifToolExtractor::default().extract(&entries)
    }
}
