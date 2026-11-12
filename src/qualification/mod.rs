use crate::domain::*;
use serde::{Deserialize, Serialize};
use url::Url;
mod immich;
pub use immich::*;
mod rapidraw;
pub use rapidraw::*;
mod nas;
pub use nas::*;
mod report;
pub use report::*;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProbeOutcome {
    Pass,
    Fail,
    Unrun,
    Inconclusive,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub name: String,
    pub outcome: ProbeOutcome,
    pub detail: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProbeScope {
    Source,
    EditorRuntime,
    FixtureAsset { origin: String, asset: uuid::Uuid },
    IsolatedRuntime { origin: String, asset: uuid::Uuid },
    SingleClient,
    TwoClient,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityReport {
    pub schema_version: u32,
    pub capability: String,
    pub backend_or_application: String,
    pub tested_version: String,
    pub mount_identity: Option<String>,
    pub config_fingerprint: Digest,
    pub scope: ProbeScope,
    pub results: Vec<ProbeResult>,
    pub limitations: Vec<String>,
}
#[derive(Default, Debug, Clone)]
pub struct CapabilityRegistry {
    reports: Vec<CapabilityReport>,
}
pub enum OperationKind<'a> {
    EditorTransform {
        version: &'a str,
        fingerprint: &'a Digest,
    },
    ArchiveWrite {
        mount_identity: &'a str,
        fingerprint: &'a Digest,
        mode: &'a crate::archive::PublicationMode,
    },
    ArchiveExchange {
        mount_identity: &'a str,
        fingerprint: &'a Digest,
        mode: &'a crate::archive::PublicationMode,
    },
    EditorPickTransform {
        version: &'a str,
        fingerprint: &'a Digest,
    },
    FavoriteWrite {
        origin: &'a Url,
        asset: uuid::Uuid,
        version: &'a str,
    },
    RatingWrite {
        origin: &'a Url,
        asset: uuid::Uuid,
        version: &'a str,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityDecision {
    pub allowed: bool,
    pub reason: String,
}
pub fn rating_fingerprint(origin: &Url, version: &str) -> Digest {
    Digest::of(
        format!(
            "immich-ratings-v3-null:{}:{version}",
            origin.origin().ascii_serialization()
        )
        .as_bytes(),
    )
}
pub fn favorite_fingerprint(origin: &Url, version: &str) -> Digest {
    Digest::of(
        format!(
            "immich-favorites-v1:{}:{version}",
            origin.origin().ascii_serialization()
        )
        .as_bytes(),
    )
}
impl CapabilityRegistry {
    pub fn new(reports: Vec<CapabilityReport>) -> Self {
        Self { reports }
    }
    pub fn allows(&self, operation: OperationKind<'_>) -> CapabilityDecision {
        let (origin, asset, version, favorite) = match operation {
            OperationKind::EditorPickTransform {
                version,
                fingerprint,
            } => {
                let allowed = self
                    .allows(OperationKind::EditorTransform {
                        version,
                        fingerprint,
                    })
                    .allowed
                    && self.reports.iter().any(|r| {
                        r.schema_version == 1
                            && r.capability == "rapidraw-transform"
                            && r.tested_version == version
                            && &r.config_fingerprint == fingerprint
                            && matches!(r.scope, ProbeScope::EditorRuntime)
                            && ["save_pick", "clear_pick", "reject_priority"]
                                .iter()
                                .all(|name| {
                                    r.results.iter().any(|p| {
                                        p.name == *name && matches!(p.outcome, ProbeOutcome::Pass)
                                    })
                                })
                    });
                return CapabilityDecision {
                    allowed,
                    reason: "native Pick runtime evidence required".into(),
                };
            }
            OperationKind::FavoriteWrite {
                origin,
                asset,
                version,
            } => (origin, asset, version, true),
            OperationKind::EditorTransform {
                version,
                fingerprint,
            } => {
                let allowed = self.reports.iter().any(|r| {
                    r.schema_version == 1
                        && r.capability == "rapidraw-transform"
                        && r.backend_or_application == "RapidRAW"
                        && r.tested_version == version
                        && &r.config_fingerprint == fingerprint
                        && matches!(r.scope, ProbeScope::EditorRuntime)
                        && [
                            "save_three",
                            "save_five",
                            "save_zero",
                            "reopen",
                            "reset_regeneration",
                            "runtime_application",
                        ]
                        .iter()
                        .all(|name| {
                            r.results
                                .iter()
                                .any(|p| p.name == *name && matches!(p.outcome, ProbeOutcome::Pass))
                        })
                });
                return CapabilityDecision {
                    allowed,
                    reason: if allowed {
                        "matching native editor runtime evidence"
                    } else {
                        "no matching native editor runtime evidence"
                    }
                    .into(),
                };
            }
            OperationKind::RatingWrite {
                origin,
                asset,
                version,
            } => (origin, asset, version, false),
            OperationKind::ArchiveWrite {
                mount_identity,
                fingerprint,
                mode,
            } => return self.archive_decision(mount_identity, fingerprint, mode, false),
            OperationKind::ArchiveExchange {
                mount_identity,
                fingerprint,
                mode,
            } => return self.archive_decision(mount_identity, fingerprint, mode, true),
        };
        let fingerprint = if favorite {
            favorite_fingerprint(origin, version)
        } else {
            rating_fingerprint(origin, version)
        };
        for report in &self.reports {
            if report.schema_version != 1
                || report.capability
                    != if favorite {
                        "immich-favorites"
                    } else {
                        "immich-ratings"
                    }
                || report.tested_version != version
                || report.config_fingerprint != fingerprint
                || report.results.is_empty()
                || (favorite
                    && ![
                        "supported_schema",
                        "favorite_true",
                        "favorite_false",
                        "restore_original",
                    ]
                    .iter()
                    .all(|name| {
                        report
                            .results
                            .iter()
                            .any(|p| p.name == *name && matches!(p.outcome, ProbeOutcome::Pass))
                    }))
                || !report
                    .results
                    .iter()
                    .all(|r| matches!(r.outcome, ProbeOutcome::Pass))
            {
                continue;
            }
            let scope = match &report.scope {
                ProbeScope::IsolatedRuntime { origin: tested, .. } => {
                    tested == origin.as_str() && report.backend_or_application == "immich"
                }
                ProbeScope::FixtureAsset {
                    origin: tested,
                    asset: tested_asset,
                } => {
                    tested == origin.as_str()
                        && *tested_asset == asset
                        && report.backend_or_application == "immich-fixture"
                        && match origin.host() {
                            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                            _ => false,
                        }
                }
                _ => false,
            };
            if scope {
                return CapabilityDecision {
                    allowed: true,
                    reason: "matching scoped runtime evidence".into(),
                };
            }
        }
        CapabilityDecision {
            allowed: false,
            reason: "no matching runtime qualification for origin/version/scope".into(),
        }
    }
}

impl CapabilityRegistry {
    pub fn reports(&self) -> &[CapabilityReport] {
        &self.reports
    }
    fn archive_decision(
        &self,
        mount_identity: &str,
        fingerprint: &Digest,
        mode: &crate::archive::PublicationMode,
        exchange: bool,
    ) -> CapabilityDecision {
        use crate::archive::PublicationMode;
        let required = [
            "exclusive_init",
            "no_replace",
            "lock_exclusion",
            "flush_reopen",
        ];
        let matches = self.reports.iter().any(|report| {
            report.schema_version == 1
                && report.capability == "nas-publication"
                && report.backend_or_application == "linux-filesystem"
                && report.tested_version == NAS_IMPLEMENTATION
                && report.mount_identity.as_deref() == Some(mount_identity)
                && &report.config_fingerprint == fingerprint
                && required.iter().all(|name| {
                    report
                        .results
                        .iter()
                        .any(|r| r.name == *name && matches!(r.outcome, ProbeOutcome::Pass))
                })
                && (!(exchange || *mode == PublicationMode::MultiWriter)
                    || report.results.iter().any(|r| {
                        r.name == "rename_exchange" && matches!(r.outcome, ProbeOutcome::Pass)
                    }))
                && match mode {
                    PublicationMode::SingleWriter => matches!(
                        report.scope,
                        ProbeScope::SingleClient | ProbeScope::TwoClient
                    ),
                    PublicationMode::MultiWriter => {
                        matches!(report.scope, ProbeScope::TwoClient)
                            && ["peer_handoff", "disconnect_recovery"].iter().all(|name| {
                                report.results.iter().any(|r| {
                                    r.name == *name && matches!(r.outcome, ProbeOutcome::Pass)
                                })
                            })
                    }
                    PublicationMode::LocalTest => false,
                }
        });
        CapabilityDecision {
            allowed: matches,
            reason: if matches {
                "matching runtime filesystem evidence"
            } else if exchange {
                "NAS metadata replacement requires matching atomic exchange evidence"
            } else {
                "no matching filesystem/version/mode qualification"
            }
            .into(),
        }
    }
}

mod doctor;
pub use doctor::doctor;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "request",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum QualificationRequest {
    Nas(NasProbeRequest),
    Rapidraw(EditorProbeRequest),
    Immich(ImmichProbeRequest),
}
pub fn validate_request(
    config: &crate::config::Config,
    request: &QualificationRequest,
) -> crate::error::AppResult<()> {
    let request = match request {
        QualificationRequest::Nas(r) => r,
        QualificationRequest::Immich(r) => {
            let configured = config.immich.as_ref().ok_or_else(|| {
                crate::error::AppError::invalid("probe requires explicit Immich profile")
            })?;
            if r.origin.as_str() != configured.origin || r.key_file != configured.key_file {
                return Err(crate::error::AppError::invalid(
                    "probe origin and secret file must match the profile",
                ));
            }
            return Ok(());
        }
        QualificationRequest::Rapidraw(r) => {
            if r.version != config.rapidraw_version || r.runtime_binary != config.rapidraw_binary {
                return Err(crate::error::AppError::invalid(
                    "editor version and binary must match profile",
                ));
            }
            let isolated = crate::config::normalized(&r.isolated_root)?;
            for root in [
                &config.working_root,
                &config.state_root,
                &config.archive_root,
            ] {
                let root = crate::config::normalized(root)?;
                if isolated.starts_with(&root) || root.starts_with(&isolated) {
                    return Err(crate::error::AppError::invalid(
                        "editor fixture root cannot overlap managed roots",
                    ));
                }
            }
            return Ok(());
        }
    };
    if crate::config::normalized(&request.archive_root)?
        != crate::config::normalized(&config.archive_root)?
        || Some(request.archive) != config.archive_id
        || request.writer != config.writer_id
        || request.local_test_backend != config.local_test_backend
    {
        return Err(crate::error::AppError::invalid(
            "probe archive, writer and backend must match the explicit profile",
        ));
    }
    let scratch = crate::config::normalized(&request.scratch_root)?;
    for root in [
        &config.working_root,
        &config.state_root,
        &config.archive_root,
    ] {
        let root = crate::config::normalized(root)?;
        if scratch.starts_with(&root) || root.starts_with(&scratch) {
            return Err(crate::error::AppError::invalid(
                "probe scratch cannot overlap any managed root",
            ));
        }
    }
    Ok(())
}
pub fn report_qualified(request: &QualificationRequest, report: &CapabilityReport) -> bool {
    let request = match request {
        QualificationRequest::Nas(r) => r,
        QualificationRequest::Immich(r) => {
            return CapabilityRegistry::new(vec![report.clone()])
                .allows(if r.favorites {
                    OperationKind::FavoriteWrite {
                        origin: &r.origin,
                        asset: r.asset,
                        version: &report.tested_version,
                    }
                } else {
                    OperationKind::RatingWrite {
                        origin: &r.origin,
                        asset: r.asset,
                        version: &report.tested_version,
                    }
                })
                .allowed;
        }
        QualificationRequest::Rapidraw(_) => {
            return matches!(report.scope, ProbeScope::EditorRuntime)
                && report
                    .results
                    .iter()
                    .all(|r| matches!(r.outcome, ProbeOutcome::Pass));
        }
    };
    report.mount_identity.as_ref().is_some_and(|mount| {
        CapabilityRegistry::new(vec![report.clone()])
            .allows(OperationKind::ArchiveWrite {
                mount_identity: mount,
                fingerprint: &report.config_fingerprint,
                mode: &request.mode,
            })
            .allowed
    })
}
