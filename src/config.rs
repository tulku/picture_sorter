use crate::{
    domain::*,
    error::{AppError, AppResult},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub working_root: PathBuf,
    pub state_root: PathBuf,
    pub archive_root: PathBuf,
    pub profile_id: ProfileId,
    pub writer_id: WriterId,
    pub archive_id: Option<ArchiveId>,
    pub budget: u64,
    pub reserve: u64,
    pub debounce_seconds: u64,
    pub reconcile_seconds: u64,
    pub activity_hold_seconds: u64,
    pub poll_seconds: u64,
    pub metadata_batch: usize,
    pub json_ceiling: u64,
    pub import_timezone: String,
    pub immich: Option<ImmichConfig>,
    pub capability_evidence: Vec<PathBuf>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImmichConfig {
    pub origin: String,
    pub key_file: PathBuf,
    pub library_id: Option<String>,
    pub write_mode: String,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            working_root: PathBuf::new(),
            state_root: PathBuf::new(),
            archive_root: PathBuf::new(),
            profile_id: ProfileId::new(),
            writer_id: WriterId::new(),
            archive_id: None,
            budget: 75_161_927_680,
            reserve: 5_368_709_120,
            debounce_seconds: 2,
            reconcile_seconds: 300,
            activity_hold_seconds: 600,
            poll_seconds: 60,
            metadata_batch: 128,
            json_ceiling: 67_108_864,
            import_timezone: "UTC".into(),
            immich: None,
            capability_evidence: vec![],
        }
    }
}
impl Config {
    pub fn new(working_root: PathBuf, state_root: PathBuf, archive_root: PathBuf) -> Self {
        Self {
            working_root,
            state_root,
            archive_root,
            ..Self::default()
        }
    }
    pub fn load(path: &Path) -> AppResult<Self> {
        let text = std::fs::read_to_string(path)?;
        let c: Self = toml::from_str(&text).map_err(|e| AppError::invalid(e.to_string()))?;
        c.validate()?;
        Ok(c)
    }
    pub fn validate(&self) -> AppResult<()> {
        let roots = [&self.working_root, &self.state_root, &self.archive_root]
            .map(|p| normalized(p))
            .into_iter()
            .collect::<AppResult<Vec<_>>>()?;
        for i in 0..roots.len() {
            for j in i + 1..roots.len() {
                if roots[i].starts_with(&roots[j]) || roots[j].starts_with(&roots[i]) {
                    return Err(AppError::invalid(
                        "working, state and archive roots must not overlap",
                    ));
                }
            }
        }
        if self.metadata_batch == 0
            || self.metadata_batch > 128
            || self.budget == 0
            || self.reconcile_seconds == 0
            || self.poll_seconds == 0
            || self.json_ceiling == 0
        {
            return Err(AppError::invalid("invalid batch, byte or timer limit"));
        }
        if self.import_timezone.parse::<chrono_tz::Tz>().is_err() {
            return Err(AppError::invalid("unknown import timezone"));
        }
        Ok(())
    }
}
pub fn normalized(path: &Path) -> AppResult<PathBuf> {
    if !path.is_absolute() {
        return Err(AppError::invalid("roots must be absolute"));
    }
    if path.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err(AppError::invalid("root contains traversal"));
    }
    match path.canonicalize() {
        Ok(p) => Ok(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| AppError::invalid("root has no parent"))?;
            Ok(normalized(parent)?.join(
                path.file_name()
                    .ok_or_else(|| AppError::invalid("root has no name"))?,
            ))
        }
        Err(e) => Err(e.into()),
    }
}
