use crate::{
    domain::ArchivePath,
    error::{AppError, AppResult, ErrorCode},
};
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Copy)]
pub enum RootPolicy {
    Local,
    Archive { expected_device: u64 },
    TestArchive,
}
#[derive(Debug)]
pub struct RootHandle {
    dir: File,
    path: PathBuf,
    device: u64,
    inode: u64,
    policy: RootPolicy,
    mount_id: u64,
}
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
impl RootHandle {
    pub fn open(path: &Path, policy: RootPolicy) -> AppResult<Self> {
        if !path.is_absolute() {
            return Err(AppError::invalid("absolute root required"));
        }
        if matches!(policy, RootPolicy::Local | RootPolicy::TestArchive) {
            reject_network_root(path)?;
        }
        let dir = open_root_directory(path)?;
        let m = dir.metadata()?;
        if let RootPolicy::Archive { expected_device } = policy
            && m.dev() != expected_device
        {
            return Err(AppError::new(
                ErrorCode::Unavailable,
                "archive filesystem identity changed",
            ));
        }
        let mount_id = descriptor_mount_id(dir.as_raw_fd())?;
        let root = Self {
            dir,
            path: path.to_owned(),
            device: m.dev(),
            inode: m.ino(),
            mount_id,
            policy,
        };
        if matches!(policy, RootPolicy::Local | RootPolicy::TestArchive) && root.is_network()? {
            return Err(AppError::invalid(
                "local/test root must not be a network filesystem",
            ));
        }
        Ok(root)
    }
    pub fn try_clone(&self) -> AppResult<Self> {
        self.validate_identity()?;
        Ok(Self {
            dir: self.dir.try_clone()?,
            path: self.path.clone(),
            device: self.device,
            inode: self.inode,
            policy: self.policy,
            mount_id: self.mount_id,
        })
    }
    pub fn require_local(&self) -> AppResult<()> {
        self.validate_identity()?;
        if !matches!(self.policy, RootPolicy::Local) {
            return Err(AppError::invalid(
                "operation requires a local working/state root, never an archive root",
            ));
        }
        Ok(())
    }
    pub fn fd(&self) -> RawFd {
        self.dir.as_raw_fd()
    }
    pub fn metadata_relative(&self, path: &ArchivePath) -> AppResult<libc::statx> {
        let (parent, name) = self.parent(path)?;
        let mut metadata = std::mem::MaybeUninit::<libc::statx>::uninit();
        // statx checks the held parent without opening and closing a SQLite database inode.
        if unsafe {
            libc::statx(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT,
                libc::STATX_BASIC_STATS | libc::STATX_MNT_ID,
                metadata.as_mut_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        // A successful statx initializes the output structure.
        let metadata = unsafe { metadata.assume_init() };
        if metadata.stx_mask & libc::STATX_MNT_ID == 0 || metadata.stx_mnt_id != self.mount_id {
            return Err(AppError::new(
                ErrorCode::Unavailable,
                "managed file crosses a mount boundary",
            ));
        }
        Ok(metadata)
    }
    pub fn inode(&self) -> u64 {
        self.inode
    }
    pub fn is_test_archive(&self) -> bool {
        matches!(self.policy, RootPolicy::TestArchive)
    }
    pub fn mount_number(&self) -> u64 {
        self.mount_id
    }
    pub fn mount_identity(&self) -> AppResult<String> {
        self.validate_identity()?;
        let id = self.mount_id.to_string();
        let info = std::fs::read_to_string("/proc/self/mountinfo")?;
        let line = info
            .lines()
            .find(|line| line.split_whitespace().next() == Some(id.as_str()))
            .ok_or_else(|| AppError::invalid("held mount is absent from mountinfo"))?;
        Ok(format!(
            "linux:{}:{}:{}",
            self.device,
            self.inode,
            crate::domain::Digest::of(line.as_bytes()).as_str()
        ))
    }
    pub fn device(&self) -> u64 {
        self.device
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn validate_identity(&self) -> AppResult<()> {
        if matches!(self.policy, RootPolicy::Local | RootPolicy::TestArchive) {
            reject_network_root(&self.path)?;
        }
        let current = open_root_directory(&self.path)?;
        let m = current.metadata()?;
        if m.dev() != self.device
            || m.ino() != self.inode
            || descriptor_mount_id(current.as_raw_fd())? != self.mount_id
        {
            return Err(AppError::new(
                ErrorCode::Unavailable,
                "managed root replaced; refusing fallback",
            ));
        }
        Ok(())
    }
    pub fn open_relative(&self, path: &ArchivePath, flags: i32, mode: u32) -> AppResult<File> {
        self.validate_identity()?;
        let c = CString::new(path.as_str()).map_err(|_| AppError::invalid("NUL in path"))?;
        let how = OpenHow {
            flags: (flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
            mode: mode as u64,
            resolve: 0x08 | 0x04 | 0x01,
        };
        // openat2 resolves beneath the held root without symlinks or mount traversal.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                self.fd(),
                c.as_ptr(),
                &how,
                std::mem::size_of::<OpenHow>(),
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // A successful openat2 returns a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd as RawFd) })
    }
    pub fn create_new(&self, path: &ArchivePath) -> AppResult<File> {
        self.open_relative(path, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600)
    }
    pub fn write_new(&self, path: &ArchivePath, bytes: &[u8]) -> AppResult<()> {
        let mut f = self.create_new(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        self.sync()?;
        Ok(())
    }
    pub fn read(&self, path: &ArchivePath, ceiling: u64) -> AppResult<Vec<u8>> {
        let f = self.open_relative(path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        if !f.metadata()?.is_file() {
            return Err(AppError::invalid("expected regular file"));
        }
        let mut bytes = Vec::new();
        f.take(ceiling.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > ceiling {
            return Err(AppError::invalid("file exceeds bounded read limit"));
        }
        Ok(bytes)
    }
    pub fn parent(&self, path: &ArchivePath) -> AppResult<(File, CString)> {
        if let Some((dir, base)) = path.as_str().rsplit_once('/') {
            Ok((
                self.open_relative(
                    &ArchivePath::parse(dir)?,
                    libc::O_RDONLY | libc::O_DIRECTORY,
                    0,
                )?,
                CString::new(base).map_err(|_| AppError::invalid("NUL"))?,
            ))
        } else {
            Ok((
                self.dir.try_clone()?,
                CString::new(path.as_str()).map_err(|_| AppError::invalid("NUL"))?,
            ))
        }
    }
    pub fn mkdir_all(&self, path: &ArchivePath) -> AppResult<()> {
        self.validate_identity()?;
        let mut parent = self.dir.try_clone()?;
        for component in path.as_str().split('/') {
            let name = CString::new(component).map_err(|_| AppError::invalid("NUL"))?;
            // mkdirat acts only on the held parent; the opened child is checked before descent.
            let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
            if rc != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
                return Err(std::io::Error::last_os_error().into());
            }
            let how = OpenHow {
                flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
                mode: 0,
                resolve: 0x08 | 0x04 | 0x01,
            };
            // openat2 rejects replacement links and child mounts between mkdir and descent.
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_openat2,
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    &how,
                    std::mem::size_of::<OpenHow>(),
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            parent.sync_all()?;
            // Each successful syscall returns a new owned descriptor.
            parent = unsafe { File::from_raw_fd(fd as RawFd) };
        }
        parent.sync_all()?;
        Ok(())
    }
    pub fn case_collision(&self, path: &ArchivePath) -> AppResult<bool> {
        let (parent, name) = self.parent(path)?;
        let wanted = name
            .to_str()
            .map_err(|_| AppError::invalid("UTF-8 required"))?;
        for entry in std::fs::read_dir(format!("/proc/self/fd/{}", parent.as_raw_fd()))? {
            let entry = entry?;
            let actual = entry.file_name();
            let actual = actual
                .to_str()
                .ok_or_else(|| AppError::invalid("non-UTF-8 directory entry"))?;
            if actual != wanted && actual.to_lowercase() == wanted.to_lowercase() {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn sync(&self) -> AppResult<()> {
        self.dir.sync_all()?;
        Ok(())
    }
    pub fn is_network(&self) -> AppResult<bool> {
        let mut st = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // fstatfs initializes the structure on success and does not follow a path.
        if unsafe { libc::fstatfs(self.fd(), st.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // The successful fstatfs initialized every field read here.
        let kind = unsafe { st.assume_init() }.f_type as u64;
        Ok(matches!(kind, 0x6969 | 0xff534d42 | 0xfe534d42 | 0x517b))
    }
}
impl RootHandle {
    pub fn mkdir_exclusive(&self, path: &ArchivePath) -> AppResult<()> {
        self.validate_identity()?;
        let (parent, name) = self.parent(path)?;
        // mkdirat creates the lock directory atomically beneath its held parent.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        parent.sync_all()?;
        Ok(())
    }
    pub fn unlink(&self, path: &ArchivePath, directory: bool) -> AppResult<()> {
        self.validate_identity()?;
        let (parent, name) = self.parent(path)?;
        // unlinkat never follows the final path component and is confined to the held parent.
        if unsafe {
            libc::unlinkat(
                parent.as_raw_fd(),
                name.as_ptr(),
                if directory { libc::AT_REMOVEDIR } else { 0 },
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        parent.sync_all()?;
        Ok(())
    }
}
impl RootHandle {
    pub fn exchange(&self, left: &ArchivePath, right: &ArchivePath) -> AppResult<()> {
        self.validate_identity()?;
        let (a, x) = self.parent(left)?;
        let (b, y) = self.parent(right)?;
        // Atomic exchange retains the displaced bytes at the staging path even across a crash.
        if unsafe {
            libc::renameat2(
                a.as_raw_fd(),
                x.as_ptr(),
                b.as_raw_fd(),
                y.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        a.sync_all()?;
        b.sync_all()?;
        self.validate_identity()
    }
}

fn descriptor_mount_id(fd: RawFd) -> AppResult<u64> {
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}"))?;
    info.lines()
        .find_map(|line| {
            line.strip_prefix("mnt_id:")
                .and_then(|value| value.trim().parse().ok())
        })
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported,
                "kernel does not expose held-root mount identity",
            )
        })
}

fn open_root_directory(path: &Path) -> AppResult<File> {
    use std::os::unix::ffi::OsStrExt;
    let name =
        CString::new(path.as_os_str().as_bytes()).map_err(|_| AppError::invalid("NUL in root"))?;
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: 0x04 | 0x02,
    };
    // Root resolution itself must not follow a replaced ancestor symlink into another filesystem.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            name.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // A successful openat2 returns a new owned directory descriptor.
    Ok(unsafe { File::from_raw_fd(fd as RawFd) })
}
pub fn ensure_disjoint_roots(left: &RootHandle, right: &RootHandle) -> AppResult<()> {
    left.validate_identity()?;
    right.validate_identity()?;
    if left.device() != right.device() {
        return Ok(());
    }
    let info = std::fs::read_to_string("/proc/self/mountinfo")?;
    let a = physical_mount_path(&info, left.mount_number(), left.path())?;
    let b = physical_mount_path(&info, right.mount_number(), right.path())?;
    if left.inode() == right.inode() || a.starts_with(&b) || b.starts_with(&a) {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "managed roots physically overlap; archive and cache bytes must be independent",
        ));
    }
    Ok(())
}
fn decode_mount_path(value: &str) -> AppResult<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let input = value.as_bytes();
    let mut output = Vec::new();
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'\\'
            && index + 3 < input.len()
            && input[index + 1..index + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let value = u16::from(input[index + 1] - b'0') * 64
                + u16::from(input[index + 2] - b'0') * 8
                + u16::from(input[index + 3] - b'0');
            output.push(
                u8::try_from(value).map_err(|_| AppError::integrity("invalid mount escape"))?,
            );
            index += 4;
        } else {
            output.push(input[index]);
            index += 1;
        }
    }
    Ok(PathBuf::from(std::ffi::OsString::from_vec(output)))
}
fn physical_mount_path(info: &str, id: u64, path: &Path) -> AppResult<PathBuf> {
    let line = info
        .lines()
        .find(|line| {
            line.split_whitespace()
                .next()
                .and_then(|id| id.parse::<u64>().ok())
                == Some(id)
        })
        .ok_or_else(|| AppError::integrity("held root mount is absent"))?;
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 6 {
        return Err(AppError::integrity("incomplete mount identity"));
    }
    let filesystem_root = decode_mount_path(fields[3])?;
    let mountpoint = decode_mount_path(fields[4])?;
    let relative = path
        .strip_prefix(&mountpoint)
        .map_err(|_| AppError::integrity("held path is outside its mount identity"))?;
    if relative
        .components()
        .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(AppError::integrity("non-canonical physical root path"));
    }
    Ok(filesystem_root.join(relative))
}
fn reject_network_root(path: &Path) -> AppResult<()> {
    use std::os::unix::ffi::OsStringExt;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")?;
    let mut best: Option<(usize, &str)> = None;
    for line in mounts.lines() {
        let Some((fields, filesystem)) = line.split_once(" - ") else {
            continue;
        };
        let Some(mount) = fields.split_whitespace().nth(4) else {
            continue;
        };
        let kind = filesystem.split_whitespace().next().unwrap_or("");
        let input = mount.as_bytes();
        let mut decoded = Vec::new();
        let mut i = 0;
        while i < input.len() {
            if input[i] == b'\\'
                && i + 3 < input.len()
                && input[i + 1..i + 4]
                    .iter()
                    .all(|n| (b'0'..=b'7').contains(n))
            {
                let n =
                    (input[i + 1] - b'0') * 64 + (input[i + 2] - b'0') * 8 + (input[i + 3] - b'0');
                decoded.push(n);
                i += 4;
            } else {
                decoded.push(input[i]);
                i += 1;
            }
        }
        let mount = PathBuf::from(std::ffi::OsString::from_vec(decoded));
        if path.starts_with(&mount)
            && best.is_none_or(|(length, _)| mount.as_os_str().len() > length)
        {
            best = Some((mount.as_os_str().len(), kind));
        }
    }
    if best.is_some_and(|(_, kind)| {
        matches!(
            kind,
            "cifs" | "smb3" | "nfs" | "nfs4" | "9p" | "ceph" | "afs"
        ) || kind.starts_with("fuse.sshfs")
            || kind.starts_with("fuse.rclone")
    }) {
        return Err(AppError::invalid(
            "working/state/test roots must be local; network roots are worker-only",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod physical_root_tests {
    use super::*;
    #[test]
    fn mount_coordinates_identify_bind_aliases_and_nested_overlap() {
        let info = "10 1 8:1 / / rw - ext4 /dev/fixture rw\n20 10 8:1 /photos /cache rw - ext4 /dev/fixture rw\n30 10 8:1 /photos /archive rw - ext4 /dev/fixture rw\n40 10 8:1 /photos/nested /other rw - ext4 /dev/fixture rw\n";
        let cache = physical_mount_path(info, 20, Path::new("/cache")).unwrap();
        assert_eq!(
            cache,
            physical_mount_path(info, 30, Path::new("/archive")).unwrap()
        );
        assert!(
            physical_mount_path(info, 40, Path::new("/other"))
                .unwrap()
                .starts_with(cache)
        );
        assert_eq!(decode_mount_path("/a\\040b").unwrap(), Path::new("/a b"));
    }
}
