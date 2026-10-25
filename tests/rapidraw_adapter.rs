use photo_sorter::{domain::*, edit::*};
fn adapter() -> RapidRawAdapter {
    RapidRawAdapter::for_version("1.6.6").unwrap()
}
#[test]
fn ratings_round_trip_zero_three_five_zero() {
    let a = adapter();
    let mut bytes = include_bytes!("fixtures/rapidraw/default.json").to_vec();
    for value in [0, 3, 5, 0] {
        bytes = a
            .with_rating(
                &a.inspect(&bytes).unwrap(),
                Rating::try_from(value).unwrap(),
            )
            .unwrap();
        assert_eq!(
            a.inspect(&bytes).unwrap().rating,
            RatingObservation::Stars(Rating::try_from(value).unwrap())
        );
    }
}
#[test]
fn unknown_fields_and_masks_survive() {
    let a = adapter();
    let d = a
        .inspect(include_bytes!("fixtures/rapidraw/edited.json"))
        .unwrap();
    let edited = a
        .inspect(&a.with_rating(&d, Rating::try_from(5).unwrap()).unwrap())
        .unwrap();
    assert_eq!(d.non_rating(), edited.non_rating());
}
#[test]
fn rejected_not_coerced() {
    let a = adapter();
    let doc = a
        .inspect(br#"{"version":1,"rating":3,"flag":"reject","adjustments":null}"#)
        .unwrap();
    assert_eq!(doc.rating, RatingObservation::Rejected);
    assert!(a.with_rating(&doc, Rating::try_from(5).unwrap()).is_err());
}
#[test]
fn initial_sidecar_is_valid_native() {
    let a = adapter();
    let bytes = a.initial_document(Rating::try_from(3).unwrap()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["rating"], 3);
    assert!(value["adjustments"].is_null());
    assert!(value["tags"].is_null());
    assert!(value.get("flag").is_none());
    assert!(a.inspect(&bytes).is_ok());
}
#[test]
fn base_raw_only_not_virtual_copy() {
    let a = adapter();
    let primary = ArchivePath::parse("RAW/a.ORF").unwrap();
    assert!(a.is_rating_authority(&primary, &ArchivePath::parse("RAW/a.ORF.rrdata").unwrap()));
    assert!(!a.is_rating_authority(
        &primary,
        &ArchivePath::parse("RAW/a.ORF.abcdef.rrdata").unwrap()
    ));
}
#[test]
fn unknown_version_transforms_disabled() {
    assert!(RapidRawAdapter::for_version("99.0").is_err());
    assert!(
        adapter()
            .inspect(br#"{"version":99,"rating":3,"adjustments":null}"#)
            .is_err()
    );
}
#[test]
fn remote_rating_only_local_adjustment_merge() {
    let a = adapter();
    let base = a
        .inspect(include_bytes!("fixtures/rapidraw/edited.json"))
        .unwrap();
    let mut local = base.value().clone();
    local["adjustments"]["exposure"] = serde_json::json!(1.2);
    let local = a.inspect(&serde_json::to_vec(&local).unwrap()).unwrap();
    let remote = a
        .inspect(&a.with_rating(&base, Rating::try_from(5).unwrap()).unwrap())
        .unwrap();
    let merged = a
        .merge_rating_adjustments(&base, &local, &remote, Rating::try_from(5).unwrap())
        .unwrap();
    let merged = a.inspect(&merged).unwrap();
    assert_eq!(merged.non_rating(), local.non_rating());
    assert_eq!(
        merged.rating,
        RatingObservation::Stars(Rating::try_from(5).unwrap())
    );
}
#[test]
fn divergent_adjustments_conflict() {
    let a = adapter();
    let base = a
        .inspect(include_bytes!("fixtures/rapidraw/edited.json"))
        .unwrap();
    let mut left = base.value().clone();
    let mut right = left.clone();
    left["adjustments"]["exposure"] = serde_json::json!(1.2);
    right["adjustments"]["exposure"] = serde_json::json!(2.4);
    let left = a.inspect(&serde_json::to_vec(&left).unwrap()).unwrap();
    let right = a.inspect(&serde_json::to_vec(&right).unwrap()).unwrap();
    assert!(
        a.merge_rating_adjustments(&base, &left, &right, Rating::try_from(5).unwrap())
            .is_err()
    );
}
