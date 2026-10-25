use super::*;
use crate::{
    catalog::Catalog,
    domain::*,
    error::{AppError, AppResult},
    fs::*,
};
use rusqlite::{OptionalExtension, params};
use std::{io::Read, os::unix::fs::MetadataExt};
#[derive(Debug)]
pub enum SnapshotObservation {
    StableContent { version: VersionId, changed: bool },
    StableAbsent { version: VersionId, changed: bool },
    Retry,
    Unreadable { reason: String },
    OpaqueInvalid { version: VersionId, reason: String },
}
pub struct Snapshotter<'a> {
    local: &'a RootHandle,
    state: &'a RootHandle,
    catalog: &'a mut Catalog,
    clock: &'a dyn Clock,
    debounce: u64,
    ceiling: u64,
}
impl<'a> Snapshotter<'a> {
    pub fn new(
        local: &'a RootHandle,
        state: &'a RootHandle,
        catalog: &'a mut Catalog,
        clock: &'a dyn Clock,
        debounce: u64,
        ceiling: u64,
    ) -> Self {
        Self {
            local,
            state,
            catalog,
            clock,
            debounce,
            ceiling,
        }
    }
    pub fn capture(&mut self, owner: &SidecarOwner) -> AppResult<SnapshotObservation> {
        if let Err(e) = self.local.validate_identity() {
            return Ok(SnapshotObservation::Unreadable {
                reason: e.to_string(),
            });
        }
        let previous = self.catalog.local_head(owner.id)?;
        let mut file =
            match self
                .local
                .open_relative(&owner.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)
            {
                Ok(file) => file,
                Err(e) if e.errno == Some(libc::ENOENT) => return self.absence(owner, previous),
                Err(e) => {
                    return Ok(SnapshotObservation::Unreadable {
                        reason: e.to_string(),
                    });
                }
            };
        let before = file.metadata()?;
        if !before.is_file() {
            return Ok(SnapshotObservation::Unreadable {
                reason: "sidecar is not a regular file".into(),
            });
        }
        let payload = if before.len() <= self.ceiling {
            let mut first = Vec::new();
            (&mut file)
                .take(self.ceiling.saturating_add(1))
                .read_to_end(&mut first)?;
            let second = self.local.read(&owner.path, self.ceiling)?;
            let after = file.metadata()?;
            let current = self
                .local
                .open_relative(&owner.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?
                .metadata()?;
            if first != second
                || before.len() != after.len()
                || before.modified()? != after.modified()?
                || before.ctime() != after.ctime()
                || before.ctime_nsec() != after.ctime_nsec()
                || current.ino() != before.ino()
            {
                return Ok(SnapshotObservation::Retry);
            }
            SnapshotPayload::Content(first)
        } else {
            let (digest, size) = crate::transfer::digest_file(&mut file)?;
            let mut second =
                self.local
                    .open_relative(&owner.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
            if crate::transfer::digest_file(&mut second)? != (digest.clone(), size) {
                return Ok(SnapshotObservation::Retry);
            }
            SnapshotPayload::File {
                file: std::sync::Arc::new(file),
                digest,
                size,
            }
        };
        let (body, invalid) = match &payload {
            SnapshotPayload::Stored(stored) => (stored.body(), false),
            SnapshotPayload::Content(bytes) => (
                VersionBody::Content {
                    digest: Digest::of(bytes),
                    size: bytes.len() as u64,
                },
                owner.kind == SidecarKind::RapidRaw
                    && serde_json::from_slice::<serde_json::Value>(bytes)
                        .ok()
                        .is_none_or(|v| !v.is_object()),
            ),
            SnapshotPayload::File { digest, size, .. } => (
                VersionBody::Content {
                    digest: digest.clone(),
                    size: *size,
                },
                owner.kind == SidecarKind::RapidRaw,
            ),
            SnapshotPayload::Archived(stored) => (stored.body(), true),
            SnapshotPayload::Reset => return Err(AppError::integrity("unexpected content reset")),
        };
        self.catalog.db.execute("INSERT INTO observations(path,digest,size,absent_since,readable) VALUES(?,?,?,NULL,1) ON CONFLICT(path) DO UPDATE SET digest=excluded.digest,size=excluded.size,absent_since=NULL,readable=1",params![owner.path.as_str(),match &body{VersionBody::Content{digest,..}=>digest.as_str(),_=>""},before.len()])?;
        if let Some(previous) = previous
            && self.catalog.version(previous)?.body == body
        {
            return Ok(if invalid {
                SnapshotObservation::OpaqueInvalid {
                    version: previous,
                    reason: "preserved opaque or malformed editor JSON".into(),
                }
            } else {
                SnapshotObservation::StableContent {
                    version: previous,
                    changed: false,
                }
            });
        }
        let parents = previous.map(|v| vec![v]).unwrap_or_default();
        let version = VersionStore::new(self.catalog, self.state).record(SnapshotIntent {
            sidecar: owner.id,
            parents,
            operation: OperationId::new(),
            payload,
            origin: VersionOrigin::Observed,
        })?;
        if let Some(group) = owner.group {
            self.catalog.db.execute(
                "UPDATE residency SET protected=1,last_activity=? WHERE group_id=?",
                params![
                    self.clock.now(),
                    self.catalog.canonical_group(group)?.to_string()
                ],
            )?;
        }
        Ok(if invalid {
            SnapshotObservation::OpaqueInvalid {
                version,
                reason: "preserved opaque or malformed editor JSON; transformations blocked".into(),
            }
        } else {
            SnapshotObservation::StableContent {
                version,
                changed: true,
            }
        })
    }
    fn absence(
        &mut self,
        owner: &SidecarOwner,
        previous: Option<VersionId>,
    ) -> AppResult<SnapshotObservation> {
        let Some(previous) = previous else {
            return Ok(SnapshotObservation::Retry);
        };
        if owner.kind == SidecarKind::JpegXmpExternal {
            return Ok(SnapshotObservation::Retry);
        }
        let parent = match self.local.parent(&owner.path) {
            Ok((parent, _)) => parent,
            Err(e) => {
                return Ok(SnapshotObservation::Unreadable {
                    reason: e.to_string(),
                });
            }
        };
        use std::os::fd::AsRawFd;
        let listing = std::fs::read_dir(format!("/proc/self/fd/{}", parent.as_raw_fd()));
        let entries = match listing {
            Ok(entries) => entries,
            Err(e) => {
                return Ok(SnapshotObservation::Unreadable {
                    reason: e.to_string(),
                });
            }
        };
        let name = owner
            .path
            .as_str()
            .rsplit('/')
            .next()
            .ok_or_else(|| AppError::invalid("sidecar filename missing"))?;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    return Ok(SnapshotObservation::Unreadable {
                        reason: e.to_string(),
                    });
                }
            };
            if entry.file_name() == name {
                return Ok(SnapshotObservation::Retry);
            }
        }
        let since: Option<Option<u64>> = self
            .catalog
            .db
            .query_row(
                "SELECT absent_since FROM observations WHERE path=?",
                [owner.path.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        let now = self.clock.now();
        let Some(since) = since.flatten() else {
            self.catalog.db.execute("INSERT INTO observations(path,absent_since) VALUES(?,?) ON CONFLICT(path) DO UPDATE SET absent_since=excluded.absent_since",params![owner.path.as_str(),now])?;
            return Ok(SnapshotObservation::Retry);
        };
        if now.saturating_sub(since) < self.debounce {
            return Ok(SnapshotObservation::Retry);
        }
        if self.catalog.version(previous)?.body == VersionBody::Reset {
            return Ok(SnapshotObservation::StableAbsent {
                version: previous,
                changed: false,
            });
        }
        let version = VersionStore::new(self.catalog, self.state).record(SnapshotIntent {
            sidecar: owner.id,
            parents: vec![previous],
            operation: OperationId::new(),
            payload: SnapshotPayload::Reset,
            origin: VersionOrigin::Observed,
        })?;
        Ok(SnapshotObservation::StableAbsent {
            version,
            changed: true,
        })
    }
}
