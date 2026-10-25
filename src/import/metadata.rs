use super::*;
use crate::{
    catalog::Catalog,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    io::Read,
    process::{Command, Stdio},
};
pub const EXTRACTOR_VERSION: &str = "exiftool-json-v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataResult {
    pub digest: Digest,
    pub extractor: String,
    pub capture_day: Option<String>,
    pub capture: String,
    pub camera: String,
    pub raw: serde_json::Value,
    pub error: Option<String>,
}
impl MetadataResult {
    pub fn from_json(digest: Digest, raw: serde_json::Value) -> Self {
        let capture = raw
            .get("DateTimeOriginal")
            .or_else(|| raw.get("CreateDate"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let capture_day = capture
            .get(..19)
            .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S").ok())
            .map(|d| d.format("%Y/%m/%d").to_string());
        let camera = ["Make", "Model", "SerialNumber"]
            .iter()
            .filter_map(|key| raw.get(key).and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("/");
        Self {
            digest,
            extractor: EXTRACTOR_VERSION.into(),
            capture_day,
            capture,
            camera,
            raw,
            error: None,
        }
    }
    pub fn fallback_day(&self, mtime: std::time::SystemTime, timezone: &str) -> AppResult<String> {
        if let Some(day) = &self.capture_day {
            return Ok(day.clone());
        }
        let tz: chrono_tz::Tz = timezone
            .parse()
            .map_err(|_| AppError::invalid("unknown import timezone"))?;
        let time: chrono::DateTime<chrono::Utc> = mtime.into();
        Ok(time.with_timezone(&tz).format("%Y/%m/%d").to_string())
    }
}
pub struct Recognition {
    pub digest: Digest,
    pub metadata: Option<MetadataResult>,
    pub shortcut: bool,
    pub hashed_bytes: u64,
}
pub fn hash_source(entry: &SourceEntry) -> AppResult<Digest> {
    let _timing = crate::metrics::Timer::start(crate::metrics::Stage::Hash);
    entry.validate()?;
    let mut file = entry
        .root
        .open_relative(&entry.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        crate::metrics::hash_bytes(n as u64);
        hasher.update(&buffer[..n]);
    }
    entry.validate()?;
    Digest::parse(&format!("blake3:{}", hasher.finalize().to_hex()))
}
pub fn recognize(
    entry: &SourceEntry,
    source: &SourceIdentity,
    catalog: &Catalog,
    strict: bool,
) -> AppResult<Recognition> {
    entry.validate()?;
    let known: Option<String> =
        if source.reliable && !strict && matches!(entry.role, SourceRole::Original(_)) {
            catalog
            .db
            .query_row(
                "SELECT digest FROM sources WHERE incarnation=? AND path=? AND mtime=? AND size=?",
                params![
                    source.incarnation,
                    entry.path.as_str(),
                    entry.fingerprint,
                    entry.size
                ],
                |r| r.get(0),
            )
            .optional()?
        } else {
            None
        };
    let shortcut = known.is_some();
    let digest = match known {
        Some(d) => Digest::parse(&d)?,
        None => hash_source(entry)?,
    };
    let metadata = catalog.metadata(&digest)?;
    Ok(Recognition {
        digest,
        metadata,
        shortcut,
        hashed_bytes: if shortcut { 0 } else { entry.size },
    })
}
impl Catalog {
    pub fn remember_source(
        &mut self,
        entry: &SourceEntry,
        identity: &SourceIdentity,
        digest: &Digest,
    ) -> AppResult<()> {
        entry.validate()?;
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT INTO blobs(digest,size) VALUES(?,?) ON CONFLICT DO NOTHING",
            params![digest.as_str(), entry.size],
        )?;
        tx.execute("INSERT INTO sources(incarnation,path,size,mtime,digest) VALUES(?,?,?,?,?) ON CONFLICT(incarnation,path) DO UPDATE SET size=excluded.size,mtime=excluded.mtime,digest=excluded.digest",params![identity.incarnation,entry.path.as_str(),entry.size,entry.fingerprint,digest.as_str()])?;
        tx.commit()?;
        Ok(())
    }
    pub fn cache_metadata(&mut self, result: &MetadataResult) -> AppResult<()> {
        self.db.execute("INSERT INTO metadata_cache(digest,metadata,extractor) VALUES(?,?,?) ON CONFLICT(digest,extractor) DO UPDATE SET metadata=excluded.metadata",params![result.digest.as_str(),serde_json::to_string(result)?,result.extractor])?;
        Ok(())
    }
    pub fn metadata(&self, digest: &Digest) -> AppResult<Option<MetadataResult>> {
        let value: Option<Option<String>> = self
            .db
            .query_row(
                "SELECT metadata FROM metadata_cache WHERE digest=? AND extractor=?",
                params![digest.as_str(), EXTRACTOR_VERSION],
                |r| r.get(0),
            )
            .optional()?;
        value
            .flatten()
            .map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
}
pub trait MetadataExtractor {
    fn extract(&self, entries: &[SourceEntry]) -> AppResult<Vec<MetadataResult>>;
}
pub struct ExifToolExtractor {
    pub executable: OsString,
}
impl Default for ExifToolExtractor {
    fn default() -> Self {
        Self {
            executable: "exiftool".into(),
        }
    }
}
impl ExifToolExtractor {
    pub fn arguments(entries: &[SourceEntry]) -> AppResult<Vec<OsString>> {
        if entries.len() > 128 {
            return Err(AppError::invalid("EXIF batch exceeds 128"));
        }
        let mut args: Vec<OsString> = [
            "-j",
            "-DateTimeOriginal",
            "-CreateDate",
            "-OffsetTimeOriginal",
            "-SubSecTimeOriginal",
            "-Make",
            "-Model",
            "-SerialNumber",
            "-SpecialMode",
            "-DriveMode",
            "-ShutterMode",
            "-ExposureCompensation",
            "--",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        for e in entries {
            e.validate()?;
            args.push(e.root.path().join(e.path.as_str()).into_os_string());
        }
        Ok(args)
    }
}
impl MetadataExtractor for ExifToolExtractor {
    fn extract(&self, entries: &[SourceEntry]) -> AppResult<Vec<MetadataResult>> {
        if entries.is_empty() {
            return Ok(vec![]);
        }
        let mut args = Self::arguments(entries)?;
        args.truncate(args.len() - entries.len());
        use std::os::fd::AsRawFd;
        let held = entries
            .iter()
            .map(|e| {
                e.root
                    .open_relative(&e.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)
            })
            .collect::<AppResult<Vec<_>>>()?;
        let paths = held
            .iter()
            .map(|file| format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd()))
            .collect::<Vec<_>>();
        args.extend(paths.iter().map(OsString::from));
        let child = Command::new(&self.executable)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                AppError::new(ErrorCode::Unavailable, format!("ExifTool unavailable: {e}"))
            })?;
        let mut child = crate::process::ChildGuard::new(child);
        crate::metrics::exif_invocation();
        let _timing = crate::metrics::Timer::start(crate::metrics::Stage::Exif);
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .ok_or_else(|| AppError::integrity("missing EXIF output"))?
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(AppError::integrity("EXIF response exceeds bound"));
        }
        let status = child.wait()?;
        let values: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap_or_default();
        let mut result = Vec::new();
        for (entry, path) in entries.iter().zip(paths.iter()) {
            entry.validate()?;
            let matched = values
                .iter()
                .filter(|v| v.get("SourceFile").and_then(|v| v.as_str()) == Some(path.as_str()))
                .collect::<Vec<_>>();
            let digest = hash_source(entry)?;
            let mut metadata = MetadataResult::from_json(
                digest,
                matched
                    .first()
                    .map(|v| (*v).clone())
                    .unwrap_or(serde_json::Value::Null),
            );
            if matched.len() != 1
                || metadata.raw.get("Error").is_some()
                || !status.success()
                || metadata.capture_day.is_none()
            {
                metadata.error =
                    Some("ExifTool failed, omitted source result, or has no capture date".into());
            }
            result.push(metadata);
        }
        Ok(result)
    }
}
