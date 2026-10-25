use super::*;
use crate::{
    archive::CommitEnvelope,
    domain::*,
    fs::{RootHandle, RootPolicy},
};
use serde::{Deserialize, Serialize};
use std::{
    os::fd::{AsRawFd, OwnedFd},
    path::PathBuf,
};
pub struct OwnershipLock {
    file: File,
}
impl OwnershipLock {
    pub fn acquire(root: &RootHandle, name: &str) -> AppResult<Self> {
        root.require_local()?;
        let file = root.open_relative(
            &ArchivePath::parse(name)?,
            libc::O_RDWR | libc::O_CREAT,
            0o600,
        )?;
        if !file.metadata()?.is_file() {
            return Err(AppError::invalid("ownership lock must be a regular file"));
        }
        // flock is retained by inherited descriptors until every potential writer exits.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(AppError::new(
                ErrorCode::Unavailable,
                "profile or working root is owned by an active service or unreaped worker",
            ));
        }
        Ok(Self { file })
    }
    pub fn inherit(&self) -> AppResult<OwnedFd> {
        Ok(self.file.try_clone()?.into())
    }
}
#[derive(Debug, Clone)]
pub struct RebuildRequest {
    pub state_root: PathBuf,
    pub working_root: PathBuf,
    pub replacement: PathBuf,
    pub profile: ProfileId,
    pub archive: ArchiveId,
    pub commits: Vec<CommitEnvelope>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub writer: WriterId,
    pub replacement: PathBuf,
    pub private_state_lost: bool,
    pub local_observations: u64,
    pub commits_applied: u64,
    pub limitations: Vec<String>,
}
pub struct CatalogRecovery;
impl CatalogRecovery {
    pub fn rebuild(request: RebuildRequest) -> AppResult<RecoveryReport> {
        let state = RootHandle::open(&request.state_root, RootPolicy::Local)?;
        let _profile = OwnershipLock::acquire(&state, "profile.lock")?;
        let local = RootHandle::open(&request.working_root, RootPolicy::Local)?;
        let _work = OwnershipLock::acquire(&local, "working.lock")?;
        let dest = RootHandle::open(&request.replacement, RootPolicy::Local)?;
        if state.device() == dest.device() && state.inode() == dest.inode() {
            return Err(AppError::invalid("rebuild requires a new state location"));
        }
        if std::fs::read_dir(&request.replacement)?.next().is_some() {
            return Err(AppError::invalid("replacement state must be empty"));
        }
        dest.mkdir_all(&ArchivePath::parse("preserved")?)?;
        for name in ["catalog.sqlite", "catalog.sqlite-wal", "catalog.sqlite-shm"] {
            match state.open_relative(&ArchivePath::parse(name)?, libc::O_RDONLY, 0) {
                Ok(mut source) => {
                    let mut copy =
                        dest.create_new(&ArchivePath::parse(&format!("preserved/{name}"))?)?;
                    std::io::copy(&mut source, &mut copy)?;
                    copy.sync_all()?;
                }
                Err(e) if e.errno == Some(libc::ENOENT) => {}
                Err(e) => return Err(e),
            }
        }
        dest.mkdir_all(&ArchivePath::parse("inspection")?)?;
        for name in ["catalog.sqlite", "catalog.sqlite-wal", "catalog.sqlite-shm"] {
            if let Ok(mut source) = dest.open_relative(
                &ArchivePath::parse(&format!("preserved/{name}"))?,
                libc::O_RDONLY,
                0,
            ) {
                let mut copy =
                    dest.create_new(&ArchivePath::parse(&format!("inspection/{name}"))?)?;
                std::io::copy(&mut source, &mut copy)?;
                copy.sync_all()?;
            }
        }
        super::vfs::register()?;
        if let Ok(db) = Connection::open_with_flags_and_vfs(
            format!("/proc/self/fd/{}/inspection/catalog.sqlite", dest.fd()),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            "photo-archive-fd",
        ) && let Ok(version) =
            db.pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            && version > schema::VERSION
        {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "newer catalog cannot be rebuilt by this version; forensic files preserved",
            ));
        }
        let writer = WriterId::new();
        let mut cat = Catalog::open(&dest)?;
        cat.db.execute(
            "INSERT INTO archives(id,writer) VALUES(?,?)",
            rusqlite::params![request.archive.to_string(), writer.to_string()],
        )?;
        let mut applied = 0;
        for commit in request.commits {
            if commit.archive_id != request.archive {
                return Err(AppError::integrity("rebuild commit archive mismatch"));
            }
            cat.apply_commit(&commit)?;
            applied += 1;
        }
        let mut inventory = crate::import::Inventory::open_forensic(local)?;
        let mut observed = 0;
        loop {
            let batch = inventory.next_batch(128)?;
            if batch.is_empty() {
                break;
            }
            for entry in batch {
                let mut file = entry.root.open_relative(&entry.path, libc::O_RDONLY, 0)?;
                let (digest, size) = crate::transfer::digest_file(&mut file)?;
                entry.validate()?;
                cat.db.execute("INSERT INTO observations(path,digest,size,readable) VALUES(?,?,?,1) ON CONFLICT(path) DO UPDATE SET digest=excluded.digest,size=excluded.size,readable=1",rusqlite::params![entry.path.as_str(),digest.as_str(),size])?;
                observed += 1;
            }
        }
        cat.db.execute(
            "UPDATE residency SET protected=1,pin=0,ready_revision=NULL,metadata_current=0",
            [],
        )?;
        let report=RecoveryReport{writer,replacement:request.replacement,private_state_lost:true,local_observations:observed,commits_applied:applied,limitations:vec!["Pins, private jobs, local-only version ancestry and unpublished snapshots are not restored from shared facts; retained local bytes require reconciliation.".into(),"Only supplied validated journal records were replayed; archive catch-up is required before publication or eviction.".into()]};
        cat.block_record("rebuild-incomplete","reconcile retained local bytes and finish archive replay before removing recovery protection")?;
        dest.write_new(
            &ArchivePath::parse("recovery.json")?,
            &serde_json::to_vec(&report)?,
        )?;
        dest.sync()?;
        Ok(report)
    }
}
