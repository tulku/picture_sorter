mod jobs;
mod models;
pub(crate) mod schema;
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
    pub(crate) async_admission: bool,
    admission_limits: Option<(u64, u64)>,
    admission_observed: std::cell::Cell<Option<std::time::Instant>>,
    admission_requested: std::cell::Cell<bool>,
    admission_proof: std::cell::RefCell<Option<(u64, crate::fs::ManagedSpaceMeasurement)>>,
}
impl Catalog {
    pub fn open(state: &RootHandle) -> AppResult<Self> {
        state.require_local()?;
        let catalog_path = ArchivePath::parse("catalog.sqlite")?;
        match state.create_new(&catalog_path) {
            Ok(file) => {
                file.sync_all()?;
                drop(file);
            }
            Err(error) if error.errno == Some(libc::EEXIST) => {}
            Err(error) => return Err(error),
        }
        // Closing another descriptor to a live SQLite inode releases all process-scoped POSIX locks.
        let metadata =
            std::fs::symlink_metadata(format!("/proc/self/fd/{}/catalog.sqlite", state.fd()))?;
        if !metadata.is_file() {
            return Err(AppError::invalid("catalog must be a regular file"));
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
            tx.execute_batch(schema::IMPORT_SCHEMA)?;
            tx.execute_batch(schema::EDIT_SCHEMA)?;
            tx.execute_batch(schema::RATING_SCHEMA)?;
            tx.execute_batch(schema::RATING_AUTHORITY_SCHEMA)?;
            tx.execute_batch(schema::BOOTSTRAP_SCHEMA)?;
            tx.execute_batch(schema::RESOLUTION_SCHEMA)?;
            tx.commit()?;
        }
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "FULL")?;
        state.sync()?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS admission_epoch(value INTEGER NOT NULL); INSERT INTO admission_epoch SELECT 0 WHERE NOT EXISTS(SELECT 1 FROM admission_epoch);
CREATE TABLE IF NOT EXISTS refresh_application_receipts(job TEXT NOT NULL REFERENCES jobs(id),sidecar TEXT NOT NULL,version TEXT NOT NULL,UNIQUE(job,sidecar,version));
CREATE TABLE IF NOT EXISTS local_residency_expectations(placement TEXT PRIMARY KEY);
INSERT OR IGNORE INTO local_residency_expectations SELECT id FROM placements WHERE local_ready=1;
DROP TRIGGER IF EXISTS residency_expected_insert;
DROP TRIGGER IF EXISTS residency_expected_update;
CREATE TRIGGER IF NOT EXISTS residency_expected_insert_v2 AFTER INSERT ON placements WHEN new.local_ready=1 BEGIN INSERT INTO local_residency_expectations SELECT new.id WHERE NOT EXISTS(SELECT 1 FROM local_residency_expectations WHERE placement=new.id); END;
CREATE TRIGGER IF NOT EXISTS residency_expected_update_v2 AFTER UPDATE OF local_ready ON placements WHEN new.local_ready=1 BEGIN INSERT INTO local_residency_expectations SELECT new.id WHERE NOT EXISTS(SELECT 1 FROM local_residency_expectations WHERE placement=new.id); END;
CREATE TRIGGER IF NOT EXISTS residency_expected_delete AFTER DELETE ON placements BEGIN DELETE FROM local_residency_expectations WHERE placement=old.id; END;
CREATE TABLE IF NOT EXISTS reservation_ceilings(id TEXT PRIMARY KEY,bytes INTEGER NOT NULL);
INSERT OR IGNORE INTO reservation_ceilings SELECT id,remaining FROM reservations;
CREATE TRIGGER IF NOT EXISTS admission_insert AFTER INSERT ON reservations BEGIN UPDATE admission_epoch SET value=value+1; INSERT INTO reservation_ceilings VALUES(new.id,new.remaining); END;
CREATE TRIGGER IF NOT EXISTS admission_update AFTER UPDATE OF remaining ON reservations WHEN old.remaining<>new.remaining BEGIN UPDATE admission_epoch SET value=value+1; UPDATE reservation_ceilings SET bytes=max(bytes,new.remaining) WHERE id=new.id; END;
CREATE TRIGGER IF NOT EXISTS admission_delete AFTER DELETE ON reservations BEGIN UPDATE admission_epoch SET value=value+1; DELETE FROM reservation_ceilings WHERE id=old.id; END;")?;
        Ok(Self {
            db,
            _root: root,
            async_admission: false,
            admission_limits: None,
            admission_observed: std::cell::Cell::new(None),
            admission_requested: std::cell::Cell::new(false),
            admission_proof: std::cell::RefCell::new(None),
        })
    }
    pub fn backup(&self, dest: &Path) -> AppResult<()> {
        backup_connection(&self.db, dest)
    }
}
pub(crate) fn backup_connection(source: &Connection, dest: &Path) -> AppResult<()> {
    let parent = crate::fs::RootHandle::open(
        dest.parent()
            .ok_or_else(|| AppError::invalid("backup requires a local parent"))?,
        crate::fs::RootPolicy::Local,
    )?;
    let name = ArchivePath::parse(
        dest.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| AppError::invalid("backup filename must be UTF-8"))?,
    )?;
    let file = parent.create_new(&name)?;
    file.sync_all()?;
    vfs::register()?;
    let mut target = Connection::open_with_flags_and_vfs(
        format!("/proc/self/fd/{}/{}", parent.fd(), name.as_str()),
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        "photo-archive-fd",
    )?;
    let backup = rusqlite::backup::Backup::new(source, &mut target)?;
    backup.run_to_completion(128, std::time::Duration::from_millis(10), None)?;
    drop(backup);
    drop(target);
    file.sync_all()?;
    parent.sync()?;
    Ok(())
}
pub(crate) fn encode<T: serde::Serialize>(value: &T) -> AppResult<String> {
    Ok(serde_json::to_string(value)?)
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(value: String) -> AppResult<T> {
    Ok(serde_json::from_str(&value)?)
}
mod identity;
pub use identity::{PairEvidence, PairFacts, Protection};
mod replay;
pub use replay::{ApplyOutcome, PublicationReadiness};
mod maintenance;
pub use maintenance::*;
mod verify;
pub use verify::*;
mod conflicts;
mod publication;
pub use conflicts::*;

pub(crate) fn backup_readonly(state: &RootHandle, dest: &Path) -> AppResult<()> {
    state.require_local()?;
    let file = state.open_relative(&ArchivePath::parse("catalog.sqlite")?, libc::O_RDONLY, 0)?;
    if !file.metadata()?.is_file() {
        return Err(AppError::invalid("catalog must be regular"));
    }
    vfs::register()?;
    let db = Connection::open_with_flags_and_vfs(
        format!("/proc/self/fd/{}/catalog.sqlite", state.fd()),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        "photo-archive-fd",
    )?;
    let _: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    backup_connection(&db, dest)?;
    state.validate_identity()?;
    Ok(())
}

mod rating_convergence;
pub use rating_convergence::*;

mod admission;
