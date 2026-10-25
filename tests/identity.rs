mod support;
use photo_sorter::{
    catalog::*,
    domain::*,
    fs::{RootHandle, RootPolicy},
};
fn member(path: &str, bytes: &[u8]) -> OriginalMember {
    OriginalMember {
        digest: Digest::of(bytes),
        size: bytes.len() as u64,
        path: ArchivePath::parse(path).unwrap(),
        role: MediaRole::Raw,
    }
}
fn evidence() -> PairEvidence {
    PairEvidence::UserSelected {
        reason: "isolated explicit association".into(),
    }
}
#[test]
fn same_placement_reuses_published_group() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let a = c.create_group(true).unwrap();
    let b = c.create_group(false).unwrap();
    c.attach_member(a, member("RAW/a.ORF", b"a"), evidence())
        .unwrap();
    c.attach_member(b, member("RAW/a.ORF", b"a"), evidence())
        .unwrap();
    assert_eq!(c.canonical_group(b).unwrap(), a);
}
#[test]
fn same_bytes_other_placement_stays_distinct() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let a = c.create_group(true).unwrap();
    let b = c.create_group(true).unwrap();
    c.attach_member(a, member("RAW/a.ORF", b"a"), evidence())
        .unwrap();
    c.attach_member(b, member("RAW/b.ORF", b"a"), evidence())
        .unwrap();
    assert_ne!(c.canonical_group(a).unwrap(), c.canonical_group(b).unwrap());
}
#[test]
fn merge_order_independent() {
    for reverse in [false, true] {
        let r = support::TestRig::new();
        let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
        let mut c = Catalog::open(&root).unwrap();
        let mut ids = vec![
            c.create_group(true).unwrap(),
            c.create_group(true).unwrap(),
            c.create_group(false).unwrap(),
        ];
        let want = ids[0].min(ids[1]);
        if reverse {
            ids.reverse();
        }
        assert_eq!(c.merge_groups(&ids, evidence()).unwrap(), want);
        for id in ids {
            assert_eq!(c.canonical_group(id).unwrap(), want);
        }
    }
}
#[test]
fn merge_preserves_pins_versions_conflicts() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let a = c.create_group(true).unwrap();
    let b = c.create_group(true).unwrap();
    c.attach_member(a, member("RAW/a.ORF", b"a"), evidence())
        .unwrap();
    c.attach_member(b, member("RAW/b.ORF", b"b"), evidence())
        .unwrap();
    c.set_protection(b, true, true).unwrap();
    let winner = c.merge_groups(&[a, b], evidence()).unwrap();
    assert_eq!(c.group(winner).unwrap().members.len(), 2);
    let protection = c.protection(winner).unwrap();
    assert!(protection.pinned && protection.conflict && protection.pending);
}
#[test]
fn membership_change_invalidates_readiness() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let id = c.create_group(false).unwrap();
    let revision = c
        .attach_member(id, member("RAW/a.ORF", b"a"), evidence())
        .unwrap();
    let revision2 = c
        .attach_member(id, member("RAW/b.ORF", b"b"), evidence())
        .unwrap();
    assert!(revision2.0 > revision.0);
    assert!(!c.protection(id).unwrap().metadata_current);
}
#[test]
fn aliases_cannot_cycle() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let a = c.create_group(true).unwrap();
    let b = c.create_group(true).unwrap();
    let winner = c.merge_groups(&[a, b], evidence()).unwrap();
    assert_eq!(c.merge_groups(&[b, a], evidence()).unwrap(), winner);
    assert_eq!(c.canonical_group(a).unwrap(), c.canonical_group(b).unwrap());
}
