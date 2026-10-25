use super::*;
use crate::{
    edit::RapidRawAdapter,
    error::{AppError, AppResult},
    fs::{RootHandle, RootPolicy},
};
use std::path::PathBuf;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickProbeDocuments {
    pub picked: ArchivePath,
    pub cleared: ArchivePath,
    pub rejected: ArchivePath,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorProbeRequest {
    #[serde(default)]
    pub pick: Option<PickProbeDocuments>,
    pub isolated_root: PathBuf,
    pub version: String,
    pub before: ArchivePath,
    pub three: ArchivePath,
    pub five: ArchivePath,
    pub zero: ArchivePath,
    pub reopened: ArchivePath,
    pub regenerated_after_reset: ArchivePath,
    pub runtime_binary: Option<PathBuf>,
    pub runtime_saves_attested: bool,
}
pub fn editor_fingerprint(version: &str, binary: Option<&std::path::Path>) -> AppResult<Digest> {
    let binary = if let Some(path) = binary {
        let parent = RootHandle::open(
            path.parent()
                .ok_or_else(|| AppError::invalid("editor binary needs a parent"))?,
            RootPolicy::Local,
        )?;
        let name = ArchivePath::parse(
            path.file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| AppError::invalid("editor binary name"))?,
        )?;
        let mut file = parent.open_relative(&name, libc::O_RDONLY, 0)?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0; 128 * 1024];
        loop {
            use std::io::Read;
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        let digest = Digest::parse(&format!("blake3:{}", hasher.finalize().to_hex()))?;
        format!("{}:{}", path.display(), digest.as_str())
    } else {
        "source-only".into()
    };
    Ok(Digest::of(
        format!("rapidraw-native-v1:{version}:{binary}").as_bytes(),
    ))
}
pub fn qualify_rapidraw(request: EditorProbeRequest) -> AppResult<CapabilityReport> {
    let root = RootHandle::open(&request.isolated_root, RootPolicy::Local)?;
    let fingerprint = editor_fingerprint(&request.version, request.runtime_binary.as_deref())?;
    let mut report=CapabilityReport {schema_version:1,capability:"rapidraw-transform".into(),backend_or_application:"RapidRAW".into(),tested_version:request.version.clone(),mount_identity:None,config_fingerprint:fingerprint,scope:ProbeScope::Source,results:vec![],limitations:vec!["Native saves and reopen/reset steps are operator supplied; the probe does not launch the editor.".into()]};
    let adapter = match RapidRawAdapter::for_version(&request.version) {
        Ok(a) => a,
        Err(error) => {
            report.results.push(ProbeResult {
                name: "supported_schema".into(),
                outcome: ProbeOutcome::Fail,
                detail: error.to_string(),
            });
            return Ok(report);
        }
    };
    let compare = || -> AppResult<Vec<ProbeResult>> {
        let base = adapter.inspect(&root.read(&request.before, 64 * 1024 * 1024)?)?;
        let mut results = Vec::new();
        for (name, path, rating) in [
            ("save_three", &request.three, 3),
            ("save_five", &request.five, 5),
            ("save_zero", &request.zero, 0),
            ("reopen", &request.reopened, 0),
        ] {
            let document = adapter.inspect(&root.read(path, 64 * 1024 * 1024)?)?;
            let pass = document.rating == RatingObservation::Stars(Rating::try_from(rating)?)
                && document.non_rating() == base.non_rating();
            results.push(ProbeResult {name:name.into(),outcome:if pass {ProbeOutcome::Pass}else {ProbeOutcome::Fail},detail:"Exact non-rating fields, unknown fields and masks compared with the supplied baseline".into()});
        }
        if let Some(pick) = &request.pick {
            for (name, path, flag) in [
                ("save_pick", &pick.picked, crate::edit::PickFlag::Pick),
                ("clear_pick", &pick.cleared, crate::edit::PickFlag::Neutral),
                (
                    "reject_priority",
                    &pick.rejected,
                    crate::edit::PickFlag::Reject,
                ),
            ] {
                let document = adapter.inspect(&root.read(path, 64 * 1024 * 1024)?)?;
                let pass = document.flag == flag
                    && document.non_flag() == base.non_flag()
                    && (flag != crate::edit::PickFlag::Reject
                        || adapter.with_favorite(&document, true)? == document.source_bytes());
                results.push(ProbeResult {
                    name: name.into(),
                    outcome: if pass {
                        ProbeOutcome::Pass
                    } else {
                        ProbeOutcome::Fail
                    },
                    detail: "Native flags and all non-flag fields compared with baseline".into(),
                });
            }
        }
        let reset =
            adapter.inspect(&root.read(&request.regenerated_after_reset, 64 * 1024 * 1024)?)?;
        let default = adapter.inspect(&adapter.initial_document(Rating::try_from(0)?)?)?;
        results.push(ProbeResult {
            name: "reset_regeneration".into(),
            outcome: if reset.value() == default.value() {
                ProbeOutcome::Pass
            } else {
                ProbeOutcome::Fail
            },
            detail: "Regenerated document compared with the source-backed native default".into(),
        });
        Ok(results)
    };
    report.results = match compare() {
        Ok(results) => results,
        Err(error) => vec![ProbeResult {
            name: "native_documents".into(),
            outcome: ProbeOutcome::Fail,
            detail: error.to_string(),
        }],
    };
    let runtime = request.runtime_binary.is_some() && request.runtime_saves_attested;
    report.results.push(ProbeResult {
        name: "runtime_application".into(),
        outcome: if runtime {
            ProbeOutcome::Pass
        } else {
            ProbeOutcome::Unrun
        },
        detail: if runtime {
            "Operator attested native saves; exact executable digest bound to evidence"
        } else {
            "Actual installed editor and native runtime saves not attested"
        }
        .into(),
    });
    if runtime {
        report.scope = ProbeScope::EditorRuntime;
    }
    Ok(report)
}
#[derive(Clone)]
pub struct EditorRuntimeIdentity {
    binary: PathBuf,
    version: String,
    stamp: (u64, u64, u64, i64, i64, i64, i64),
    fingerprint: Digest,
}
fn binary_stamp(path: &std::path::Path) -> AppResult<(u64, u64, u64, i64, i64, i64, i64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err(AppError::invalid("editor binary must be a regular file"));
    }
    Ok((
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    ))
}
impl EditorRuntimeIdentity {
    pub fn capture(config: &crate::config::Config) -> AppResult<Option<Self>> {
        let Some(binary) = config.rapidraw_binary.as_ref() else {
            return Ok(None);
        };
        let stamp = binary_stamp(binary)?;
        let fingerprint = editor_fingerprint(&config.rapidraw_version, Some(binary))?;
        if stamp != binary_stamp(binary)? {
            return Err(AppError::invalid(
                "editor changed during qualification identity capture",
            ));
        }
        Ok(Some(Self {
            binary: binary.clone(),
            version: config.rapidraw_version.clone(),
            stamp,
            fingerprint,
        }))
    }
    pub fn decision(&self, config: &crate::config::Config) -> CapabilityDecision {
        if config.rapidraw_version != self.version
            || config.rapidraw_binary.as_ref() != Some(&self.binary)
            || binary_stamp(&self.binary).ok() != Some(self.stamp)
        {
            return CapabilityDecision {
                allowed: false,
                reason: "editor executable identity changed; restart and requalify".into(),
            };
        }
        super::load_registry(config)
            .unwrap_or_default()
            .allows(OperationKind::EditorTransform {
                version: &self.version,
                fingerprint: &self.fingerprint,
            })
    }
    pub fn pick_decision(&self, config: &crate::config::Config) -> CapabilityDecision {
        if config.rapidraw_version != self.version
            || config.rapidraw_binary.as_ref() != Some(&self.binary)
            || binary_stamp(&self.binary).ok() != Some(self.stamp)
        {
            return CapabilityDecision {
                allowed: false,
                reason: "editor executable identity changed; restart and requalify".into(),
            };
        }
        super::load_registry(config).unwrap_or_default().allows(
            OperationKind::EditorPickTransform {
                version: &self.version,
                fingerprint: &self.fingerprint,
            },
        )
    }
}
