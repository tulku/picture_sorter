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
    pub(crate) fn of(metadata: &std::fs::Metadata) -> Self {
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
    #[serde(default)]
    pub archive_history: bool,
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
        validate_saved_path(path, kind.saved_owner())?;
    }
    let mut source = match local.open_relative(path, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(e) if e.errno == Some(libc::ENOENT) => {
            if kind == SidecarKind::Generated {
                return Err(AppError::new(
                    ErrorCode::Unavailable,
                    "generated output is absent; last save retained",
                ));
            }
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
                archive_history: false,
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
        if size
            .checked_add(n as u64)
            .is_none_or(|next| next > before.len())
        {
            return Err(AppError::new(
                ErrorCode::SourceChanged,
                "sidecar grew beyond its admitted snapshot size; partial retained",
            ));
        }
        destination.write_all(&buffer[..n])?;
        crate::metrics::hash_bytes(n as u64);
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
        archive_history: false,
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
    _ceiling: u64,
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
            let mut source = state.open_relative(&payload, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
            let metadata = source.metadata()?;
            if !metadata.is_file() {
                return Err(AppError::integrity("owned capture payload is not regular"));
            }
            let (digest, size) = digest_file(&mut source)?;
            let target = snapshot_path(&digest)?;
            state.mkdir_all(&ArchivePath::parse(".photo-sorter/snapshots")?)?;
            link_noreplace(state, &payload, &target)?;
            let mut stored = state.open_relative(&target, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
            if digest_file(&mut stored)? != (digest.clone(), size) {
                return Err(AppError::integrity("recovered capture payload differs"));
            }
            Ok(SnapshotReceipt {
                archive_history: false,
                path: context.owner.path.clone(),
                body: VersionBody::Content { digest, size },
                snapshot: Some(target),
                stamp: Some(SnapshotStamp::of(&stored.metadata()?)),
                source_stamp: None,
                state_device: state.device(),
                state_inode: state.inode(),
                opaque_invalid: true,
            })
        }
        Err(e) => Err(e),
    }
}
pub(crate) fn capture_history(
    local: &RootHandle,
    archive: &RootHandle,
    context: &CaptureContext,
    ceiling: u64,
) -> AppResult<SnapshotReceipt> {
    local.require_local()?;
    validate_saved_path(&context.owner.path, context.owner.kind.saved_owner())?;
    archive.validate_identity()?;
    let directory = ArchivePath::parse(&format!(
        ".photo-sorter/staging/history-capture-{}",
        context.operation
    ))?;
    archive.mkdir_all(&directory)?;
    let entry = |name: &str| ArchivePath::parse(&format!("{}/{name}", directory.as_str()));
    let intent = serde_json::to_vec(context)?;
    match archive.write_new(&entry("intent.json")?, &intent) {
        Ok(()) => {}
        Err(e)
            if e.errno == Some(libc::EEXIST)
                && archive.read(&entry("intent.json")?, 65536)? == intent => {}
        Err(e) => return Err(e),
    }
    match archive.read(&entry("receipt.json")?, 65536) {
        Ok(bytes) => {
            let receipt: SnapshotReceipt = serde_json::from_slice(&bytes)?;
            validate_history_receipt(archive, context, &receipt)?;
            cleanup_history_partial(archive, context, &receipt)?;
            return Ok(receipt);
        }
        Err(e) if e.errno == Some(libc::ENOENT) => {}
        Err(e) => return Err(e),
    }
    let payload = crate::transfer::staging_path(JobId(context.operation.0), 1);
    let prepared = entry("prepared.json")?;
    let mut receipt = match archive.read(&prepared, 65536) {
        Ok(bytes) => serde_json::from_slice::<SnapshotReceipt>(&bytes)?,
        Err(error) if error.errno == Some(libc::ENOENT) => {
            let previous = archive.open_relative(&payload, libc::O_RDONLY | libc::O_NONBLOCK, 0);
            let (digest, size, source_stamp, opaque_invalid) = match previous {
                Ok(mut partial) => {
                    if !partial.metadata()?.is_file() {
                        return Err(AppError::integrity("owned history partial is not regular"));
                    }
                    let (digest, size) = digest_file(&mut partial)?;
                    (digest, size, None, true)
                }
                Err(error) if error.errno == Some(libc::ENOENT) => {
                    let mut source = local.open_relative(
                        &context.owner.path,
                        libc::O_RDONLY | libc::O_NONBLOCK,
                        0,
                    )?;
                    let before = source.metadata()?;
                    if !before.is_file() {
                        return Err(AppError::invalid("saved sidecar is not regular"));
                    }
                    let (digest, size) = digest_file(&mut source)?;
                    if size != before.len()
                        || SnapshotStamp::of(&before) != SnapshotStamp::of(&source.metadata()?)
                    {
                        return Err(AppError::new(
                            ErrorCode::SourceChanged,
                            "saved sidecar changed before direct transfer",
                        ));
                    }
                    let spec = crate::transfer::TransferSpec {
                        job: JobId(context.operation.0),
                        generation: 1,
                        source: crate::transfer::SourceLocation {
                            root: local.path().to_path_buf(),
                            path: context.owner.path.clone(),
                        },
                        destination_root: archive.path().to_path_buf(),
                        staged_path: payload.clone(),
                        expected_digest: digest.clone(),
                        expected_size: size,
                    };
                    let history = crate::archive::history_path(&digest)?;
                    match archive.open_relative(&history, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
                        Ok(mut existing) => {
                            if digest_file(&mut existing)? != (digest.clone(), size) {
                                return Err(AppError::integrity(
                                    "existing direct history has unknown bytes; preserved",
                                ));
                            }
                        }
                        Err(e) if e.errno == Some(libc::ENOENT) => {
                            if available_bytes(archive)? < before.len() {
                                return Err(AppError::new(
                                    ErrorCode::Capacity,
                                    "archive lacks space for direct saved history",
                                ));
                            }
                            crate::transfer::TransferEngine::stage_confined(&spec, local, archive)?;
                        }
                        Err(e) => return Err(e),
                    }
                    let mut second = local.open_relative(
                        &context.owner.path,
                        libc::O_RDONLY | libc::O_NONBLOCK,
                        0,
                    )?;
                    if digest_file(&mut second)? != (digest.clone(), size)
                        || SnapshotStamp::of(&before) != SnapshotStamp::of(&source.metadata()?)
                        || SnapshotStamp::of(&before) != SnapshotStamp::of(&second.metadata()?)
                    {
                        return Err(AppError::new(
                            ErrorCode::SourceChanged,
                            "saved sidecar changed during direct capture; owned partial retained",
                        ));
                    }
                    let document = match archive.metadata_relative(&payload) {
                        Ok(_) => &payload,
                        Err(e) if e.errno == Some(libc::ENOENT) => &history,
                        Err(e) => return Err(e),
                    };
                    let invalid = context.owner.kind == SidecarKind::RapidRaw
                        && (size > ceiling
                            || serde_json::from_slice::<serde_json::Value>(
                                &archive.read(document, ceiling)?,
                            )
                            .ok()
                            .is_none_or(|v| !v.is_object()));
                    (digest, size, Some(SnapshotStamp::of(&before)), invalid)
                }
                Err(error) => return Err(error),
            };
            let receipt = SnapshotReceipt {
                path: context.owner.path.clone(),
                body: VersionBody::Content {
                    digest: digest.clone(),
                    size,
                },
                snapshot: Some(crate::archive::history_path(&digest)?),
                stamp: None,
                source_stamp,
                state_device: archive.device(),
                state_inode: archive.inode(),
                opaque_invalid,
                archive_history: true,
            };
            archive.write_new(&prepared, &serde_json::to_vec(&receipt)?)?;
            receipt
        }
        Err(error) => return Err(error),
    };
    if receipt.path != context.owner.path
        || !receipt.archive_history
        || receipt.state_device != archive.device()
        || receipt.state_inode != archive.inode()
    {
        return Err(AppError::integrity(
            "prepared history receipt identity mismatch",
        ));
    }
    let VersionBody::Content { digest, .. } = &receipt.body else {
        return Err(AppError::integrity(
            "direct saved history must contain bytes",
        ));
    };
    let target = crate::archive::history_path(digest)?;
    archive.mkdir_all(&ArchivePath::parse(
        target
            .as_str()
            .rsplit_once('/')
            .ok_or_else(|| AppError::invalid("history parent"))?
            .0,
    )?)?;
    match publish_noreplace(archive, &payload, &target) {
        Ok(_) => {}
        Err(error) if error.errno == Some(libc::ENOENT) => {}
        Err(error) => return Err(error),
    }
    receipt.snapshot = Some(target);
    validate_history_receipt(archive, context, &receipt)?;
    archive.write_new(&entry("receipt.json")?, &serde_json::to_vec(&receipt)?)?;
    cleanup_history_partial(archive, context, &receipt)?;
    local.validate_identity()?;
    Ok(receipt)
}
fn cleanup_history_partial(
    archive: &RootHandle,
    context: &CaptureContext,
    receipt: &SnapshotReceipt,
) -> AppResult<()> {
    let path = crate::transfer::staging_path(JobId(context.operation.0), 1);
    let mut file = match archive.open_relative(&path, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(e) if e.errno == Some(libc::ENOENT) => return Ok(()),
        Err(e) => return Err(e),
    };
    let VersionBody::Content { digest, size } = &receipt.body else {
        return Err(AppError::integrity(
            "history cleanup requires exact content",
        ));
    };
    if digest_file(&mut file)? != (digest.clone(), *size) {
        return Err(AppError::integrity(
            "unexpected direct history partial retained",
        ));
    }
    archive.unlink(&path, false)
}
fn validate_history_receipt(
    archive: &RootHandle,
    context: &CaptureContext,
    receipt: &SnapshotReceipt,
) -> AppResult<()> {
    let VersionBody::Content { digest, size } = &receipt.body else {
        return Err(AppError::integrity("history receipt must contain bytes"));
    };
    if receipt.path != context.owner.path
        || !receipt.archive_history
        || receipt.state_device != archive.device()
        || receipt.state_inode != archive.inode()
        || receipt.snapshot.as_ref() != Some(&crate::archive::history_path(digest)?)
    {
        return Err(AppError::integrity("direct history receipt scope mismatch"));
    }
    let mut file = archive.open_relative(
        &crate::archive::history_path(digest)?,
        libc::O_RDONLY | libc::O_NONBLOCK,
        0,
    )?;
    if digest_file(&mut file)? != (digest.clone(), *size) {
        return Err(AppError::integrity(
            "direct history object differs from its durable receipt",
        ));
    }
    archive.validate_identity()
}
#[derive(Debug, Clone)]
pub struct ArchivedSnapshot {
    body: VersionBody,
}
impl ArchivedSnapshot {
    pub(crate) fn from_receipt(receipt: &SnapshotReceipt) -> AppResult<Self> {
        let VersionBody::Content { digest, .. } = &receipt.body else {
            return Err(AppError::integrity("archived content required"));
        };
        if !receipt.archive_history
            || receipt.snapshot.as_ref() != Some(&crate::archive::history_path(digest)?)
        {
            return Err(AppError::integrity("not an archive history receipt"));
        }
        Ok(Self {
            body: receipt.body.clone(),
        })
    }
    pub(crate) fn body(&self) -> VersionBody {
        self.body.clone()
    }
}
#[cfg(test)]
mod direct_history_tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, RootHandle, RootHandle, CaptureContext) {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("local");
        let archive = temp.path().join("archive");
        std::fs::create_dir(&local).unwrap();
        std::fs::create_dir(&archive).unwrap();
        let local = RootHandle::open(&local, RootPolicy::Local).unwrap();
        let archive = RootHandle::open(&archive, RootPolicy::TestArchive).unwrap();
        let context = CaptureContext {
            operation: OperationId::new(),
            owner: super::super::SidecarOwner {
                id: SidecarId::new(),
                group: None,
                path: ArchivePath::parse("A.ORF.orig").unwrap(),
                kind: SidecarKind::Opaque,
                virtual_copy: None,
            },
            previous: None,
        };
        (temp, local, archive, context)
    }
    #[test]
    fn direct_history_retry_uses_owned_receipt_after_live_file_changes() {
        let (_temp, local, archive, context) = fixture();
        local.write_new(&context.owner.path, b"captured").unwrap();
        let receipt = capture_history(&local, &archive, &context, 1024).unwrap();
        std::fs::write(local.path().join(context.owner.path.as_str()), b"new save").unwrap();
        let recovered = capture_history(&local, &archive, &context, 1024).unwrap();
        assert_eq!(receipt.body, recovered.body);
        assert!(!recovered.still_matches(&local).unwrap());
        assert_eq!(
            archive
                .read(receipt.snapshot.as_ref().unwrap(), 1024)
                .unwrap(),
            b"captured"
        );
    }
    #[test]
    fn interrupted_direct_prefix_remains_opaque_and_detached_without_overwriting_live_save() {
        let (_temp, local, archive, context) = fixture();
        local
            .write_new(&context.owner.path, b"latest complete save")
            .unwrap();
        let directory = ArchivePath::parse(&format!(
            ".photo-sorter/staging/history-capture-{}",
            context.operation
        ))
        .unwrap();
        archive.mkdir_all(&directory).unwrap();
        archive
            .write_new(
                &ArchivePath::parse(&format!("{}/intent.json", directory.as_str())).unwrap(),
                &serde_json::to_vec(&context).unwrap(),
            )
            .unwrap();
        let payload = crate::transfer::staging_path(JobId(context.operation.0), 1);
        archive
            .mkdir_all(&ArchivePath::parse(payload.as_str().rsplit_once('/').unwrap().0).unwrap())
            .unwrap();
        archive.write_new(&payload, b"partial").unwrap();
        let receipt = capture_history(&local, &archive, &context, 1024).unwrap();
        assert!(receipt.opaque_invalid);
        assert!(receipt.source_stamp.is_none());
        assert_eq!(
            receipt.body,
            VersionBody::Content {
                digest: Digest::of(b"partial"),
                size: 7
            }
        );
        assert_eq!(
            local.read(&context.owner.path, 1024).unwrap(),
            b"latest complete save"
        );
        assert_eq!(
            archive
                .read(receipt.snapshot.as_ref().unwrap(), 1024)
                .unwrap(),
            b"partial"
        );
    }
    #[test]
    fn unchanged_direct_native_history_reuses_exact_object_without_bulk_copy() {
        let (_temp, local, archive, mut context) = fixture();
        context.owner.kind = SidecarKind::RapidRaw;
        context.owner.path = ArchivePath::parse("A.ORF.rrdata").unwrap();
        local
            .write_new(
                &context.owner.path,
                br#"{"version":1,"rating":3,"adjustments":null}"#,
            )
            .unwrap();
        let first = capture_history(&local, &archive, &context, 1024).unwrap();
        context.operation = OperationId::new();
        let next = capture_history(&local, &archive, &context, 1024).unwrap();
        assert_eq!(next.body, first.body);
        assert!(!next.opaque_invalid);
        let unused = ArchivePath::parse(&format!(
            ".photo-sorter/staging/{}/1",
            JobId(context.operation.0)
        ))
        .unwrap();
        assert_eq!(
            archive.metadata_relative(&unused).unwrap_err().errno,
            Some(libc::ENOENT)
        );
    }
}

#[cfg(test)]
mod generated_capture_tests {
    use super::*;
    #[test]
    fn generated_capture_keeps_versions_and_refuses_absence_reset() {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("local");
        let state = temp.path().join("state");
        std::fs::create_dir(&local).unwrap();
        std::fs::create_dir(&state).unwrap();
        let local = RootHandle::open(&local, RootPolicy::Local).unwrap();
        let state = RootHandle::open(&state, RootPolicy::Local).unwrap();
        let mut catalog = crate::catalog::Catalog::open(&state).unwrap();
        let path = ArchivePath::parse("render.TIF").unwrap();
        let owner = catalog.register_generated_output(&path).unwrap();
        local.write_new(&path, b"first save").unwrap();
        let first = capture_local(&local, &state, &path, SidecarKind::Generated, 1024).unwrap();
        assert_eq!(
            first.body,
            VersionBody::Content {
                digest: Digest::of(b"first save"),
                size: 10
            }
        );
        let first_snapshot = first.snapshot.clone().unwrap();
        let first_version = super::super::VersionStore::new(&mut catalog, &state)
            .record(super::super::SnapshotIntent {
                sidecar: owner.id,
                parents: vec![],
                operation: OperationId::new(),
                payload: super::super::SnapshotPayload::Stored(
                    StoredSnapshot::from_receipt(first, &state).unwrap(),
                ),
                origin: VersionOrigin::Observed,
            })
            .unwrap();
        std::fs::write(local.path().join(path.as_str()), b"second save").unwrap();
        let second = capture_local(&local, &state, &path, SidecarKind::Generated, 1024).unwrap();
        let second_snapshot = second.snapshot.clone().unwrap();
        let second_version = super::super::VersionStore::new(&mut catalog, &state)
            .record(super::super::SnapshotIntent {
                sidecar: owner.id,
                parents: vec![first_version],
                operation: OperationId::new(),
                payload: super::super::SnapshotPayload::Stored(
                    StoredSnapshot::from_receipt(second, &state).unwrap(),
                ),
                origin: VersionOrigin::Observed,
            })
            .unwrap();
        assert_eq!(state.read(&first_snapshot, 1024).unwrap(), b"first save");
        assert_eq!(state.read(&second_snapshot, 1024).unwrap(), b"second save");
        drop(catalog);
        let mut catalog = crate::catalog::Catalog::open(&state).unwrap();
        assert_eq!(catalog.local_head(owner.id).unwrap(), Some(second_version));
        assert_eq!(
            catalog.version(second_version).unwrap().parents,
            vec![first_version]
        );
        local.unlink(&path, false).unwrap();
        assert!(capture_local(&local, &state, &path, SidecarKind::Generated, 1024).is_err());
        assert!(
            super::super::VersionStore::new(&mut catalog, &state)
                .record(super::super::SnapshotIntent {
                    sidecar: owner.id,
                    parents: vec![],
                    operation: OperationId::new(),
                    payload: super::super::SnapshotPayload::Reset,
                    origin: VersionOrigin::Observed
                })
                .is_err()
        );
    }
}
