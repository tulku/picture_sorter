use super::LocalEvent;
use crate::{
    domain::ArchivePath,
    error::{AppError, AppResult},
    fs::RootHandle,
};
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs::File,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};
pub struct LocalWatcher {
    root: RootHandle,
    fd: OwnedFd,
    directories: BTreeMap<i32, String>,
}
impl LocalWatcher {
    pub fn open(root: &RootHandle) -> AppResult<Self> {
        // The returned inotify descriptor is owned and nonblocking from creation.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // A successful inotify_init1 returns a newly owned descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut watcher = Self {
            root: root.try_clone()?,
            fd,
            directories: BTreeMap::new(),
        };
        watcher.rescan()?;
        Ok(watcher)
    }
    fn rescan(&mut self) -> AppResult<()> {
        self.root.validate_identity()?;
        let mut stack = vec![(
            File::open(format!("/proc/self/fd/{}", self.root.fd()))?,
            String::new(),
        )];
        while let Some((dir, prefix)) = stack.pop() {
            let name = CString::new(format!("/proc/self/fd/{}", dir.as_raw_fd()))
                .map_err(|_| AppError::invalid("NUL"))?;
            let mask = libc::IN_CLOSE_WRITE
                | libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_MOVED_FROM
                | libc::IN_MOVED_TO
                | libc::IN_DELETE_SELF
                | libc::IN_MOVE_SELF;
            // Each watch follows only the procfs reference to an already confined directory descriptor.
            let wd = unsafe { libc::inotify_add_watch(self.fd.as_raw_fd(), name.as_ptr(), mask) };
            if wd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            self.directories.insert(wd, prefix.clone());
            for entry in std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))? {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .ok_or_else(|| AppError::invalid("non-UTF-8 watched directory"))?;
                if name == ".photo-sorter" {
                    continue;
                }
                let relative = ArchivePath::parse(&format!("{prefix}{name}"))?;
                stack.push((
                    self.root
                        .open_relative(&relative, libc::O_RDONLY | libc::O_DIRECTORY, 0)?,
                    format!("{}/", relative.as_str()),
                ));
            }
        }
        Ok(())
    }
    pub fn poll(&mut self) -> AppResult<Vec<LocalEvent>> {
        self.root.validate_identity()?;
        let mut events = Vec::new();
        let mut buffer = [0u8; 64 * 1024];
        // read writes at most the provided buffer length through the owned inotify descriptor.
        let count = unsafe {
            libc::read(
                self.fd.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if count < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(events);
            }
            return Err(e.into());
        }
        let mut offset = 0;
        let mut rescan = false;
        while offset + std::mem::size_of::<libc::inotify_event>() <= count as usize {
            // Kernel events can be unaligned within the byte buffer and are length-checked before reading.
            let event = unsafe {
                std::ptr::read_unaligned(buffer.as_ptr().add(offset).cast::<libc::inotify_event>())
            };
            offset += std::mem::size_of::<libc::inotify_event>();
            if offset + event.len as usize > count as usize {
                return Err(AppError::integrity("truncated inotify event"));
            }
            let name = &buffer[offset..offset + event.len as usize];
            offset += event.len as usize;
            if event.mask
                & (libc::IN_Q_OVERFLOW
                    | libc::IN_DELETE_SELF
                    | libc::IN_MOVE_SELF
                    | libc::IN_IGNORED)
                != 0
                || event.mask & libc::IN_ISDIR != 0
            {
                rescan = true;
                continue;
            }
            let name = &name[..name.iter().position(|b| *b == 0).unwrap_or(name.len())];
            if let (Some(prefix), Ok(name)) =
                (self.directories.get(&event.wd), std::str::from_utf8(name))
            {
                if !name.is_empty() {
                    events.push(LocalEvent::Changed(ArchivePath::parse(&format!(
                        "{prefix}{name}"
                    ))?));
                }
            } else {
                rescan = true;
            }
        }
        if rescan {
            events.push(LocalEvent::RescanRequired);
            self.rescan()?;
        }
        Ok(events)
    }
}
