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
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RootUsage {
    pub total: u64,
    pub snapshots: u64,
    pub staging: u64,
    pub companions: u64,
    pub retained: u64,
    pub catalog: u64,
    pub other: u64,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ManagedSpaceMeasurement {
    pub local: RootUsage,
    pub state: RootUsage,
    pub filesystem_free: u64,
    pub state_filesystem_free: u64,
}
pub fn measure_managed_space(
    local: &RootHandle,
    state: &RootHandle,
) -> AppResult<ManagedSpaceMeasurement> {
    local.require_local()?;
    state.require_local()?;
    Ok(ManagedSpaceMeasurement {
        local: measure(local, false)?,
        state: measure(state, true)?,
        filesystem_free: available_bytes(local)?,
        state_filesystem_free: available_bytes(state)?,
    })
}
fn occupied(root: &RootHandle, state: bool) -> AppResult<u64> {
    Ok(measure(root, state)?.total)
}
fn category(path: &str, state: bool) -> (&'static str, u8) {
    if state && path.starts_with(".photo-sorter/snapshots/") {
        ("snapshots", 0)
    } else if path.starts_with(".photo-sorter/staging/")
        || (state && path.starts_with(".photo-sorter/captures/"))
    {
        ("staging", 1)
    } else if !state && path.starts_with(".photo-sorter/companions/") {
        ("companions", 2)
    } else if !state
        && (path.starts_with(".photo-sorter/displaced/")
            || path.starts_with(".photo-sorter/retained/"))
    {
        ("retained", 3)
    } else if state
        && matches!(
            path,
            "catalog.sqlite" | "catalog.sqlite-wal" | "catalog.sqlite-shm"
        )
    {
        ("catalog", 4)
    } else {
        ("other", 5)
    }
}
fn measure(root: &RootHandle, state: bool) -> AppResult<RootUsage> {
    use std::os::fd::AsRawFd;
    struct Frame {
        _dir: std::fs::File,
        entries: std::fs::ReadDir,
        prefix: String,
    }
    fn frame(directory: std::fs::File, prefix: String) -> AppResult<Frame> {
        let entries = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
        Ok(Frame {
            _dir: directory,
            entries,
            prefix,
        })
    }
    let directory = std::fs::File::open(format!("/proc/self/fd/{}", root.fd()))?;
    let mut stack = vec![frame(directory, String::new())?];
    let mut inodes = std::collections::HashMap::new();
    while let Some(current) = stack.last_mut() {
        let Some(entry) = current.entries.next() else {
            stack.pop();
            continue;
        };
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| AppError::invalid("non-UTF-8 managed filename"))?;
        let path = super::ArchivePath::parse(&format!("{}{name}", current.prefix))?;
        let kind = entry.file_type()?;
        if state && path.as_str() == "service.sock" {
            let metadata = root.metadata_relative(&path)?;
            if u32::from(metadata.stx_mode) & libc::S_IFMT != libc::S_IFSOCK
                || metadata.stx_uid != unsafe { libc::geteuid() }
                || metadata.stx_mode & 0o777 != 0o600
            {
                return Err(AppError::invalid(
                    "unexpected state socket type, owner or permissions",
                ));
            }
            continue;
        }
        if kind.is_dir() {
            stack.push(frame(
                root.open_relative(&path, libc::O_RDONLY | libc::O_DIRECTORY, 0)?,
                format!("{}/", path.as_str()),
            )?);
        } else if kind.is_file() {
            let metadata = root.metadata_relative(&path)?;
            if u32::from(metadata.stx_mode) & libc::S_IFMT != libc::S_IFREG {
                return Err(AppError::invalid(
                    "managed file changed type during occupancy check",
                ));
            }
            let key = (
                metadata.stx_dev_major,
                metadata.stx_dev_minor,
                metadata.stx_ino,
            );
            let (class, priority) = category(path.as_str(), state);
            let item = inodes
                .entry(key)
                .or_insert((class, priority, metadata.stx_size));
            if priority < item.1 {
                item.0 = class;
                item.1 = priority;
            }
            item.2 = item.2.max(metadata.stx_size);
        } else {
            return Err(AppError::invalid(
                "symlink or special file in managed root; preserve and resolve before admission",
            ));
        }
    }
    root.validate_identity()?;
    let mut usage = RootUsage::default();
    for (class, _, bytes) in inodes.into_values() {
        usage.total = usage
            .total
            .checked_add(bytes)
            .ok_or_else(|| AppError::invalid("occupancy overflow"))?;
        let target = match class {
            "snapshots" => &mut usage.snapshots,
            "staging" => &mut usage.staging,
            "companions" => &mut usage.companions,
            "retained" => &mut usage.retained,
            "catalog" => &mut usage.catalog,
            _ => &mut usage.other,
        };
        *target = target
            .checked_add(bytes)
            .ok_or_else(|| AppError::invalid("occupancy category overflow"))?;
    }
    Ok(usage)
}
