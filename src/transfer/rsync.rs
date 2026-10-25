use crate::{
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::{RootHandle, RootPolicy},
};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::PathBuf,
    process::{Command, Stdio},
};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceLocation {
    pub root: PathBuf,
    pub path: ArchivePath,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferSpec {
    pub job: JobId,
    pub generation: u64,
    pub source: SourceLocation,
    pub destination_root: PathBuf,
    pub staged_path: ArchivePath,
    pub expected_digest: Digest,
    pub expected_size: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferReceipt {
    pub transferred_bytes: u64,
    pub job: JobId,
    pub generation: u64,
    pub digest: Digest,
    pub size: u64,
    pub staged_path: ArchivePath,
}
impl TransferReceipt {
    pub fn matches(&self, job: JobId, generation: u64) -> bool {
        self.job == job && self.generation == generation
    }
}
pub struct TransferEngine;
pub fn digest_file(file: &mut std::fs::File) -> AppResult<(Digest, u64)> {
    let mut hash = blake3::Hasher::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        count = count
            .checked_add(n as u64)
            .ok_or_else(|| AppError::invalid("file size overflow"))?;
    }
    Ok((
        Digest::parse(&format!("blake3:{}", hash.finalize().to_hex()))?,
        count,
    ))
}
pub fn staging_path(job: JobId, generation: u64) -> ArchivePath {
    ArchivePath::parse(&format!(".photo-sorter/staging/{job}/{generation}/payload"))
        .expect("typed job and generation form a valid relative path")
}
impl TransferEngine {
    pub fn arguments(source: &str, dest: &str) -> Vec<String> {
        [
            "--checksum",
            "--copy-links",
            "--partial",
            "--no-whole-file",
            "--protect-args",
            "--",
            source,
            dest,
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
    }
    pub fn stage(spec: &TransferSpec) -> AppResult<TransferReceipt> {
        if spec.staged_path != staging_path(spec.job, spec.generation) {
            return Err(AppError::invalid(
                "transfer destination must be its job-owned hidden staging path",
            ));
        }
        let source = RootHandle::open(
            &spec.source.root,
            RootPolicy::Archive {
                expected_device: std::fs::metadata(&spec.source.root)?.dev(),
            },
        )?;
        let dest = RootHandle::open(
            &spec.destination_root,
            RootPolicy::Archive {
                expected_device: std::fs::metadata(&spec.destination_root)?.dev(),
            },
        )?;
        Self::stage_confined(spec, &source, &dest)
    }
    pub fn stage_confined(
        spec: &TransferSpec,
        source: &RootHandle,
        dest: &RootHandle,
    ) -> AppResult<TransferReceipt> {
        if spec.staged_path != staging_path(spec.job, spec.generation) {
            return Err(AppError::invalid(
                "transfer destination must be its job-owned hidden staging path",
            ));
        }
        use std::io::{Seek, SeekFrom};
        let mut original =
            source.open_relative(&spec.source.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        let before = original.metadata()?;
        if !before.is_file() {
            return Err(AppError::invalid("source must be regular file"));
        }
        if digest_file(&mut original)? != (spec.expected_digest.clone(), spec.expected_size) {
            return Err(AppError::new(
                ErrorCode::SourceChanged,
                "source digest/size changed",
            ));
        }
        original.seek(SeekFrom::Start(0))?;
        if let Some((parent, _)) = spec.staged_path.as_str().rsplit_once('/') {
            dest.mkdir_all(&ArchivePath::parse(parent)?)?;
        }
        let (parent, filename) = dest.parent(&spec.staged_path)?;
        let source_arg = format!("/proc/{}/fd/{}", std::process::id(), original.as_raw_fd());
        let dest_arg = format!(
            "/proc/{}/fd/{}/{}",
            std::process::id(),
            parent.as_raw_fd(),
            filename
                .to_str()
                .map_err(|_| AppError::invalid("UTF-8 path required"))?
        );
        let mut child = Command::new("rsync")
            .args(Self::arguments(&source_arg, &dest_arg))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                AppError::new(ErrorCode::Unavailable, format!("rsync unavailable: {e}"))
            })?;
        let mut error = Vec::new();
        child
            .stderr
            .take()
            .ok_or_else(|| AppError::integrity("missing rsync error pipe"))?
            .take(64 * 1024 + 1)
            .read_to_end(&mut error)?;
        if error.len() > 64 * 1024 {
            child.kill()?;
            child.wait()?;
            return Err(AppError::new(ErrorCode::Io, "rsync output exceeded bound"));
        }
        let status = child.wait()?;
        if !status.success() {
            return Err(AppError::new(
                ErrorCode::Io,
                format!(
                    "rsync failed ({status}): {}",
                    String::from_utf8_lossy(&error)
                ),
            ));
        }
        let after = original.metadata()?;
        let current = source
            .open_relative(&spec.source.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?
            .metadata()?;
        if before.ino() != after.ino()
            || before.len() != after.len()
            || before.modified()? != after.modified()?
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
            || current.ino() != before.ino()
        {
            return Err(AppError::new(
                ErrorCode::SourceChanged,
                "source changed during transfer",
            ));
        }
        let mut staged =
            dest.open_relative(&spec.staged_path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        let (digest, size) = digest_file(&mut staged)?;
        if digest != spec.expected_digest || size != spec.expected_size {
            return Err(AppError::integrity(
                "staged content mismatch; retained for recovery",
            ));
        }
        staged.sync_all()?;
        parent.sync_all()?;
        dest.validate_identity()?;
        source.validate_identity()?;
        Ok(TransferReceipt {
            transferred_bytes: size,
            job: spec.job,
            generation: spec.generation,
            digest,
            size,
            staged_path: spec.staged_path.clone(),
        })
    }
}
pub fn publish_local(
    root: &RootHandle,
    receipt: &TransferReceipt,
    destination: &ArchivePath,
) -> AppResult<()> {
    root.require_local()?;
    if receipt.staged_path != staging_path(receipt.job, receipt.generation) {
        return Err(AppError::integrity(
            "local receipt staging ownership mismatch",
        ));
    }
    if let Some((parent, _)) = destination.as_str().rsplit_once('/') {
        root.mkdir_all(&ArchivePath::parse(parent)?)?;
    }
    if root.case_collision(destination)? {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "local case collision; bytes preserved",
        ));
    }
    if crate::fs::publish_noreplace(root, &receipt.staged_path, destination)?
        == crate::fs::PublishOutcome::Exists
    {
        let mut existing = root.open_relative(destination, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        if digest_file(&mut existing)? != (receipt.digest.clone(), receipt.size) {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "different local bytes exist at destination; preserved",
            ));
        }
    }
    Ok(())
}
impl TransferEngine {
    pub fn local_transfer(
        spec: &TransferSpec,
        destination: &ArchivePath,
    ) -> AppResult<TransferReceipt> {
        let dest = RootHandle::open(&spec.destination_root, RootPolicy::Local)?;
        if let Some((parent, _)) = destination.as_str().rsplit_once('/') {
            dest.mkdir_all(&ArchivePath::parse(parent)?)?;
        }
        if dest.case_collision(destination)? {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "local case collision; bytes preserved",
            ));
        }
        match dest.open_relative(destination, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
            Ok(mut file) => {
                if digest_file(&mut file)? != (spec.expected_digest.clone(), spec.expected_size) {
                    return Err(AppError::new(
                        ErrorCode::Conflict,
                        "different local bytes at destination; preserved",
                    ));
                }
                return Ok(TransferReceipt {
                    transferred_bytes: 0,
                    job: spec.job,
                    generation: spec.generation,
                    digest: spec.expected_digest.clone(),
                    size: spec.expected_size,
                    staged_path: spec.staged_path.clone(),
                });
            }
            Err(e) if e.errno == Some(libc::ENOENT) => {}
            Err(e) => return Err(e),
        }
        let source = RootHandle::open(&spec.source.root, RootPolicy::Local)?;
        let receipt = Self::stage_confined(spec, &source, &dest)?;
        publish_local(&dest, &receipt, destination)?;
        Ok(receipt)
    }
}
