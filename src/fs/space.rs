use super::RootHandle;
use crate::error::{AppError, AppResult};
pub fn available_bytes(root: &RootHandle) -> AppResult<u64> {
    root.validate_identity()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // fstatvfs fills the structure on success using the held filesystem descriptor.
    if unsafe { libc::fstatvfs(root.fd(), st.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // The successful syscall initialized the structure.
    let st = unsafe { st.assume_init() };
    st.f_bavail
        .checked_mul(st.f_frsize)
        .ok_or_else(|| AppError::invalid("free-space count overflow"))
}
pub fn occupied_bytes(root: &RootHandle) -> AppResult<u64> {
    occupied(root, false)
}
pub fn state_occupied_bytes(root: &RootHandle) -> AppResult<u64> {
    root.require_local()?;
    occupied(root, true)
}
fn occupied(root: &RootHandle, allow_service_socket: bool) -> AppResult<u64> {
    use std::os::fd::AsRawFd;
    let mut stack = vec![(
        std::fs::File::open(format!("/proc/self/fd/{}", root.fd()))?,
        String::new(),
    )];
    let mut total = 0u64;
    let mut seen = std::collections::HashSet::new();
    while let Some((dir, prefix)) = stack.pop() {
        for entry in std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| AppError::invalid("non-UTF-8 managed filename"))?;
            let path = super::ArchivePath::parse(&format!("{prefix}{name}"))?;
            let kind = entry.file_type()?;
            if allow_service_socket && path.as_str() == "service.sock" {
                use std::os::unix::fs::{FileTypeExt, MetadataExt};
                let metadata = entry.metadata()?;
                if !kind.is_socket()
                    || metadata.uid() != unsafe { libc::geteuid() }
                    || metadata.mode() & 0o777 != 0o600
                {
                    return Err(AppError::invalid(
                        "unexpected state socket type, owner or permissions",
                    ));
                }
                continue;
            }
            if kind.is_dir() {
                stack.push((
                    root.open_relative(&path, libc::O_RDONLY | libc::O_DIRECTORY, 0)?,
                    format!("{}/", path.as_str()),
                ));
            } else if kind.is_file() {
                let m = root.metadata_relative(&path)?;
                if u32::from(m.stx_mode) & libc::S_IFMT != libc::S_IFREG {
                    return Err(AppError::invalid(
                        "managed file changed type during occupancy check",
                    ));
                }
                if seen.insert((m.stx_dev_major, m.stx_dev_minor, m.stx_ino)) {
                    total = total
                        .checked_add(m.stx_size)
                        .ok_or_else(|| AppError::invalid("occupancy overflow"))?;
                }
            } else {
                return Err(AppError::invalid(
                    "symlink or special file in managed root; preserve and resolve before admission",
                ));
            }
        }
    }
    root.validate_identity()?;
    Ok(total)
}
