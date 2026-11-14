use super::*;
use crate::{
    catalog::{Catalog, JobKind},
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::{RootHandle, RootPolicy},
};
use rusqlite::{OptionalExtension, params};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

fn mount_path(value: &str) -> PathBuf {
    PathBuf::from(
        value
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\"),
    )
}
fn filesystem_source_identity(
    uuid: Option<&str>,
    cid: &str,
    location: &Path,
    directory: &str,
) -> (String, bool) {
    match uuid {
        Some(uuid) => {
            let mut incarnation = format!("filesystem:{uuid}:{cid}:{}", location.display());
            if !directory.is_empty() {
                incarnation.push(':');
                incarnation.push_str(directory);
            }
            (incarnation, true)
        }
        None => (format!("mmc:{cid}:{}", location.display()), false),
    }
}
fn filesystem_directory_identity(
    uuid: &str,
    cid: &str,
    location: &Path,
    inode: u64,
    birth: Option<std::time::SystemTime>,
) -> (String, bool) {
    let (incarnation, _) =
        filesystem_source_identity(Some(uuid), cid, location, &format!("{inode}:{birth:?}"));
    (incarnation, birth.is_some())
}
fn local_source_identity(
    machine: Option<&str>,
    boot: Option<&str>,
    device: u64,
    inode: u64,
    birth: Option<std::time::SystemTime>,
    path: &Path,
) -> (String, bool) {
    let scope = machine
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|machine| format!("local:{machine}"))
        .or_else(|| {
            boot.map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|boot| format!("local-boot:{boot}"))
        });
    match (birth, scope) {
        (Some(birth), Some(scope)) => (format!("{scope}:{device}:{inode}:{birth:?}"), true),
        _ => (format!("unidentified:{}", path.display()), false),
    }
}
fn source_identity(source: &Path) -> AppResult<SourceIdentity> {
    let root = RootHandle::open(source, RootPolicy::Local)?;
    let path = std::fs::canonicalize(source)?;
    let metadata = std::fs::metadata(format!("/proc/self/fd/{}", root.fd()))?;
    let device = metadata.dev();
    let device_id = format!("{}:{}", libc::major(device), libc::minor(device));
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
    let mount = mountinfo
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.get(2)? != &device_id {
                return None;
            }
            let point = mount_path(fields.get(4)?);
            if !path.starts_with(&point) {
                return None;
            }
            let separator = fields.iter().position(|field| *field == "-")?;
            Some((
                point,
                mount_path(fields.get(3)?),
                fields.get(separator + 1)?.to_string(),
            ))
        })
        .max_by_key(|(point, _, _)| point.as_os_str().len());
    let uuid = std::fs::read_dir("/dev/disk/by-uuid")
        .ok()
        .and_then(|entries| {
            entries.filter_map(Result::ok).find_map(|entry| {
                let stat = entry.path().metadata().ok()?;
                (stat.rdev() == device).then(|| entry.file_name().to_string_lossy().into_owned())
            })
        });
    let block = std::fs::canonicalize(format!("/sys/dev/block/{device_id}")).ok();
    let cid = block.as_ref().and_then(|block| {
        block.ancestors().find_map(|parent| {
            std::fs::read_to_string(parent.join("device/cid"))
                .ok()
                .map(|s| s.trim().to_owned())
        })
    });
    let removable = block.as_ref().is_some_and(|block| {
        block.ancestors().any(|parent| {
            std::fs::read_to_string(parent.join("removable")).is_ok_and(|s| s.trim() == "1")
        })
    });
    let (incarnation, reliable) =
        if let (Some(uuid), Some((point, mount_root, kind))) = (uuid, mount.as_ref()) {
            let location = mount_root.join(
                path.strip_prefix(point)
                    .map_err(|_| AppError::integrity("source left its mount"))?,
            );
            if matches!(kind.as_str(), "vfat" | "exfat" | "msdos") {
                filesystem_source_identity(
                    Some(&uuid),
                    cid.as_deref().unwrap_or_default(),
                    &location,
                    "",
                )
            } else {
                filesystem_directory_identity(
                    &uuid,
                    cid.as_deref().unwrap_or_default(),
                    &location,
                    metadata.ino(),
                    metadata.created().ok(),
                )
            }
        } else if let Some(cid) = cid {
            let location = mount
                .as_ref()
                .map(|(point, mount_root, _)| mount_root.join(path.strip_prefix(point).unwrap()))
                .unwrap_or(path.clone());
            filesystem_source_identity(None, &cid, &location, "")
        } else if !removable
            && !mount.as_ref().is_some_and(|(_, _, kind)| {
                matches!(kind.as_str(), "vfat" | "exfat" | "msdos" | "fuseblk")
            })
        {
            local_source_identity(
                std::fs::read_to_string("/etc/machine-id").ok().as_deref(),
                std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
                    .ok()
                    .as_deref(),
                device,
                metadata.ino(),
                metadata.created().ok(),
                &path,
            )
        } else {
            (format!("unidentified:{}", path.display()), false)
        };
    root.validate_identity()?;
    Ok(SourceIdentity {
        incarnation,
        reliable,
    })
}
impl Catalog {
    pub(crate) fn ensure_import_source(
        &mut self,
        job: JobId,
        source: &Path,
    ) -> AppResult<SourceIdentity> {
        let identity = source_identity(source)?;
        let existing: Option<(String, bool, String)> = self
            .db
            .query_row(
                "SELECT incarnation,reliable,source_root FROM import_source_bindings WHERE job=?",
                [job.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let canonical = std::fs::canonicalize(source)?;
        let root = canonical
            .to_str()
            .ok_or_else(|| AppError::invalid("UTF-8 source required"))?;
        if let Some((incarnation, reliable, source_root)) = existing {
            if incarnation != identity.incarnation
                || reliable != identity.reliable
                || source_root != root
            {
                return Err(AppError::new(
                    ErrorCode::SourceChanged,
                    "import source identity changed",
                ));
            }
        } else {
            self.db.execute("INSERT INTO import_source_bindings(job,incarnation,reliable,source_root) VALUES(?,?,?,?)",params![job.to_string(),identity.incarnation,identity.reliable,root])?;
        }
        Ok(identity)
    }
    pub(crate) fn import_source_identity(&self, job: JobId) -> AppResult<SourceIdentity> {
        self.db
            .query_row(
                "SELECT incarnation,reliable FROM import_source_bindings WHERE job=?",
                [job.to_string()],
                |r| {
                    Ok(SourceIdentity {
                        incarnation: r.get(0)?,
                        reliable: r.get(1)?,
                    })
                },
            )
            .map_err(Into::into)
    }
    pub(crate) fn cached_source_digest(
        &self,
        entry: &SourceEntry,
        identity: &SourceIdentity,
        strict: bool,
    ) -> AppResult<Option<Digest>> {
        entry.validate()?;
        if strict || !identity.reliable || !matches!(entry.role, SourceRole::Original(_)) {
            return Ok(None);
        }
        let digest: Option<String> = self
            .db
            .query_row(
                "SELECT digest FROM sources WHERE incarnation=? AND path=? AND mtime=? AND size=?",
                params![
                    identity.incarnation,
                    entry.path.as_str(),
                    entry.fingerprint,
                    entry.size
                ],
                |r| r.get(0),
            )
            .optional()?;
        digest.map(|digest| Digest::parse(&digest)).transpose()
    }
    pub fn adopt_import_source_history(&mut self, job: JobId, source: &Path) -> AppResult<u64> {
        let record = self.job(job)?;
        let JobKind::Import {
            source: old_root, ..
        } = record.request.kind
        else {
            return Err(AppError::invalid("history must be an import scan"));
        };
        if std::fs::canonicalize(&old_root)? != std::fs::canonicalize(source)? {
            return Err(AppError::invalid(
                "history source root differs from current source",
            ));
        }
        let complete: bool = self
            .db
            .query_row(
                "SELECT complete FROM import_scans WHERE job=?",
                [job.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if !complete {
            return Err(AppError::invalid("history scan is incomplete"));
        }
        let identity = source_identity(source)?;
        if !identity.reliable {
            return Err(AppError::invalid(
                "current source cannot be identified reliably",
            ));
        }
        let old_identity: Option<String> = self
            .db
            .query_row(
                "SELECT incarnation FROM import_source_bindings WHERE job=?",
                [job.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if old_identity
            .as_ref()
            .is_some_and(|old| *old != identity.incarnation)
        {
            return Err(AppError::invalid(
                "history is already bound to a different source",
            ));
        }
        let old_incarnation =
            old_identity.unwrap_or_else(|| record.request.operation_id.to_string());
        let rows = self
            .db
            .prepare("SELECT path,size,mtime,digest FROM sources WHERE incarnation=?")?
            .query_map([&old_incarnation], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Err(AppError::invalid("history has no source fingerprints"));
        }
        let mut inventory = Inventory::open(RootHandle::open(source, RootPolicy::Local)?)?;
        let mut current = std::collections::HashMap::new();
        loop {
            let entries = inventory.next_batch(128)?;
            if entries.is_empty() {
                break;
            }
            for entry in entries {
                current.insert(entry.path.as_str().to_owned(), entry);
            }
        }
        for (path, size, mtime, _) in &rows {
            let entry = current
                .get(path)
                .ok_or_else(|| AppError::invalid("historical source file is missing"))?;
            entry.validate()?;
            if entry.size != *size || entry.fingerprint != *mtime {
                return Err(AppError::invalid("historical source file metadata changed"));
            }
        }
        if source_identity(source)?.incarnation != identity.incarnation {
            return Err(AppError::new(
                ErrorCode::SourceChanged,
                "source changed during adoption",
            ));
        }
        for entry in current.values() {
            entry.validate()?;
        }
        let tx = self.db.transaction()?;
        for (path, size, mtime, digest) in &rows {
            let conflict: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sources WHERE incarnation=? AND path=? AND (size<>? OR mtime<>? OR digest<>?))",params![identity.incarnation,path,size,mtime,digest],|r|r.get(0))?;
            if conflict {
                return Err(AppError::invalid(
                    "current source cache conflicts with history",
                ));
            }
            tx.execute("INSERT INTO sources(incarnation,path,size,mtime,digest) VALUES(?,?,?,?,?) ON CONFLICT DO NOTHING",params![identity.incarnation,path,size,mtime,digest])?;
        }
        tx.execute("INSERT INTO import_source_bindings(job,incarnation,reliable,source_root) VALUES(?,?,1,?) ON CONFLICT DO NOTHING",params![job.to_string(),identity.incarnation,std::fs::canonicalize(source)?.to_string_lossy()])?;
        tx.commit()?;
        Ok(rows.len() as u64)
    }
}
impl ImportCoordinator {
    pub(crate) fn accept_source_batch(
        &mut self,
        job: JobId,
        entries: &[SourceEntry],
        metadata: Option<Vec<MetadataResult>>,
    ) -> AppResult<ImportReport> {
        let identity = if let Some(entry) = entries.first() {
            self.catalog.ensure_import_source(job, entry.root.path())?
        } else {
            self.catalog.import_source_identity(job)?
        };
        let strict = matches!(
            self.catalog.job(job)?.request.kind,
            JobKind::Import { strict: true, .. }
        );
        let mut digests = Vec::new();
        let mut cached_bytes = 0;
        for entry in entries {
            if let Some(digest) = self
                .catalog
                .cached_source_digest(entry, &identity, strict)?
            {
                cached_bytes += entry.size;
                digests.push(digest);
            } else if matches!(entry.role, SourceRole::Original(MediaRole::Jpeg)) {
                let inspection = inspect_jpeg_source(entry)?;
                digests.push(inspection.digest.clone());
                self.catalog.cache_jpeg_inspection(&inspection)?;
            } else {
                digests.push(hash_source(entry)?);
            }
        }
        self.accept_cached_hashed_batch(job, entries, &digests, metadata, cached_bytes)
    }
    pub(crate) fn accept_cached_hashed_batch(
        &mut self,
        job: JobId,
        entries: &[SourceEntry],
        digests: &[Digest],
        metadata: Option<Vec<MetadataResult>>,
        cached_bytes: u64,
    ) -> AppResult<ImportReport> {
        self.accept_hashed_batch(job, entries, digests, metadata)?;
        self.catalog.db.execute(
            "UPDATE import_stats SET hash_bytes=hash_bytes-? WHERE job=?",
            params![cached_bytes, job.to_string()],
        )?;
        self.report(job)
    }
}

#[cfg(test)]
mod generation_tests {
    use super::*;
    fn identity(generation: Option<&str>) -> SourceIdentity {
        let (incarnation, reliable) =
            filesystem_source_identity(generation, "physical-card-cid", Path::new("/"), "");
        SourceIdentity {
            incarnation,
            reliable,
        }
    }
    #[test]
    fn card_without_filesystem_generation_never_reuses_cached_digest() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let state = temp.path().join("state");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&state).unwrap();
        std::fs::write(source.join("a.ORF"), b"original").unwrap();
        let root = RootHandle::open(&state, RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let mut inventory =
            Inventory::open(RootHandle::open(&source, RootPolicy::Local).unwrap()).unwrap();
        let entry = inventory.next_batch(128).unwrap().remove(0);
        let first = recognize(&entry, &identity(None), &catalog, false).unwrap();
        catalog
            .remember_source(&entry, &identity(None), &first.digest)
            .unwrap();
        let next = recognize(&entry, &identity(None), &catalog, false).unwrap();
        assert!(!next.shortcut);
        assert_eq!(next.hashed_bytes, 8);
    }
    #[test]
    fn card_reformat_generation_invalidates_unchanged_metadata_cache() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let state = temp.path().join("state");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&state).unwrap();
        std::fs::write(source.join("a.ORF"), b"original").unwrap();
        let root = RootHandle::open(&state, RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let mut inventory =
            Inventory::open(RootHandle::open(&source, RootPolicy::Local).unwrap()).unwrap();
        let entry = inventory.next_batch(128).unwrap().remove(0);
        let first = recognize(&entry, &identity(Some("before-format")), &catalog, false).unwrap();
        catalog
            .remember_source(&entry, &identity(Some("before-format")), &first.digest)
            .unwrap();
        assert_eq!(
            recognize(&entry, &identity(Some("before-format")), &catalog, false)
                .unwrap()
                .hashed_bytes,
            0
        );
        let reformatted =
            recognize(&entry, &identity(Some("after-format")), &catalog, false).unwrap();
        assert!(!reformatted.shortcut);
        assert_eq!(reformatted.hashed_bytes, 8);
    }
}

#[cfg(test)]
mod identity_compatibility_tests {
    use super::*;
    #[test]
    fn existing_fat_source_fingerprint_remains_reusable_without_directory_discriminator() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let state = temp.path().join("state");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&state).unwrap();
        std::fs::write(source.join("a.ORF"), b"original").unwrap();
        let root = RootHandle::open(&state, RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let mut inventory =
            Inventory::open(RootHandle::open(&source, RootPolicy::Local).unwrap()).unwrap();
        let entry = inventory.next_batch(128).unwrap().remove(0);
        let historical = SourceIdentity {
            incarnation: "filesystem:card-uuid:card-cid:/".into(),
            reliable: true,
        };
        catalog
            .remember_source(&entry, &historical, &Digest::of(b"original"))
            .unwrap();
        let (incarnation, reliable) =
            filesystem_source_identity(Some("card-uuid"), "card-cid", Path::new("/"), "");
        let current = SourceIdentity {
            incarnation,
            reliable,
        };
        let recognition = recognize(&entry, &current, &catalog, false).unwrap();
        assert!(recognition.shortcut);
        assert_eq!(recognition.hashed_bytes, 0);
        assert_eq!(recognition.digest, Digest::of(b"original"));
    }
}

#[cfg(test)]
mod local_scope_tests {
    use super::*;
    #[test]
    fn boot_scoped_local_cache_reuses_only_same_boot_directory_and_birth() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let state = temp.path().join("state");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&state).unwrap();
        std::fs::write(source.join("a.ORF"), b"original").unwrap();
        let root = RootHandle::open(&state, RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let mut inventory =
            Inventory::open(RootHandle::open(&source, RootPolicy::Local).unwrap()).unwrap();
        let entry = inventory.next_batch(128).unwrap().remove(0);
        let birth = std::time::UNIX_EPOCH + std::time::Duration::from_secs(123);
        let identify = |boot, device, inode, birth| {
            let (incarnation, reliable) =
                local_source_identity(None, boot, device, inode, birth, &source);
            SourceIdentity {
                incarnation,
                reliable,
            }
        };
        let original = identify(Some("boot-one"), 7, 11, Some(birth));
        catalog
            .remember_source(&entry, &original, &Digest::of(b"original"))
            .unwrap();
        assert_eq!(
            recognize(
                &entry,
                &identify(Some("boot-one"), 7, 11, Some(birth)),
                &catalog,
                false
            )
            .unwrap()
            .hashed_bytes,
            0
        );
        for identity in [
            identify(Some("boot-two"), 7, 11, Some(birth)),
            identify(Some("boot-one"), 8, 11, Some(birth)),
            identify(Some("boot-one"), 7, 12, Some(birth)),
            identify(
                Some("boot-one"),
                7,
                11,
                Some(birth + std::time::Duration::from_secs(1)),
            ),
            identify(Some("boot-one"), 7, 11, None),
            identify(None, 7, 11, Some(birth)),
        ] {
            let recognition = recognize(&entry, &identity, &catalog, false).unwrap();
            assert!(!recognition.shortcut);
            assert_eq!(recognition.hashed_bytes, 8);
        }
    }
}

#[cfg(test)]
mod directory_generation_tests {
    use super::*;
    #[test]
    fn non_fat_directory_requires_birth_even_with_filesystem_uuid() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let state = temp.path().join("state");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&state).unwrap();
        std::fs::write(source.join("a.ORF"), b"original").unwrap();
        let root = RootHandle::open(&state, RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let mut inventory =
            Inventory::open(RootHandle::open(&source, RootPolicy::Local).unwrap()).unwrap();
        let entry = inventory.next_batch(128).unwrap().remove(0);
        let identify = |birth| {
            let (incarnation, reliable) = filesystem_directory_identity(
                "filesystem-uuid",
                "",
                Path::new("/photos"),
                11,
                birth,
            );
            SourceIdentity {
                incarnation,
                reliable,
            }
        };
        let missing_birth = identify(None);
        catalog
            .remember_source(&entry, &missing_birth, &Digest::of(b"original"))
            .unwrap();
        assert_eq!(
            recognize(&entry, &missing_birth, &catalog, false)
                .unwrap()
                .hashed_bytes,
            8
        );
        let birth = std::time::UNIX_EPOCH + std::time::Duration::from_secs(123);
        let identified = identify(Some(birth));
        catalog
            .remember_source(&entry, &identified, &Digest::of(b"original"))
            .unwrap();
        assert_eq!(
            recognize(&entry, &identified, &catalog, false)
                .unwrap()
                .hashed_bytes,
            0
        );
        assert_eq!(
            recognize(
                &entry,
                &identify(Some(birth + std::time::Duration::from_secs(1))),
                &catalog,
                false
            )
            .unwrap()
            .hashed_bytes,
            8
        );
    }
}
