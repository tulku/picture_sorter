mod client;
pub mod protocol;
pub(crate) mod server;
mod status;
use crate::{
    catalog::*,
    config::Config,
    domain::*,
    error::{AppError, AppResult},
    fs::{RootHandle, RootPolicy},
};
pub use client::Client;
pub use status::*;
use std::path::Path;
pub struct Service;
impl Service {
    pub fn run(config: Config) -> AppResult<()> {
        server::run(config)
    }
}
pub fn offline_backup(config: &Config, destination: &Path) -> AppResult<()> {
    let state = RootHandle::open(&config.state_root, RootPolicy::Local)?;
    let _profile = OwnershipLock::acquire(&state, "profile.lock")?;
    let local = RootHandle::open(&config.working_root, RootPolicy::Local)?;
    let _work = OwnershipLock::acquire(&local, "working.lock")?;
    let parent = RootHandle::open(
        destination
            .parent()
            .ok_or_else(|| AppError::invalid("backup needs parent"))?,
        RootPolicy::Local,
    )?;
    let preserved = ArchivePath::parse(&format!("forensic-{}", OperationId::new()))?;
    parent.mkdir_all(&preserved)?;
    for name in ["catalog.sqlite", "catalog.sqlite-wal", "catalog.sqlite-shm"] {
        match state.open_relative(&ArchivePath::parse(name)?, libc::O_RDONLY, 0) {
            Ok(mut source) => {
                let mut dest = parent.create_new(&ArchivePath::parse(&format!(
                    "{}/{name}",
                    preserved.as_str()
                ))?)?;
                std::io::copy(&mut source, &mut dest)?;
                dest.sync_all()?;
            }
            Err(e) if e.errno == Some(libc::ENOENT) && name != "catalog.sqlite" => {}
            Err(e) => return Err(e),
        }
    }
    crate::catalog::backup_readonly(&state, destination)?;
    Ok(())
}
mod recovery;
pub use recovery::{RecoveryReport, ServiceRecovery};
mod local_transfer;
mod scan;
mod scheduler;
pub use scheduler::*;
mod archive_reader;
mod http;
mod observation;
mod publication;

mod pin;

mod metadata_merge;
