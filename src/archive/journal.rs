use super::*;
use crate::{
    domain::*,
    error::{AppError, AppResult},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, ReadDir},
    os::fd::AsRawFd,
};
struct Cursor {
    _dir: File,
    entries: ReadDir,
    prefix: String,
}
pub(crate) struct JournalCursor {
    stack: Vec<Cursor>,
}
impl JournalCursor {
    pub fn open(root: &crate::fs::RootHandle) -> AppResult<Self> {
        let dir = root.open_relative(
            &path(".photo-sorter/commits")?,
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?;
        let entries = std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))?;
        Ok(Self {
            stack: vec![Cursor {
                _dir: dir,
                entries,
                prefix: ".photo-sorter/commits".into(),
            }],
        })
    }
}
impl JournalCursor {
    fn next(
        &mut self,
        root: &crate::fs::RootHandle,
        limit: usize,
    ) -> AppResult<(Vec<CommitDescriptor>, bool)> {
        if limit == 0 || limit > 128 {
            return Err(AppError::invalid("journal page must be 1..128"));
        }
        let mut end = false;
        let mut entries = Vec::new();
        while entries.len() < limit {
            let Some(cursor) = self.stack.last_mut() else {
                end = true;
                break;
            };
            let prefix = cursor.prefix.clone();
            let Some(entry) = cursor.entries.next() else {
                self.stack.pop();
                continue;
            };
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| AppError::integrity("non-UTF-8 journal entry"))?;
            let relative = path(&format!("{prefix}/{name}"))?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if prefix != ".photo-sorter/commits" || uuid::Uuid::parse_str(name).is_err() {
                    return Err(AppError::integrity("unexpected journal directory"));
                }
                let dir = root.open_relative(&relative, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
                let entries = std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))?;
                self.stack.push(Cursor {
                    _dir: dir,
                    entries,
                    prefix: relative.as_str().into(),
                });
                continue;
            }
            if !kind.is_file() {
                return Err(AppError::integrity("journal symlink or special file"));
            }
            let writer: WriterId = serde_json::from_str(&format!(
                "\"{}\"",
                prefix
                    .rsplit('/')
                    .next()
                    .ok_or_else(|| AppError::integrity("missing writer"))?
            ))?;
            let (sequence, id) = name
                .strip_suffix(".json")
                .and_then(|s| s.split_once('-'))
                .ok_or_else(|| AppError::integrity("invalid commit filename"))?;
            let id: CommitId = serde_json::from_str(&format!("\"{id}\""))?;
            let sequence = sequence
                .parse()
                .map_err(|_| AppError::integrity("invalid commit sequence"))?;
            entries.push(CommitDescriptor {
                id,
                writer,
                sequence,
                path: relative,
            });
        }
        Ok((entries, end))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitDescriptor {
    pub id: CommitId,
    pub writer: WriterId,
    pub sequence: u64,
    pub path: ArchivePath,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalBatch {
    pub token: OperationId,
    pub entries: Vec<CommitDescriptor>,
    pub end: bool,
}
impl ArchiveStore {
    pub fn journal_batch(
        &self,
        guard: &mut PublicationGuard,
        limit: usize,
    ) -> AppResult<JournalBatch> {
        self.validate_guard(guard)?;
        if guard.pending.is_some() || limit == 0 || limit > 128 {
            return Err(AppError::invalid(
                "acknowledge previous batch; limit must be 1..128",
            ));
        }
        let (entries, end) = guard.cursor.next(&self.root, limit)?;
        guard.end = end;
        let token = OperationId::new();
        guard.pending = Some(token);
        Ok(JournalBatch {
            token,
            entries,
            end: guard.end,
        })
    }
    pub fn ack_journal(&self, guard: &mut PublicationGuard, token: OperationId) -> AppResult<()> {
        self.validate_guard(guard)?;
        if guard.pending != Some(token) {
            return Err(AppError::integrity("journal batch token mismatch"));
        }
        guard.pending = None;
        guard.replayed = guard.end;
        Ok(())
    }
    pub fn read_commit(
        &self,
        guard: &PublicationGuard,
        relative: &ArchivePath,
    ) -> AppResult<CommitEnvelope> {
        self.validate_guard(guard)?;
        self.read_commit_readonly(relative)
    }
    pub fn read_commit_readonly(&self, relative: &ArchivePath) -> AppResult<CommitEnvelope> {
        self.validate()?;
        if !relative.as_str().starts_with(".photo-sorter/commits/") {
            return Err(AppError::invalid("not a journal path"));
        }
        let commit = CommitEnvelope::decode(
            &self.root.read(relative, MAX_COMMIT_BYTES as u64)?,
            self.marker.archive_id,
        )?;
        let expected = format!(
            ".photo-sorter/commits/{}/{}-{}.json",
            commit.writer_id, commit.writer_sequence, commit.commit_id
        );
        if relative.as_str() != expected {
            return Err(AppError::integrity(
                "commit filename disagrees with envelope",
            ));
        }
        for record in commit.records()? {
            match record {
                ArchiveRecord::VersionAdded {
                    version:
                        Version {
                            body: VersionBody::Content { digest, size },
                            ..
                        },
                    ..
                } => {
                    let mut file = self
                        .root
                        .open_relative(&history_path(&digest)?, libc::O_RDONLY, 0)
                        .map_err(|_| {
                            AppError::integrity("journal references missing history payload")
                        })?;
                    if crate::transfer::digest_file(&mut file)? != (digest, size) {
                        return Err(AppError::integrity(
                            "journal references corrupt history payload",
                        ));
                    }
                }
                ArchiveRecord::MemberAttached { member, .. } => {
                    member.validate()?;
                    let mut file = self
                        .root
                        .open_relative(&member.path, libc::O_RDONLY, 0)
                        .map_err(|_| AppError::integrity("journal references missing original"))?;
                    if crate::transfer::digest_file(&mut file)? != (member.digest, member.size) {
                        return Err(AppError::integrity("journal references corrupt original"));
                    }
                }
                ArchiveRecord::SidecarCreated { path, .. } => validate_sidecar_path(&path)?,
                _ => {}
            }
        }
        Ok(commit)
    }
}

pub struct ReadOnlyJournal {
    pub id: OperationId,
    store: ArchiveStore,
    cursor: JournalCursor,
}
impl ArchiveStore {
    pub fn read_only_journal(&self) -> AppResult<ReadOnlyJournal> {
        let store = Self::open(self.root.try_clone()?, self.marker.archive_id, self.writer)?;
        let cursor = JournalCursor::open(&store.root)?;
        Ok(ReadOnlyJournal {
            id: OperationId::new(),
            store,
            cursor,
        })
    }
}
impl ReadOnlyJournal {
    pub fn next_batch(&mut self, limit: usize) -> AppResult<JournalBatch> {
        self.store.validate()?;
        let (entries, end) = self.cursor.next(&self.store.root, limit)?;
        Ok(JournalBatch {
            token: OperationId::new(),
            entries,
            end,
        })
    }
    pub fn read(&self, path: &ArchivePath) -> AppResult<CommitEnvelope> {
        self.store.read_commit_readonly(path)
    }
}
