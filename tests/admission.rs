mod support;
use photo_sorter::{
    catalog::*,
    domain::*,
    fs::{RootHandle, RootPolicy},
    storage::*,
};
fn input() -> AdmissionInput {
    AdmissionInput {
        budget: 100,
        occupied: 60,
        reserved: 0,
        available: 100,
        reserve: 10,
        required: 40,
        existing: 0,
        partial: 0,
        overhead: 0,
        nas_available: false,
        allow_direct: true,
    }
}
#[test]
fn exact_budget_boundary() {
    assert_eq!(
        admit(&input()),
        AdmissionDecision::Local {
            additional_bytes: 40
        }
    );
    let mut i = input();
    i.required = 41;
    assert_eq!(admit(&i), AdmissionDecision::WaitingCapacity);
    i.nas_available = true;
    assert_eq!(admit(&i), AdmissionDecision::DirectNas);
}
#[test]
fn partials_not_double_counted() {
    let mut i = input();
    i.occupied = 80;
    i.partial = 20;
    assert_eq!(
        admit(&i),
        AdmissionDecision::Local {
            additional_bytes: 20
        }
    );
}
#[test]
fn resident_member_only_charges_missing_bytes() {
    let mut i = input();
    i.required = 100;
    i.existing = 70;
    assert_eq!(
        admit(&i),
        AdmissionDecision::Local {
            additional_bytes: 30
        }
    );
}
#[test]
fn shared_filesystem_reserve() {
    let mut i = input();
    i.available = 49;
    assert_eq!(admit(&i), AdmissionDecision::WaitingCapacity);
    i.available = 50;
    assert!(matches!(admit(&i), AdmissionDecision::Local { .. }));
    i.reserved = u64::MAX;
    assert_eq!(admit(&i), AdmissionDecision::WaitingCapacity);
}
#[test]
fn pin_protects_before_fetch() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let g = c.create_group(false).unwrap();
    let op = OperationId::new();
    let job = c.request_pin(g, op).unwrap();
    assert!(c.protection(g).unwrap().pinned);
    assert_eq!(c.request_pin(g, op).unwrap(), job);
    assert!(!c.readiness(g).unwrap().originals_ready);
}
#[test]
fn unpin_keeps_pending_and_conflict_protection() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let g = c.create_group(false).unwrap();
    c.set_protection(g, true, true).unwrap();
    c.unpin(g).unwrap();
    let p = c.protection(g).unwrap();
    assert!(!p.pinned && p.pending && p.conflict);
}
#[test]
fn oversized_pin_stays_blocked() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let g = c.create_group(false).unwrap();
    let id = c.request_pin(g, OperationId::new()).unwrap();
    c.transition(
        id,
        0,
        JobTransition::Wait(JobState::WaitingCapacity, "oversized".into()),
    )
    .unwrap();
    assert!(c.protection(g).unwrap().pinned);
    assert!(!c.readiness(g).unwrap().originals_ready);
}
#[test]
fn expanded_group_not_ready() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let g = c.create_group(false).unwrap();
    let a = OriginalMember {
        digest: Digest::of(b"a"),
        size: 1,
        path: ArchivePath::parse("RAW/a.ORF").unwrap(),
        role: MediaRole::Raw,
    };
    c.attach_member(
        g,
        a.clone(),
        PairEvidence::UserSelected {
            reason: "test".into(),
        },
    )
    .unwrap();
    c.record_materialization(g, &a, false).unwrap();
    assert!(c.readiness(g).unwrap().originals_ready);
    let b = OriginalMember {
        path: ArchivePath::parse("RAW/b.ORF").unwrap(),
        ..a
    };
    c.attach_member(
        g,
        b,
        PairEvidence::UserSelected {
            reason: "test".into(),
        },
    )
    .unwrap();
    assert!(!c.readiness(g).unwrap().originals_ready);
}
#[test]
fn enospc_after_admission() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let id = c
        .enqueue(DurableRequest {
            operation_id: OperationId::new(),
            kind: JobKind::Clean,
        })
        .unwrap();
    c.reserve(id, FilesystemId(1), 40).unwrap();
    c.transition(id, 0, JobTransition::RequestCancel).unwrap();
    assert_eq!(c.remaining_reservations(FilesystemId(1)).unwrap(), 40);
    assert!(c.transition(id, 0, JobTransition::Complete).is_err());
    c.transition(id, 0, JobTransition::ConfirmCancelled)
        .unwrap();
    assert_eq!(c.remaining_reservations(FilesystemId(1)).unwrap(), 0);
}
