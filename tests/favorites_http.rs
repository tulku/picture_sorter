mod support;
use photo_sorter::{immich::*, qualification::*};
use std::os::unix::fs::PermissionsExt;
fn client(r: &support::TestRig, s: &support::http::RatingServer) -> ImmichClient {
    let key = r.state.join("key");
    std::fs::write(&key, b"fixture").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    ImmichClient::new(s.origin.clone(), &key).unwrap()
}
fn request(s: &support::http::RatingServer, expected: bool, desired: bool) -> FavoriteWrite {
    FavoriteWrite {
        asset: s.asset,
        expected,
        desired,
        jpeg_checksum: s.state.checksum.lock().unwrap().clone(),
        external_path: "/external/a.JPG".into(),
        library_id: None,
    }
}
fn evidence(r: &support::TestRig, s: &support::http::RatingServer) -> CapabilityReport {
    let _ = client(r, s);
    qualify_immich(ImmichProbeRequest {
        origin: s.origin.clone(),
        key_file: r.state.join("key"),
        asset: s.asset,
        isolated_asset_confirmed: true,
        fixture: true,
        favorites: true,
    })
    .unwrap()
}
#[test]
fn qualified_favorite_roundtrip_and_restore_preserves_stars() {
    let r = support::TestRig::new();
    let s = support::http::RatingServer::new();
    *s.state.rating.lock().unwrap() = Some(5);
    let report = evidence(&r, &s);
    assert_eq!(report.capability, "immich-favorites");
    assert!(
        report
            .results
            .iter()
            .all(|p| matches!(p.outcome, ProbeOutcome::Pass))
    );
    assert_eq!(*s.state.favorite.lock().unwrap(), Some(false));
    assert_eq!(*s.state.rating.lock().unwrap(), Some(5));
    let c = client(&r, &s).with_rating_policy(
        RatingWriteMode::BestEffort,
        CapabilityRegistry::new(vec![report]),
    );
    for (expected, desired) in [(false, true), (true, false)] {
        assert!(matches!(
            c.set_favorite(request(&s, expected, desired)).unwrap(),
            FavoriteOutcome::Confirmed
        ));
    }
    assert!(s.state.writes.lock().unwrap().is_empty());
}
#[test]
fn missing_favorite_is_unobserved_and_never_written() {
    let r = support::TestRig::new();
    let s = support::http::RatingServer::new();
    let report = evidence(&r, &s);
    *s.state.favorite.lock().unwrap() = None;
    let c = client(&r, &s).with_rating_policy(
        RatingWriteMode::BestEffort,
        CapabilityRegistry::new(vec![report]),
    );
    assert_eq!(c.asset(s.asset).unwrap().is_favorite, None);
    assert!(c.set_favorite(request(&s, false, true)).is_err());
}
#[test]
fn favorite_precondition_identity_and_uncertainty_are_checked() {
    let r = support::TestRig::new();
    let s = support::http::RatingServer::new();
    let report = evidence(&r, &s);
    let c = client(&r, &s).with_rating_policy(
        RatingWriteMode::BestEffort,
        CapabilityRegistry::new(vec![report]),
    );
    assert!(matches!(
        c.set_favorite(request(&s, true, true)).unwrap(),
        FavoriteOutcome::Conflict { observed: false }
    ));
    let mut wrong = request(&s, false, true);
    wrong.jpeg_checksum = "changed".into();
    assert!(c.set_favorite(wrong).is_err());
    s.state
        .drop_put
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        c.set_favorite(request(&s, false, true)).unwrap(),
        FavoriteOutcome::Uncertain
    ));
    assert_eq!(*s.state.favorite.lock().unwrap(), Some(true));
}
#[test]
fn star_only_report_does_not_qualify_favorite_writes() {
    let r = support::TestRig::new();
    let s = support::http::RatingServer::new();
    let mut report = evidence(&r, &s);
    report.capability = "immich-ratings".into();
    report.config_fingerprint = rating_fingerprint(&s.origin, TESTED_VERSION);
    let c = client(&r, &s).with_rating_policy(
        RatingWriteMode::BestEffort,
        CapabilityRegistry::new(vec![report]),
    );
    assert!(c.set_favorite(request(&s, false, true)).is_err());
}
#[test]
fn incomplete_favorite_evidence_cannot_enable_writes() {
    let r = support::TestRig::new();
    let s = support::http::RatingServer::new();
    let mut report = evidence(&r, &s);
    report.results.retain(|p| p.name == "supported_schema");
    let registry = CapabilityRegistry::new(vec![report]);
    assert!(
        !registry
            .allows(OperationKind::FavoriteWrite {
                origin: &s.origin,
                asset: s.asset,
                version: TESTED_VERSION
            })
            .allowed
    );
}
#[test]
fn full_jpeg_identity_is_checked_immediately_before_favorite_put() {
    for changed in [
        serde_json::json!({"originalPath":"/external/moved.JPG"}),
        serde_json::json!({"libraryId":uuid::Uuid::new_v4()}),
        serde_json::json!({"type":"VIDEO"}),
        serde_json::json!({"originalMimeType":"image/png"}),
    ] {
        let r = support::TestRig::new();
        let s = support::http::RatingServer::new();
        let report = evidence(&r, &s);
        let c = client(&r, &s).with_rating_policy(
            RatingWriteMode::BestEffort,
            CapabilityRegistry::new(vec![report]),
        );
        let write = request(&s, false, true);
        let count = s.state.favorite_writes.lock().unwrap().len();
        *s.state.asset_fields.lock().unwrap() = changed;
        assert!(c.set_favorite(write).is_err());
        assert_eq!(s.state.favorite_writes.lock().unwrap().len(), count);
    }
}
#[test]
fn identity_change_after_put_is_not_reported_as_favorite_agreement_or_disagreement() {
    let r = support::TestRig::new();
    let s = support::http::RatingServer::new();
    let report = evidence(&r, &s);
    let c = client(&r, &s).with_rating_policy(
        RatingWriteMode::BestEffort,
        CapabilityRegistry::new(vec![report]),
    );
    *s.state.after_put_fields.lock().unwrap() =
        serde_json::json!({"originalPath":"/external/moved.JPG"});
    let outcome = c.set_favorite(request(&s, false, true));
    assert!(!matches!(
        outcome,
        Ok(FavoriteOutcome::Confirmed | FavoriteOutcome::Conflict { .. })
    ));
}
