mod support;
use photo_sorter::{
    catalog::*,
    domain::*,
    edit::*,
    fs::{RootHandle, RootPolicy},
};
use std::fs;
fn setup(r: &support::TestRig) -> (Catalog, SidecarOwner) {
    let mut c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    let owner = SidecarOwner {
        id: SidecarId::new(),
        group: None,
        path: ArchivePath::parse("RAW/a.ORF.rrdata").unwrap(),
        kind: SidecarKind::RapidRaw,
        virtual_copy: None,
    };
    fs::create_dir(r.local.join("RAW")).unwrap();
    c.register_sidecar(owner.id, None, owner.path.clone(), "rapidraw-base")
        .unwrap();
    (c, owner)
}
fn capture(r: &support::TestRig, c: &mut Catalog, owner: &SidecarOwner) -> SnapshotObservation {
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    Snapshotter::new(&local, &state, c, &r.clock, 2, 64 * 1024 * 1024)
        .capture(owner)
        .unwrap()
}
#[test]
fn atomic_replace_save_detected() {
    let r = support::TestRig::new();
    let (mut c, o) = setup(&r);
    fs::write(r.local.join(o.path.as_str()), b"{\"rating\":3}").unwrap();
    capture(&r, &mut c, &o);
    let previous = c.local_head(o.id).unwrap();
    fs::write(r.local.join("RAW/temp"), b"{\"rating\":5}").unwrap();
    fs::rename(r.local.join("RAW/temp"), r.local.join(o.path.as_str())).unwrap();
    capture(&r, &mut c, &o);
    assert_ne!(c.local_head(o.id).unwrap(), previous);
}
#[test]
fn same_size_mtime_change_detected() {
    let r = support::TestRig::new();
    let (mut c, o) = setup(&r);
    let path = r.local.join(o.path.as_str());
    fs::write(&path, b"{\"rating\":3}").unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    capture(&r, &mut c, &o);
    let previous = c.local_head(o.id).unwrap();
    fs::write(&path, b"{\"rating\":5}").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    capture(&r, &mut c, &o);
    assert_ne!(c.local_head(o.id).unwrap(), previous);
}
#[test]
fn startup_recovers_saved_edit() {
    let r = support::TestRig::new();
    let (mut c, o) = setup(&r);
    fs::write(r.local.join(o.path.as_str()), b"{\"rating\":3}").unwrap();
    assert!(matches!(
        capture(&r, &mut c, &o),
        SnapshotObservation::StableContent { changed: true, .. }
    ));
    drop(c);
    let mut c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    assert!(c.snapshot_dirty(o.id).unwrap());
}
#[test]
fn overflow_scan_finds_edit() {
    let r = support::TestRig::new();
    let (mut c, o) = setup(&r);
    fs::write(r.local.join(o.path.as_str()), b"{\"rating\":3}").unwrap();
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut tracker = EditTracker::new(&local, &state, &mut c, &r.clock);
    tracker.observe(LocalEvent::RescanRequired).unwrap();
    assert_eq!(tracker.reconcile(ReconcileScope::All).unwrap().captured, 1);
}
#[test]
fn unreadable_parent_not_reset() {
    let r = support::TestRig::new();
    let (mut c, o) = setup(&r);
    fs::write(r.local.join(o.path.as_str()), b"{}").unwrap();
    capture(&r, &mut c, &o);
    fs::rename(r.local.join("RAW"), r.local.join("retained")).unwrap();
    r.clock.advance(10);
    assert!(matches!(
        capture(&r, &mut c, &o),
        SnapshotObservation::Unreadable { .. }
    ));
    assert!(matches!(
        c.version(c.local_head(o.id).unwrap().unwrap())
            .unwrap()
            .body,
        VersionBody::Content { .. }
    ));
}
#[test]
fn root_disappearance_not_mass_reset() {
    let r = support::TestRig::new();
    let (mut c, o) = setup(&r);
    fs::write(r.local.join(o.path.as_str()), b"{}").unwrap();
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    Snapshotter::new(&local, &state, &mut c, &r.clock, 2, 100)
        .capture(&o)
        .unwrap();
    fs::rename(&r.local, r.source.join("retained-root")).unwrap();
    assert!(matches!(
        Snapshotter::new(&local, &state, &mut c, &r.clock, 2, 100)
            .capture(&o)
            .unwrap(),
        SnapshotObservation::Unreadable { .. }
    ));
}
#[test]
fn temporary_absence_not_reset() {
    let r = support::TestRig::new();
    let (mut c, o) = setup(&r);
    let path = r.local.join(o.path.as_str());
    fs::write(&path, b"{}").unwrap();
    capture(&r, &mut c, &o);
    fs::remove_file(&path).unwrap();
    assert!(matches!(
        capture(&r, &mut c, &o),
        SnapshotObservation::Retry
    ));
    fs::write(&path, b"{}").unwrap();
    r.clock.advance(3);
    assert!(matches!(
        capture(&r, &mut c, &o),
        SnapshotObservation::StableContent { .. }
    ));
}
#[test]
fn stable_delete_becomes_versioned_reset() {
    let r = support::TestRig::new();
    let (mut c, o) = setup(&r);
    let path = r.local.join(o.path.as_str());
    fs::write(&path, b"{}").unwrap();
    capture(&r, &mut c, &o);
    let old = c.local_head(o.id).unwrap().unwrap();
    fs::remove_file(&path).unwrap();
    capture(&r, &mut c, &o);
    r.clock.advance(3);
    assert!(matches!(
        capture(&r, &mut c, &o),
        SnapshotObservation::StableAbsent { .. }
    ));
    let new = c.local_head(o.id).unwrap().unwrap();
    assert_eq!(c.version(new).unwrap().body, VersionBody::Reset);
    assert_eq!(c.version(new).unwrap().parents, vec![old]);
}
#[test]
fn malformed_json_protected_opaque() {
    let r = support::TestRig::new();
    let (mut c, o) = setup(&r);
    fs::write(r.local.join(o.path.as_str()), b"{bad").unwrap();
    assert!(matches!(
        capture(&r, &mut c, &o),
        SnapshotObservation::OpaqueInvalid { .. }
    ));
    assert!(c.snapshot_dirty(o.id).unwrap());
}
#[test]
fn virtual_copy_longest_owner_match() {
    let members = ["RAW/a.ORF", "RAW/a.one.ORF"].map(|p| OriginalMember {
        path: ArchivePath::parse(p).unwrap(),
        digest: Digest::of(p.as_bytes()),
        size: 1,
        role: MediaRole::Raw,
    });
    match resolve_owner(
        &ArchivePath::parse("RAW/a.one.ORF.abcdef.rrdata").unwrap(),
        &members,
    ) {
        OwnerDecision::Owned {
            member,
            virtual_copy,
            ..
        } => {
            assert_eq!(member.as_str(), "RAW/a.one.ORF");
            assert_eq!(virtual_copy.as_deref(), Some("abcdef"));
        }
        _ => panic!("longest exact owner required"),
    }
}
#[test]
fn binary_companion_not_json() {
    let r = support::TestRig::new();
    let (mut c, mut o) = setup(&r);
    o.kind = SidecarKind::Rrexif;
    fs::write(r.local.join(o.path.as_str()), [0u8, 255]).unwrap();
    assert!(matches!(
        capture(&r, &mut c, &o),
        SnapshotObservation::StableContent { .. }
    ));
}
#[test]
fn newly_created_sidecar_discovered_without_existing_catalog_row() {
    let r = support::TestRig::new();
    let mut c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    let group = c.create_group(false).unwrap();
    c.attach_member(
        group,
        OriginalMember {
            path: ArchivePath::parse("RAW/a.ORF").unwrap(),
            digest: Digest::of(b"raw"),
            size: 3,
            role: MediaRole::Raw,
        },
        PairEvidence::UserSelected {
            reason: "fixture".into(),
        },
    )
    .unwrap();
    fs::create_dir(r.local.join("RAW")).unwrap();
    fs::write(r.local.join("RAW/a.ORF.rrdata"), b"{\"rating\":3}").unwrap();
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut tracker = EditTracker::new(&local, &state, &mut c, &r.clock);
    assert_eq!(tracker.reconcile(ReconcileScope::All).unwrap().captured, 1);
}
#[test]
fn real_inotify_reports_local_save() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    let mut watcher = LocalWatcher::open(&root).unwrap();
    fs::write(r.local.join("saved.rrdata"), b"{}").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if !watcher.poll().unwrap().is_empty() {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
#[test]
fn oversized_opaque_companion_streamed_without_json_transformation() {
    let r = support::TestRig::new();
    let (mut c, mut owner) = setup(&r);
    owner.kind = SidecarKind::Opaque;
    fs::write(r.local.join(owner.path.as_str()), vec![13u8; 1024]).unwrap();
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    assert!(matches!(
        Snapshotter::new(&local, &state, &mut c, &r.clock, 2, 16)
            .capture(&owner)
            .unwrap(),
        SnapshotObservation::StableContent { .. }
    ));
    let version = c.version(c.local_head(owner.id).unwrap().unwrap()).unwrap();
    assert_eq!(
        version.body,
        VersionBody::Content {
            digest: Digest::of(&vec![13u8; 1024]),
            size: 1024
        }
    );
}
