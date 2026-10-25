use crate::domain::*;
use serde::{Deserialize, Serialize};
use url::Url;
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
    RatingWrite {
        origin: &'a Url,
        asset: uuid::Uuid,
        version: &'a str,
    },
}
#[derive(Debug)]
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
impl CapabilityRegistry {
    pub fn new(reports: Vec<CapabilityReport>) -> Self {
        Self { reports }
    }
    pub fn allows(&self, operation: OperationKind<'_>) -> CapabilityDecision {
        let OperationKind::RatingWrite {
            origin,
            asset,
            version,
        } = operation;
        let fingerprint = rating_fingerprint(origin, version);
        for report in &self.reports {
            if report.schema_version != 1
                || report.capability != "immich-ratings"
                || report.tested_version != version
                || report.config_fingerprint != fingerprint
                || report.results.is_empty()
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
