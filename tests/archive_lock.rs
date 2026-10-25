mod support;
use photo_sorter::{
    archive::*,
    domain::*,
    fs::{RootHandle, RootPolicy},
};
fn store(r: &support::TestRig) -> ArchiveStore {
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    let writer = WriterId::new();
    let marker = ArchiveStore::initialize(
        &root,
        InitArchive {
            archive_id: ArchiveId::new(),
            writer_id: writer,
        },
    )
    .unwrap();
    ArchiveStore::open(root, marker.archive_id, writer).unwrap()
}
#[test]
fn expired_lock_not_stolen() {
    let r = support::TestRig::new();
    let s = store(&r);
    let guard = s.acquire_publication().unwrap();
    drop(guard);
    assert!(s.acquire_publication().is_err());
}
#[test]
fn recover_lock_requires_quiescence() {
    let r = support::TestRig::new();
    let mut s = store(&r);
    s.acquire_publication().unwrap();
    assert!(s.recover_lock(false, true).is_err());
    assert!(s.recover_lock(true, false).is_err());
    s.recover_lock(true, true).unwrap();
    assert!(s.acquire_publication().is_ok());
}
#[test]
fn lock_held_across_replay_and_publish() {
    let r = support::TestRig::new();
    let s = store(&r);
    let mut guard = s.acquire_publication().unwrap();
    let batch = s.journal_batch(&mut guard, 128).unwrap();
    s.ack_journal(&mut guard, batch.token).unwrap();
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    let other = ArchiveStore::open(root, s.marker().archive_id, s.marker().writer_id).unwrap();
    assert!(other.acquire_publication().is_err());
    s.release(guard).unwrap();
    assert!(other.acquire_publication().is_ok());
}
#[test]
fn reacquire_requires_new_replay() {
    let r = support::TestRig::new();
    let s = store(&r);
    let mut g = s.acquire_publication().unwrap();
    let batch = s.journal_batch(&mut g, 128).unwrap();
    s.ack_journal(&mut g, batch.token).unwrap();
    assert!(g.replayed());
    s.release(g).unwrap();
    let g = s.acquire_publication().unwrap();
    assert!(!g.replayed());
}
