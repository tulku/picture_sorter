use super::*;
use crate::{
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
    transfer::digest_file,
};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitArchive {
    pub archive_id: ArchiveId,
    pub writer_id: WriterId,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublicationMode {
    SingleWriter,
    MultiWriter,
    LocalTest,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveMarker {
    pub schema_version: u32,
    pub archive_id: ArchiveId,
    pub writer_id: WriterId,
    pub mode: PublicationMode,
    pub mode_revision: u64,
}
pub struct ArchiveStore {
    pub(crate) root: RootHandle,
    pub(crate) marker: ArchiveMarker,
    pub(crate) writer: WriterId,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedMember {
    pub member: OriginalMember,
    pub staged: ArchivePath,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationPlan {
    pub commit: CommitEnvelope,
    pub staged: Vec<StagedMember>,
    pub attempt: OperationId,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationReceipt {
    pub commit: CommitEnvelope,
    pub groups: Vec<GroupId>,
    pub projection_pending: bool,
}
pub(crate) fn path(value: &str) -> AppResult<ArchivePath> {
    ArchivePath::parse(value)
}
impl ArchiveStore {
    pub fn initialize(root: &RootHandle, request: InitArchive) -> AppResult<ArchiveMarker> {
        root.mkdir_all(&path(".photo-sorter")?)?;
        for dir in ["commits", "versions", "staging", "locks", "sequences"] {
            root.mkdir_all(&path(&format!(".photo-sorter/{dir}"))?)?;
        }
        let marker = ArchiveMarker {
            schema_version: 1,
            archive_id: request.archive_id,
            writer_id: request.writer_id,
            mode: PublicationMode::SingleWriter,
            mode_revision: 1,
        };
        let staged = path(&format!(
            ".photo-sorter/staging/init-{}.json",
            OperationId::new()
        ))?;
        root.write_new(&staged, &serde_json::to_vec(&marker)?)?;
        let target = path(".photo-sorter/archive.json")?;
        match publish_noreplace(root, &staged, &target)? {
            PublishOutcome::Published => Ok(marker),
            PublishOutcome::Exists => {
                let existing: ArchiveMarker =
                    serde_json::from_slice(&root.read(&target, 64 * 1024)?)?;
                if existing.schema_version != 1 || existing.archive_id != marker.archive_id {
                    return Err(AppError::new(
                        ErrorCode::Conflict,
                        "archive already initialized with another identity/schema",
                    ));
                }
                Ok(existing)
            }
        }
    }
    pub fn open(root: RootHandle, expected: ArchiveId, writer: WriterId) -> AppResult<Self> {
        let marker: ArchiveMarker =
            serde_json::from_slice(&root.read(&path(".photo-sorter/archive.json")?, 64 * 1024)?)?;
        if marker.schema_version != 1 || marker.archive_id != expected {
            return Err(AppError::integrity(
                "archive marker identity/schema mismatch",
            ));
        }
        Ok(Self {
            root,
            marker,
            writer,
        })
    }
    pub fn root_path(&self) -> &std::path::Path {
        self.root.path()
    }
    pub fn marker(&self) -> &ArchiveMarker {
        &self.marker
    }
    pub fn validate(&self) -> AppResult<()> {
        self.root.validate_identity()?;
        let current: ArchiveMarker = serde_json::from_slice(
            &self
                .root
                .read(&path(".photo-sorter/archive.json")?, 64 * 1024)?,
        )?;
        if current != self.marker {
            return Err(AppError::new(
                ErrorCode::Unavailable,
                "archive marker changed; reconnect and revalidate capabilities",
            ));
        }
        Ok(())
    }
    pub fn publish_originals(
        &self,
        guard: &mut PublicationGuard,
        plan: PublicationPlan,
        faults: &dyn FaultInjector,
    ) -> AppResult<PublicationReceipt> {
        self.validate_guard(guard)?;
        if !guard.replayed() {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "publication requires complete fresh replay in this lock session",
            ));
        }
        if self.marker.mode == PublicationMode::SingleWriter && self.writer != self.marker.writer_id
        {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "archive is bound to another publisher",
            ));
        }
        let commit = &plan.commit;
        if commit.archive_id != self.marker.archive_id || commit.writer_id != self.writer {
            return Err(AppError::integrity("publication identity mismatch"));
        }
        commit.encode()?;
        for staged in &plan.staged {
            if !staged.staged.as_str().starts_with(".photo-sorter/staging/") {
                return Err(AppError::invalid("original must come from hidden staging"));
            }
            if let Ok(mut existing) =
                self.root
                    .open_relative(&staged.member.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)
            {
                if digest_file(&mut existing)? != (staged.member.digest.clone(), staged.member.size)
                {
                    return Err(AppError::new(
                        ErrorCode::Conflict,
                        "archive destination contains different bytes; preserved",
                    ));
                }
                faults.hit(FaultPoint::OriginalPublished)?;
                continue;
            }
            let mut file = self.root.open_relative(&staged.staged, libc::O_RDONLY, 0)?;
            if digest_file(&mut file)? != (staged.member.digest.clone(), staged.member.size) {
                return Err(AppError::integrity(
                    "staged member differs from immutable intent",
                ));
            }
            if let Some((parent, _)) = staged.member.path.as_str().rsplit_once('/') {
                self.root.mkdir_all(&path(parent)?)?;
            }
            match publish_noreplace(&self.root, &staged.staged, &staged.member.path)? {
                PublishOutcome::Published => {}
                PublishOutcome::Exists => {
                    let mut existing = self.root.open_relative(
                        &staged.member.path,
                        libc::O_RDONLY | libc::O_NONBLOCK,
                        0,
                    )?;
                    if digest_file(&mut existing)?
                        != (staged.member.digest.clone(), staged.member.size)
                    {
                        return Err(AppError::new(
                            ErrorCode::Conflict,
                            "archive destination contains different bytes; preserved",
                        ));
                    }
                }
            }
            faults.hit(FaultPoint::OriginalPublished)?;
        }
        let mut groups = Vec::new();
        for record in commit.records()? {
            match record {
                ArchiveRecord::MemberAttached { group, member, .. } => {
                    let mut original = self.root.open_relative(
                        &member.path,
                        libc::O_RDONLY | libc::O_NONBLOCK,
                        0,
                    )?;
                    if digest_file(&mut original)? != (member.digest, member.size) {
                        return Err(AppError::integrity(
                            "commit references unavailable original",
                        ));
                    }
                    groups.push(group);
                }
                ArchiveRecord::VersionAdded {
                    version:
                        Version {
                            body: VersionBody::Content { digest, size },
                            ..
                        },
                    ..
                } => {
                    let mut history =
                        self.root
                            .open_relative(&history_path(&digest)?, libc::O_RDONLY, 0)?;
                    if digest_file(&mut history)? != (digest, size) {
                        return Err(AppError::integrity("commit references unavailable history"));
                    }
                }
                _ => {}
            }
        }
        let sequence_dir = path(&format!(".photo-sorter/sequences/{}", commit.writer_id))?;
        self.root.mkdir_all(&sequence_dir)?;
        let sequence_path = path(&format!(
            "{}/{}",
            sequence_dir.as_str(),
            commit.writer_sequence
        ))?;
        match self
            .root
            .write_new(&sequence_path, commit.commit_id.to_string().as_bytes())
        {
            Ok(()) => {}
            Err(_) => {
                if self.root.read(&sequence_path, 128)? != commit.commit_id.to_string().as_bytes() {
                    return Err(AppError::integrity(
                        "writer sequence reused by another commit",
                    ));
                }
            }
        }
        let dir = path(&format!(".photo-sorter/commits/{}", commit.writer_id))?;
        self.root.mkdir_all(&dir)?;
        let target = path(&format!(
            "{}/{}-{}.json",
            dir.as_str(),
            commit.writer_sequence,
            commit.commit_id
        ))?;
        let staged = path(&format!(
            ".photo-sorter/staging/commit-{}.json",
            plan.attempt
        ))?;
        let bytes = commit.encode()?;
        match self.root.write_new(&staged, &bytes) {
            Ok(()) => {}
            Err(_) => {
                if self.root.read(&staged, MAX_COMMIT_BYTES as u64)? != bytes {
                    return Err(AppError::integrity("commit staging identity differs"));
                }
            }
        }
        match publish_noreplace(&self.root, &staged, &target)? {
            PublishOutcome::Published => {}
            PublishOutcome::Exists => {
                if self.root.read(&target, MAX_COMMIT_BYTES as u64)? != bytes {
                    return Err(AppError::integrity("commit ID collision"));
                }
            }
        }
        faults.hit(FaultPoint::CommitPublished)?;
        self.validate_guard(guard)?;
        Ok(PublicationReceipt {
            commit: plan.commit,
            groups,
            projection_pending: false,
        })
    }
}
pub fn history_path(digest: &Digest) -> AppResult<ArchivePath> {
    let hex = &digest.as_str()[7..];
    path(&format!(".photo-sorter/versions/{}/{}", &hex[..2], hex))
}
impl ArchiveStore {
    pub fn store_history(&self, staged: &ArchivePath, digest: &Digest, size: u64) -> AppResult<()> {
        self.validate()?;
        if !staged.as_str().starts_with(".photo-sorter/staging/") {
            return Err(AppError::invalid("history must come from hidden staging"));
        }
        let target = history_path(digest)?;
        let mut source = self
            .root
            .open_relative(staged, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        if digest_file(&mut source)? != (digest.clone(), size) {
            return Err(AppError::integrity("history snapshot digest mismatch"));
        }
        let parent = target
            .as_str()
            .rsplit_once('/')
            .ok_or_else(|| AppError::invalid("history parent missing"))?
            .0;
        self.root.mkdir_all(&path(parent)?)?;
        if publish_noreplace(&self.root, staged, &target)? == PublishOutcome::Exists {
            let mut existing = self.root.open_relative(&target, libc::O_RDONLY, 0)?;
            if digest_file(&mut existing)? != (digest.clone(), size) {
                return Err(AppError::integrity("corrupt immutable history exists"));
            }
        }
        self.validate()?;
        Ok(())
    }
}
