mod jobs;
mod models;
mod schema;
mod vfs;
use crate::{
    domain::ArchivePath,
    error::{AppError, AppResult, ErrorCode},
    fs::RootHandle,
};
pub use models::*;
use rusqlite::{Connection, OpenFlags};
use std::{fs::File, os::fd::AsRawFd, path::Path};
pub struct Catalog {
    pub(crate) db: Connection,
    _root: File,
}
impl Catalog {
    pub fn open(state: &RootHandle) -> AppResult<Self> {
        state.validate_identity()?;
        let file = state.open_relative(
            &ArchivePath::parse("catalog.sqlite")?,
            libc::O_RDWR | libc::O_CREAT,
            0o600,
        )?;
        if !file.metadata()?.is_file() {
            return Err(AppError::invalid("catalog must be regular file"));
        }
        let root = File::open(format!("/proc/self/fd/{}", state.fd()))?;
        let path = format!("/proc/self/fd/{}/catalog.sqlite", root.as_raw_fd());
        vfs::register()?;
        let mut db = Connection::open_with_flags_and_vfs(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            "photo-archive-fd",
        )?;
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > schema::VERSION {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "catalog schema is newer; preserve it and upgrade this application",
            ));
        }
        db.pragma_update(None, "foreign_keys", true)?;
        if version == 0 {
            let count: i64 = db.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )?;
            if count != 0 {
                return Err(AppError::integrity("unversioned nonempty catalog"));
            }
            let tx = db.transaction()?;
            tx.execute_batch(schema::SCHEMA)?;
            tx.commit()?;
        }
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "FULL")?;
        state.sync()?;
        Ok(Self { db, _root: root })
    }
    pub fn backup(&self, dest: &Path) -> AppResult<()> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)?;
        file.sync_all()?;
        let mut target = Connection::open(dest)?;
        let backup = rusqlite::backup::Backup::new(&self.db, &mut target)?;
        backup.run_to_completion(128, std::time::Duration::from_millis(10), None)?;
        drop(backup);
        drop(target);
        File::open(dest)?.sync_all()?;
        if let Some(parent) = dest.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}
pub(crate) fn encode<T: serde::Serialize>(value: &T) -> AppResult<String> {
    Ok(serde_json::to_string(value)?)
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(value: String) -> AppResult<T> {
    Ok(serde_json::from_str(&value)?)
}
mod identity;
pub use identity::{PairEvidence, PairFacts, Protection};
