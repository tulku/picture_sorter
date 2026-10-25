use super::*;
use crate::{
    error::{AppError, AppResult},
    immich::*,
};
use std::path::PathBuf;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImmichProbeRequest {
    pub origin: Url,
    pub key_file: PathBuf,
    pub asset: uuid::Uuid,
    pub isolated_asset_confirmed: bool,
    pub fixture: bool,
}
pub fn qualify_immich(request: ImmichProbeRequest) -> AppResult<CapabilityReport> {
    if !request.isolated_asset_confirmed || request.asset.is_nil() {
        return Err(AppError::invalid(
            "probe requires an explicit isolated test asset and mutation confirmation",
        ));
    }
    if request.fixture
        && !match request.origin.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        }
    {
        return Err(AppError::invalid(
            "fixture evidence requires an IP loopback origin",
        ));
    }
    let mut report=CapabilityReport {schema_version:1,capability:"immich-ratings".into(),backend_or_application:if request.fixture {"immich-fixture"}else {"immich"}.into(),tested_version:TESTED_VERSION.into(),mount_identity:None,config_fingerprint:rating_fingerprint(&request.origin,TESTED_VERSION),scope:if request.fixture {ProbeScope::FixtureAsset {origin:request.origin.to_string(),asset:request.asset}}else {ProbeScope::IsolatedRuntime {origin:request.origin.to_string(),asset:request.asset}},results:vec![],limitations:vec!["Immich has no conditional rating update; a concurrent user change can race a PUT. Only the explicitly isolated asset is mutated by this probe.".into()]};
    let execute = || -> AppResult<Vec<ProbeResult>> {
        let client = ImmichClient::new(request.origin.clone(), &request.key_file)?;
        if client.server_info()?.version != TESTED_VERSION {
            return Err(AppError::invalid(
                "server version does not match the pinned profile",
            ));
        }
        let initial = client.asset(request.asset)?;
        if initial.kind != "IMAGE"
            || !matches!(initial.original_mime_type.as_deref(), Some("image/jpeg"))
        {
            return Err(AppError::invalid(
                "isolated probe asset must be a JPEG image",
            ));
        }
        let original = match remote_rating(&initial)? {
            RatingObservation::Stars(rating) => rating,
            _ => {
                return Err(AppError::invalid(
                    "isolated asset has no supported baseline rating",
                ));
            }
        };
        let mut expected = RatingObservation::Stars(original);
        let mut results = vec![ProbeResult {
            name: "supported_schema".into(),
            outcome: ProbeOutcome::Pass,
            detail: "Pinned server and explicit JPEG identity observed".into(),
        }];
        for value in [0, 3, 5, 0] {
            let desired = Rating::try_from(value)?;
            let outcome = client.probe_rating(RatingWrite {
                asset: request.asset,
                expected: expected.clone(),
                desired,
                jpeg_checksum: initial.checksum.clone(),
            })?;
            if !matches!(outcome, WriteOutcome::Confirmed) {
                results.push(ProbeResult {
                    name: format!("rating_{value}"),
                    outcome: ProbeOutcome::Fail,
                    detail:
                        "Uncertain or conflicting result; no blind retry or restoration attempted"
                            .into(),
                });
                return Ok(results);
            }
            expected = RatingObservation::Stars(desired);
            results.push(ProbeResult {
                name: format!("rating_{value}"),
                outcome: ProbeOutcome::Pass,
                detail:
                    "Write or already-equal value confirmed by GET with unchanged JPEG checksum"
                        .into(),
            });
        }
        let restored = client.probe_rating(RatingWrite {
            asset: request.asset,
            expected,
            desired: original,
            jpeg_checksum: initial.checksum,
        })?;
        results.push(ProbeResult {name:"restore_original".into(),outcome:if matches!(restored,WriteOutcome::Confirmed) {ProbeOutcome::Pass}else {ProbeOutcome::Fail},detail:"Restoration uses the last confirmed probe value as its precondition; non-CAS race remains".into()});
        Ok(results)
    };
    report.results = match execute() {
        Ok(results) => results,
        Err(error) => vec![ProbeResult {
            name: "runtime_probe".into(),
            outcome: ProbeOutcome::Fail,
            detail: error.to_string(),
        }],
    };
    Ok(report)
}
