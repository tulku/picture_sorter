use super::SidecarKind;
use crate::{
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
    transfer::digest_file,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotStamp {
    pub inode: u64,
    pub size: u64,
    pub ctime: i64,
    pub ctime_nsec: i64,
    pub mtime: i64,
    pub mtime_nsec: i64,
}
impl SnapshotStamp {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            inode: metadata.ino(),
            size: metadata.len(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotReceipt {
    pub path: ArchivePath,
    pub body: VersionBody,
    pub snapshot: Option<ArchivePath>,
    pub stamp: Option<SnapshotStamp>,
    pub source_stamp: Option<SnapshotStamp>,
    pub state_device: u64,
    pub state_inode: u64,
    pub opaque_invalid: bool,
}
#[derive(Debug, Clone)]
pub struct StoredSnapshot {
    receipt: SnapshotReceipt,
}
impl StoredSnapshot {
    pub(crate) fn from_receipt(receipt: SnapshotReceipt, state: &RootHandle) -> AppResult<Self> {
        state.require_local()?;
        if receipt.state_device != state.device() || receipt.state_inode != state.inode() {
            return Err(AppError::integrity(
                "snapshot receipt belongs to another state root",
            ));
        }
        let VersionBody::Content { digest, size } = &receipt.body else {
            return Err(AppError::integrity("stored snapshot must contain bytes"));
        };
        let expected = snapshot_path(digest)?;
        if receipt.snapshot.as_ref() != Some(&expected) {
            return Err(AppError::integrity("stored snapshot pathname mismatch"));
        }
        let metadata = state
            .open_relative(&expected, libc::O_RDONLY | libc::O_NONBLOCK, 0)?
            .metadata()?;
        if !metadata.is_file()
            || metadata.len() != *size
            || receipt.stamp.as_ref() != Some(&SnapshotStamp::of(&metadata))
        {
            return Err(AppError::integrity(
                "verified stored snapshot changed before recording",
            ));
        }
        Ok(Self { receipt })
    }
    pub(crate) fn body(&self) -> VersionBody {
        self.receipt.body.clone()
    }
    pub(crate) fn register(
        &self,
        root: &RootHandle,
        catalog: &mut crate::catalog::Catalog,
    ) -> AppResult<()> {
        Self::from_receipt(self.receipt.clone(), root)?;
        let VersionBody::Content { digest, size } = &self.receipt.body else {
            return Err(AppError::integrity("stored content required"));
        };
        let path = self
            .receipt
            .snapshot
            .as_ref()
            .ok_or_else(|| AppError::integrity("snapshot path missing"))?;
        catalog.db.execute(
            "INSERT INTO snapshots(digest,path,size) VALUES(?,?,?) ON CONFLICT DO NOTHING",
            rusqlite::params![digest.as_str(), path.as_str(), size],
        )?;
        Ok(())
    }
}
pub fn snapshot_path(digest: &Digest) -> AppResult<ArchivePath> {
    ArchivePath::parse(&format!(
        ".photo-sorter/snapshots/{}",
        &digest.as_str()[7..]
    ))
}
pub(crate) fn capture_local(
    local: &RootHandle,
    state: &RootHandle,
    path: &ArchivePath,
    kind: SidecarKind,
    ceiling: u64,
) -> AppResult<SnapshotReceipt> {
    capture_with_stage(local, state, path, kind, ceiling, None)
}
fn capture_with_stage(
    local: &RootHandle,
    state: &RootHandle,
    path: &ArchivePath,
    kind: SidecarKind,
    ceiling: u64,
    owned: Option<OperationId>,
) -> AppResult<SnapshotReceipt> {
    local.require_local()?;
    state.require_local()?;
    if !path.as_str().starts_with(".photo-sorter/captures/") {
        validate_sidecar_path(path)?;
    }
    let mut source = match local.open_relative(path, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(e) if e.errno == Some(libc::ENOENT) => {
            let (parent, name) = local.parent(path)?;
            let wanted = name
                .to_str()
                .map_err(|_| AppError::invalid("sidecar filename must be UTF-8"))?;
            for entry in std::fs::read_dir(format!("/proc/self/fd/{}", parent.as_raw_fd()))? {
                let entry = entry?;
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(wanted))
                {
                    return Err(AppError::new(
                        ErrorCode::Unavailable,
                        "sidecar changed during complete absence observation",
                    ));
                }
            }
            local.validate_identity()?;
            return Ok(SnapshotReceipt {
                path: path.clone(),
                body: VersionBody::Reset,
                snapshot: None,
                stamp: None,
                source_stamp: None,
                state_device: state.device(),
                state_inode: state.inode(),
                opaque_invalid: false,
            });
        }
        Err(e) => return Err(e),
    };
    let before = source.metadata()?;
    if !before.is_file() {
        return Err(AppError::invalid("sidecar must be a regular file"));
    }
    state.mkdir_all(&ArchivePath::parse(".photo-sorter/snapshots")?)?;
    let stage = if let Some(operation) = owned {
        capture_path(operation, "payload")?
    } else {
        ArchivePath::parse(&format!(
            ".photo-sorter/snapshots/capture-{}",
            OperationId::new()
        ))?
    };
    let mut destination = state.create_new(&stage)?;
    let mut hash = blake3::Hasher::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        destination.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
        size = size
            .checked_add(n as u64)
            .ok_or_else(|| AppError::invalid("snapshot size overflow"))?;
    }
    destination.sync_all()?;
    let digest = Digest::parse(&format!("blake3:{}", hash.finalize().to_hex()))?;
    let mut second = local.open_relative(path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
    if digest_file(&mut second)? != (digest.clone(), size)
        || SnapshotStamp::of(&before) != SnapshotStamp::of(&source.metadata()?)
        || SnapshotStamp::of(&before) != SnapshotStamp::of(&second.metadata()?)
    {
        return Err(AppError::new(
            ErrorCode::SourceChanged,
            "sidecar changed during coherent snapshot; partial evidence retained",
        ));
    }
    let target = snapshot_path(&digest)?;
    let published = if owned.is_some() {
        link_noreplace(state, &stage, &target)?
    } else {
        publish_noreplace(state, &stage, &target)?
    };
    if published == PublishOutcome::Exists {
        let mut existing = state.open_relative(&target, libc::O_RDONLY, 0)?;
        if digest_file(&mut existing)? != (digest.clone(), size) {
            return Err(AppError::integrity("stored immutable snapshot is corrupt"));
        }
    }
    let opaque_invalid = kind == SidecarKind::RapidRaw
        && (size > ceiling
            || serde_json::from_slice::<serde_json::Value>(&state.read(&target, ceiling)?)
                .ok()
                .is_none_or(|v| !v.is_object()));
    local.validate_identity()?;
    state.validate_identity()?;
    let stamp = Some(SnapshotStamp::of(
        &state
            .open_relative(&target, libc::O_RDONLY, 0)?
            .metadata()?,
    ));
    Ok(SnapshotReceipt {
        path: path.clone(),
        body: VersionBody::Content { digest, size },
        snapshot: Some(target),
        stamp,
        source_stamp: Some(SnapshotStamp::of(&before)),
        state_device: state.device(),
        state_inode: state.inode(),
        opaque_invalid,
    })
}

impl SnapshotReceipt {
    pub(crate) fn still_matches(&self, local: &RootHandle) -> AppResult<bool> {
        local.require_local()?;
        match local.open_relative(&self.path, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
            Ok(file) => {
                Ok(self.source_stamp.as_ref() == Some(&SnapshotStamp::of(&file.metadata()?)))
            }
            Err(error) if error.errno == Some(libc::ENOENT) && self.body == VersionBody::Reset => {
                Ok(local.parent(&self.path).is_ok())
            }
            Err(_) => Ok(false),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureContext {
    pub operation: OperationId,
    pub owner: super::SidecarOwner,
    pub previous: Option<VersionId>,
}
fn capture_path(operation: OperationId, name: &str) -> AppResult<ArchivePath> {
    ArchivePath::parse(&format!(".photo-sorter/captures/{operation}/{name}"))
}
impl CaptureContext {
    pub fn persist(&self, state: &RootHandle) -> AppResult<()> {
        state.require_local()?;
        state.mkdir_all(&ArchivePath::parse(&format!(
            ".photo-sorter/captures/{}",
            self.operation
        ))?)?;
        let path = capture_path(self.operation, "intent.json")?;
        let bytes = serde_json::to_vec(self)?;
        match state.write_new(&path, &bytes) {
            Ok(()) => Ok(()),
            Err(e) if e.errno == Some(libc::EEXIST) && state.read(&path, 65536)? == bytes => Ok(()),
            Err(e) => Err(e),
        }
    }
    pub(crate) fn cleanup(&self, state: &RootHandle) -> AppResult<()> {
        for name in ["payload", "receipt.json", "intent.json"] {
            match state.unlink(&capture_path(self.operation, name)?, false) {
                Ok(()) => {}
                Err(e) if e.errno == Some(libc::ENOENT) => {}
                Err(e) => return Err(e),
            }
        }
        state.unlink(
            &ArchivePath::parse(&format!(".photo-sorter/captures/{}", self.operation))?,
            true,
        )
    }
}
pub(crate) fn capture_owned(
    local: &RootHandle,
    state: &RootHandle,
    context: &CaptureContext,
    ceiling: u64,
) -> AppResult<SnapshotReceipt> {
    context.persist(state)?;
    let receipt = capture_with_stage(
        local,
        state,
        &context.owner.path,
        context.owner.kind,
        ceiling,
        Some(context.operation),
    )?;
    state.write_new(
        &capture_path(context.operation, "receipt.json")?,
        &serde_json::to_vec(&receipt)?,
    )?;
    Ok(receipt)
}
pub(crate) fn recover_capture(
    state: &RootHandle,
    context: &CaptureContext,
    ceiling: u64,
) -> AppResult<SnapshotReceipt> {
    context.persist(state)?;
    match state.read(&capture_path(context.operation, "receipt.json")?, 65536) {
        Ok(bytes) => {
            let receipt: SnapshotReceipt = serde_json::from_slice(&bytes)?;
            if receipt.path != context.owner.path {
                return Err(AppError::integrity("capture receipt owner mismatch"));
            }
            if let VersionBody::Content { digest, size } = &receipt.body {
                let mut file = state.open_relative(&snapshot_path(digest)?, libc::O_RDONLY, 0)?;
                if digest_file(&mut file)? != (digest.clone(), *size) {
                    return Err(AppError::integrity("recovered snapshot corrupt"));
                }
            }
            Ok(receipt)
        }
        Err(e) if e.errno == Some(libc::ENOENT) => {
            let payload = capture_path(context.operation, "payload")?;
            let mut receipt = capture_with_stage(
                state,
                state,
                &payload,
                super::SidecarKind::Opaque,
                ceiling,
                None,
            )?;
            if receipt.body == VersionBody::Reset {
                return Err(AppError::new(
                    ErrorCode::NotFound,
                    "capture stopped before retaining payload",
                ));
            }
            receipt.path = context.owner.path.clone();
            receipt.source_stamp = None;
            receipt.opaque_invalid = true;
            Ok(receipt)
        }
        Err(e) => Err(e),
    }
}
