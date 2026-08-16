//! Incremental watermark stored in the destination root.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const SIDECAR_NAME: &str = ".photo_sorter.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncrementalState {
    pub version: u32,
    /// Camera DateTimeOriginal, stored as naive local time treated as UTC.
    pub cutoff: String,
    /// Dest-relative path of the file that produced `cutoff`.
    pub last_file: String,
}

impl IncrementalState {
    pub fn new(cutoff: DateTime<Utc>, last_file: impl Into<String>) -> Self {
        Self {
            version: 1,
            cutoff: cutoff.format("%Y-%m-%dT%H:%M:%S").to_string(),
            last_file: last_file.into(),
        }
    }

    pub fn cutoff_dt(&self) -> Option<DateTime<Utc>> {
        parse_cutoff(&self.cutoff)
    }
}

pub fn sidecar_path(output_dir: &Path) -> PathBuf {
    output_dir.join(SIDECAR_NAME)
}

pub fn load(output_dir: &Path) -> io::Result<Option<IncrementalState>> {
    let path = sidecar_path(output_dir);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)?;
    let state: IncrementalState = serde_json::from_str(&text)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(state))
}

pub fn save(output_dir: &Path, state: &IncrementalState) -> io::Result<()> {
    let path = sidecar_path(output_dir);
    let text = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(path, text + "\n")
}

pub fn parse_cutoff(s: &str) -> Option<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S"))
        .ok()?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn tmp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "photo_sorter_sidecar_{}_{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_returns_none_when_missing() {
        let dir = tmp_dir();
        assert_eq!(load(&dir).unwrap(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trip_preserves_cutoff() {
        let dir = tmp_dir();
        let cutoff = Utc.with_ymd_and_hms(2026, 5, 25, 10, 52, 12).unwrap();
        let state = IncrementalState::new(cutoff, "RAW/2026/05/25/P5258981.ORF");
        save(&dir, &state).unwrap();
        let loaded = load(&dir).unwrap().unwrap();
        assert_eq!(loaded.cutoff_dt(), Some(cutoff));
        assert_eq!(loaded.last_file, "RAW/2026/05/25/P5258981.ORF");
        let _ = fs::remove_dir_all(&dir);
    }
}
