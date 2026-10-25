use super::RootHandle;
use crate::{
    domain::ArchivePath,
    error::{AppError, AppResult, ErrorCode},
};
use std::os::fd::AsRawFd;
#[derive(Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    Published,
    Exists,
}
pub fn publish_noreplace(
    root: &RootHandle,
    staged: &ArchivePath,
    dest: &ArchivePath,
) -> AppResult<PublishOutcome> {
    root.validate_identity()?;
    root.open_relative(staged, libc::O_RDONLY, 0)?.sync_all()?;
    if root.case_collision(dest)? {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "case-equivalent destination exists",
        ));
    }
    let (source_dir, source) = root.parent(staged)?;
    let (dest_dir, destination) = root.parent(dest)?;
    // RENAME_NOREPLACE guarantees that an existing destination cannot be overwritten.
    let rc = unsafe {
        libc::renameat2(
            source_dir.as_raw_fd(),
            source.as_ptr(),
            dest_dir.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EEXIST) {
            return Ok(PublishOutcome::Exists);
        }
        return Err(e.into());
    }
    source_dir.sync_all()?;
    dest_dir.sync_all()?;
    root.validate_identity()?;
    Ok(PublishOutcome::Published)
}

pub fn link_noreplace(
    root: &RootHandle,
    source: &ArchivePath,
    destination: &ArchivePath,
) -> AppResult<PublishOutcome> {
    root.require_local()?;
    root.validate_identity()?;
    root.open_relative(source, libc::O_RDONLY | libc::O_NONBLOCK, 0)?
        .sync_all()?;
    let (source_dir, source_name) = root.parent(source)?;
    let (dest_dir, dest_name) = root.parent(destination)?;
    // Both parents are held beneath the same local root; linkat never replaces an existing destination.
    if unsafe {
        libc::linkat(
            source_dir.as_raw_fd(),
            source_name.as_ptr(),
            dest_dir.as_raw_fd(),
            dest_name.as_ptr(),
            0,
        )
    } != 0
    {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EEXIST) {
            return Ok(PublishOutcome::Exists);
        }
        return Err(e.into());
    }
    dest_dir.sync_all()?;
    root.validate_identity()?;
    Ok(PublishOutcome::Published)
}
