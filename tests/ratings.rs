use photo_sorter::{domain::*, ratings::*};
fn stars(n: u8) -> RatingObservation {
    RatingObservation::Stars(Rating::try_from(n).unwrap())
}
fn input() -> RatingInputs {
    RatingInputs {
        last_common: stars(3),
        last_applied_local: stars(3),
        local: stars(3),
        remote: stars(3),
        pending_canonical: None,
        local_observation: None,
        remote_observation: None,
    }
}
#[test]
fn missing_vs_explicit_zero() {
    let mut i = input();
    i.last_common = RatingObservation::Unobserved;
    i.last_applied_local = RatingObservation::Unobserved;
    i.local = stars(0);
    i.remote = RatingObservation::Unobserved;
    assert_eq!(
        reconcile_rating(&i),
        RatingDecision::PublishLocal(Rating::try_from(0).unwrap())
    );
    i.local = RatingObservation::Unobserved;
    assert_eq!(reconcile_rating(&i), RatingDecision::Unchanged);
}
#[test]
fn pending_remote_star_unrelated_exposure_save() {
    let mut i = input();
    i.remote = stars(5);
    i.pending_canonical = Some(Rating::try_from(5).unwrap());
    assert_eq!(
        reconcile_rating(&i),
        RatingDecision::KeepPending(Rating::try_from(5).unwrap())
    );
}
#[test]
fn reset_keeps_pending_rating_intent() {
    let mut i = input();
    i.local = RatingObservation::Unobserved;
    i.pending_canonical = Some(Rating::try_from(5).unwrap());
    assert_eq!(
        reconcile_rating(&i),
        RatingDecision::KeepPending(Rating::try_from(5).unwrap())
    );
}
#[test]
fn deliberate_change_during_incoming_rating_conflicts() {
    let mut i = input();
    i.local = stars(4);
    i.remote = stars(5);
    i.pending_canonical = Some(Rating::try_from(5).unwrap());
    assert!(matches!(
        reconcile_rating(&i),
        RatingDecision::Conflict { .. }
    ));
}
#[test]
fn one_sided_equal_and_divergent_changes() {
    let mut i = input();
    i.local = stars(4);
    assert_eq!(
        reconcile_rating(&i),
        RatingDecision::PublishLocal(Rating::try_from(4).unwrap())
    );
    i.remote = stars(4);
    assert_eq!(
        reconcile_rating(&i),
        RatingDecision::Converge(Rating::try_from(4).unwrap())
    );
    i.remote = stars(5);
    assert!(matches!(
        reconcile_rating(&i),
        RatingDecision::Conflict { .. }
    ));
}
#[test]
fn newer_remote_value_supersedes_unapplied_previous_remote() {
    let mut i = input();
    i.last_common = stars(5);
    i.remote = stars(4);
    i.pending_canonical = Some(Rating::try_from(5).unwrap());
    assert_eq!(
        reconcile_rating(&i),
        RatingDecision::AdoptRemote(Rating::try_from(4).unwrap())
    );
}
#[test]
fn both_agree_on_new_stars_over_old_pending() {
    let mut i = input();
    i.last_common = stars(5);
    i.local = stars(4);
    i.remote = stars(4);
    i.pending_canonical = Some(Rating::try_from(5).unwrap());
    assert_eq!(
        reconcile_rating(&i),
        RatingDecision::Converge(Rating::try_from(4).unwrap())
    );
}
