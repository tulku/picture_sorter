use crate::{
    archive::ArchiveStore,
    catalog::{Catalog, OwnershipLock},
    config::Config,
    domain::*,
    error::{AppError, AppResult},
    fs::{RootHandle, RootPolicy},
    import::{JPEG_FINGERPRINT_VERSION, inspect_jpeg, jpeg_file_stamp},
};
use rusqlite::params;
use serde::Serialize;
use std::{os::unix::fs::MetadataExt, path::Path};

#[derive(Debug, Default, Serialize)]
pub struct JpegIndexReport {
    pub selected: u64,
    pub indexed: u64,
    pub bytes: u64,
    pub path: Option<ArchivePath>,
}

fn with_offline_catalog<T>(
    config: &Config,
    action: impl FnOnce(Catalog) -> AppResult<T>,
) -> AppResult<T> {
    let state = RootHandle::open(&config.state_root, RootPolicy::Local)?;
    let _profile = OwnershipLock::acquire(&state, "profile.lock")?;
    let local = RootHandle::open(&config.working_root, RootPolicy::Local)?;
    let _work = OwnershipLock::acquire(&local, "working.lock")?;
    let catalog = Catalog::open(&state)?;
    action(catalog)
}

pub fn offline_adopt_source(config: &Config, job: JobId, source: &Path) -> AppResult<u64> {
    with_offline_catalog(config, |mut catalog| {
        catalog.adopt_import_source_history(job, source)
    })
}

pub fn offline_index_jpegs(
    config: &Config,
    scan: Option<JobId>,
    mut progress: impl FnMut(&JpegIndexReport),
) -> AppResult<JpegIndexReport> {
    with_offline_catalog(config, |mut catalog| {
        let archive = config
            .archive_id
            .ok_or_else(|| AppError::invalid("archive identity required"))?;
        if let Some(scan) = scan {
            let record = catalog.job(scan)?;
            if !matches!(record.request.kind, crate::catalog::JobKind::Import { .. }) {
                return Err(AppError::invalid("JPEG index scope must be an import scan"));
            }
        }
        let policy = if config.local_test_backend {
            RootPolicy::TestArchive
        } else {
            RootPolicy::Archive {
                expected_device: std::fs::metadata(&config.archive_root)?.dev(),
            }
        };
        let root = RootHandle::open(&config.archive_root, policy)?;
        let store = ArchiveStore::open(root, archive, config.writer_id)?;
        let mut query = catalog.db.prepare("SELECT DISTINCT m.path,m.digest,b.size FROM members m JOIN placements p ON p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest JOIN blobs b ON b.digest=m.digest WHERE m.archive=?1 AND m.role=?2 AND p.archived=1 AND NOT EXISTS(SELECT 1 FROM jpeg_fingerprints f WHERE f.digest=m.digest AND f.algorithm=?3) AND (?4 IS NULL OR EXISTS(SELECT 1 FROM jpeg_checks c JOIN import_items i ON i.id=c.item WHERE i.parent=?4 AND json_extract(c.payload,'$.candidate.path')=m.path AND json_extract(c.payload,'$.candidate.digest')=m.digest)) ORDER BY m.path")?;
        let rows = query
            .query_map(
                params![
                    archive.to_string(),
                    serde_json::to_string(&MediaRole::Jpeg)?,
                    JPEG_FINGERPRINT_VERSION,
                    scan.map(|id| id.to_string())
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u64>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(query);
        let mut report = JpegIndexReport {
            selected: rows.len() as u64,
            ..Default::default()
        };
        for (path, expected, size) in rows {
            let path = ArchivePath::parse(&path)?;
            report.path = Some(path.clone());
            progress(&report);
            let mut file = store
                .root
                .open_relative(&path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
            let before = file.metadata()?;
            if !before.is_file() || before.len() != size {
                return Err(AppError::integrity(
                    "indexed JPEG size or file type changed",
                ));
            }
            let inspection = inspect_jpeg(&mut file)?;
            let current = store
                .root
                .open_relative(&path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
            if inspection.digest.as_str() != expected
                || inspection.size != size
                || jpeg_file_stamp(&before) != jpeg_file_stamp(&file.metadata()?)
                || jpeg_file_stamp(&before) != jpeg_file_stamp(&current.metadata()?)
            {
                return Err(AppError::integrity(
                    "indexed JPEG differs from catalogued bytes",
                ));
            }
            store.validate()?;
            catalog.cache_jpeg_inspection(&inspection)?;
            report.indexed += 1;
            report.bytes += size;
        }
        report.path = None;
        progress(&report);
        Ok(report)
    })
}

#[derive(Debug, Default, Serialize)]
pub struct SourcePreview {
    pub selected: u64,
    pub cached: u64,
    pub uncached: u64,
    pub archive_present: u64,
    pub image_duplicates: u64,
    pub new_files: u64,
    pub new_bytes: u64,
    pub ambiguous: u64,
    pub hash_bytes: u64,
    pub transfer_bytes: u64,
}

pub fn offline_preview_source(
    config: &Config,
    job: JobId,
    source: &Path,
) -> AppResult<SourcePreview> {
    with_offline_catalog(config, |mut catalog| {
        catalog.import_source_identity(job)?;
        let identity = catalog.ensure_import_source(job, source)?;
        let before = crate::metrics::snapshot();
        let engine = crate::import::ImportCoordinator::new(config.clone(), catalog);
        let mut inventory =
            crate::import::Inventory::open(RootHandle::open(source, RootPolicy::Local)?)?;
        let mut report = SourcePreview::default();
        loop {
            let entries = inventory.next_batch(128)?;
            if entries.is_empty() {
                break;
            }
            for entry in entries {
                report.selected += 1;
                let Some(digest) = engine
                    .catalog
                    .cached_source_digest(&entry, &identity, false)?
                else {
                    report.uncached += 1;
                    continue;
                };
                report.cached += 1;
                let crate::import::SourceRole::Original(_) = entry.role else {
                    continue;
                };
                match engine.canonical_archived_original(&entry, &digest, false)? {
                    crate::import::CanonicalSelection::Exact(_) => report.archive_present += 1,
                    crate::import::CanonicalSelection::Image(_, _) => report.image_duplicates += 1,
                    crate::import::CanonicalSelection::Ambiguous => report.ambiguous += 1,
                    crate::import::CanonicalSelection::None => {
                        report.new_files += 1;
                        report.new_bytes += entry.size;
                    }
                }
            }
        }
        let work = crate::metrics::snapshot().difference(&before);
        report.hash_bytes = work.streamed_hash_bytes;
        report.transfer_bytes = work.transfer_bytes;
        Ok(report)
    })
}
