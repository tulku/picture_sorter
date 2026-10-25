use crate::{
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::{RootHandle, path_from_os},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, Metadata, ReadDir},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::Arc,
    time::SystemTime,
};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CompanionKind {
    Rrdata,
    Rrexif,
    Xmp,
    Orig,
    Opaque,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SourceRole {
    Original(MediaRole),
    Companion(CompanionKind),
}
#[derive(Debug, Clone)]
pub struct SourceEntry {
    pub root: Arc<RootHandle>,
    pub path: ArchivePath,
    pub size: u64,
    pub modified: SystemTime,
    pub fingerprint: String,
    pub role: SourceRole,
    pub(crate) inode: u64,
    pub(crate) ctime: (i64, i64),
}
impl SourceEntry {
    pub fn refresh(&self) -> AppResult<Self> {
        Self::from_metadata(
            self.root.clone(),
            self.path.clone(),
            self.role.clone(),
            &self
                .root
                .open_relative(&self.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?
                .metadata()?,
        )
    }
    fn from_metadata(
        root: Arc<RootHandle>,
        path: ArchivePath,
        role: SourceRole,
        m: &Metadata,
    ) -> AppResult<Self> {
        if !m.is_file() {
            return Err(AppError::invalid("source is not regular file"));
        }
        Ok(Self {
            root,
            path,
            size: m.len(),
            modified: m.modified()?,
            fingerprint: format!("{}:{}:{}", m.len(), m.mtime(), m.mtime_nsec()),
            role,
            inode: m.ino(),
            ctime: (m.ctime(), m.ctime_nsec()),
        })
    }
    pub fn validate(&self) -> AppResult<()> {
        let now = self.refresh()?;
        if now.size != self.size
            || now.modified != self.modified
            || now.inode != self.inode
            || now.ctime != self.ctime
        {
            return Err(AppError::new(
                ErrorCode::SourceChanged,
                "source changed during operation",
            ));
        }
        self.root.validate_identity()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub incarnation: String,
    pub reliable: bool,
}
struct Cursor {
    _dir: File,
    iter: ReadDir,
    prefix: String,
}
pub struct Inventory {
    forensic: bool,
    root: Arc<RootHandle>,
    stack: Vec<Cursor>,
    pub skipped: u64,
    pub diagnostics: Vec<String>,
}
impl Inventory {
    pub fn open(source: RootHandle) -> AppResult<Self> {
        let root = Arc::new(source);
        let dir = match root.open_relative(
            &ArchivePath::parse("DCIM")?,
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        ) {
            Ok(dir) => dir,
            Err(e) if e.errno == Some(libc::ENOENT) => {
                File::open(format!("/proc/self/fd/{}", root.fd()))?
            }
            Err(e) => return Err(e),
        };
        let prefix = if dir.metadata()?.ino()
            == File::open(format!("/proc/self/fd/{}", root.fd()))?
                .metadata()?
                .ino()
        {
            String::new()
        } else {
            "DCIM/".into()
        };
        let iter = std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))?;
        Ok(Self {
            root,
            forensic: false,
            stack: vec![Cursor {
                _dir: dir,
                iter,
                prefix,
            }],
            skipped: 0,
            diagnostics: vec![],
        })
    }
    pub fn open_all(source: RootHandle, scope: Option<&ArchivePath>) -> AppResult<Self> {
        let root = Arc::new(source);
        let (dir, prefix) = if let Some(scope) = scope {
            (
                root.open_relative(scope, libc::O_RDONLY | libc::O_DIRECTORY, 0)?,
                format!("{}/", scope.as_str()),
            )
        } else {
            (
                File::open(format!("/proc/self/fd/{}", root.fd()))?,
                String::new(),
            )
        };
        let iter = std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))?;
        Ok(Self {
            root,
            forensic: false,
            stack: vec![Cursor {
                _dir: dir,
                iter,
                prefix,
            }],
            skipped: 0,
            diagnostics: vec![],
        })
    }
    pub fn open_forensic(source: RootHandle) -> AppResult<Self> {
        let mut inventory = Self::open_all(source, None)?;
        inventory.forensic = true;
        Ok(inventory)
    }
    pub fn next_batch(&mut self, limit: usize) -> AppResult<Vec<SourceEntry>> {
        if limit == 0 || limit > 128 {
            return Err(AppError::invalid("inventory batches must be 1..128"));
        }
        let mut result = Vec::new();
        while result.len() < limit {
            let Some(cursor) = self.stack.last_mut() else {
                break;
            };
            let next = cursor.iter.next();
            let prefix = cursor.prefix.clone();
            let Some(next) = next else {
                self.stack.pop();
                continue;
            };
            let entry = next?;
            let name = entry.file_name();
            let name = match name.to_str() {
                Some(n) => n,
                None => {
                    self.skip("non-UTF-8 filename; rename before import");
                    continue;
                }
            };
            if (name.starts_with('.') && (!self.forensic || name == ".photo-sorter"))
                || (self.forensic && name == "working.lock")
            {
                self.skip("hidden entry excluded");
                continue;
            }
            let path = match path_from_os(std::ffi::OsStr::new(&format!("{prefix}{name}"))) {
                Ok(path) => path,
                Err(_) => {
                    self.skip("non-portable filename; rename before import");
                    continue;
                }
            };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                let dir = self
                    .root
                    .open_relative(&path, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
                let iter = std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))?;
                self.stack.push(Cursor {
                    _dir: dir,
                    iter,
                    prefix: format!("{}/", path.as_str()),
                });
                continue;
            }
            if !kind.is_file() {
                self.skip("symlink or special file excluded");
                continue;
            }
            let Some(role) = source_role(name).or_else(|| {
                self.forensic
                    .then_some(SourceRole::Companion(CompanionKind::Opaque))
            }) else {
                self.skip("unsupported source file preserved");
                continue;
            };
            let file = self
                .root
                .open_relative(&path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
            result.push(SourceEntry::from_metadata(
                self.root.clone(),
                path,
                role,
                &file.metadata()?,
            )?);
        }
        Ok(result)
    }
    fn skip(&mut self, reason: &str) {
        self.skipped += 1;
        if self.diagnostics.len() < 128 {
            self.diagnostics.push(reason.into());
        }
    }
}
pub fn source_role(name: &str) -> Option<SourceRole> {
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "cr2" | "nef" | "arw" | "dng" | "raw" | "orf" | "ori" => {
            SourceRole::Original(MediaRole::Raw)
        }
        "jpg" | "jpeg" => SourceRole::Original(MediaRole::Jpeg),
        "tif" | "tiff" | "png" => SourceRole::Original(MediaRole::OtherStill),
        "mov" | "mp4" | "avi" | "mts" | "m4v" | "mpg" | "mpeg" => {
            SourceRole::Original(MediaRole::Video)
        }
        "rrdata" => SourceRole::Companion(CompanionKind::Rrdata),
        "rrexif" => SourceRole::Companion(CompanionKind::Rrexif),
        "xmp" => SourceRole::Companion(CompanionKind::Xmp),
        "orig" => SourceRole::Companion(CompanionKind::Orig),
        "pp3" | "dop" | "on1" => SourceRole::Companion(CompanionKind::Opaque),
        _ => return None,
    })
}
