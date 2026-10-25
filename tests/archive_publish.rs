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
fn publish(
    s: &ArchiveStore,
    r: &support::TestRig,
    existing: Option<&[u8]>,
) -> photo_sorter::error::AppResult<PublicationReceipt> {
    if let Some(bytes) = existing {
        std::fs::create_dir_all(r.archive.join("RAW")).unwrap();
        std::fs::write(r.archive.join("RAW/a.ORF"), bytes).unwrap();
    }
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    let stage = ArchivePath::parse(".photo-sorter/staging/test").unwrap();
    root.write_new(&stage, b"original").unwrap();
    let group = GroupId::new();
    let member = OriginalMember {
        digest: Digest::of(b"original"),
        size: 8,
        path: ArchivePath::parse("RAW/a.ORF").unwrap(),
        role: MediaRole::Raw,
    };
    let records = vec![
        ArchiveRecord::GroupCreated { id: group },
        ArchiveRecord::MemberAttached {
            group,
            member: member.clone(),
            evidence: photo_sorter::catalog::PairEvidence::UserSelected {
                reason: "test".into(),
            },
        },
    ];
    let commit = CommitEnvelope::new(
        s.marker().archive_id,
        s.marker().writer_id,
        1,
        vec![],
        records,
    )
    .unwrap();
    let mut guard = s.acquire_publication()?;
    let batch = s.journal_batch(&mut guard, 128)?;
    s.ack_journal(&mut guard, batch.token)?;
    let result = s.publish_originals(
        &mut guard,
        PublicationPlan {
            commit,
            staged: vec![StagedMember {
                member,
                staged: stage,
            }],
            attempt: OperationId::new(),
        },
        &NoFaults,
    );
    if result.is_ok() {
        s.release(guard)?;
    }
    result
}
#[test]
fn competing_init_one_identity() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    let a = ArchiveStore::initialize(
        &root,
        InitArchive {
            archive_id: ArchiveId::new(),
            writer_id: WriterId::new(),
        },
    )
    .unwrap();
    assert!(
        ArchiveStore::initialize(
            &root,
            InitArchive {
                archive_id: ArchiveId::new(),
                writer_id: WriterId::new()
            }
        )
        .is_err()
    );
    let bytes = root
        .read(
            &ArchivePath::parse(".photo-sorter/archive.json").unwrap(),
            65536,
        )
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<ArchiveMarker>(&bytes)
            .unwrap()
            .archive_id,
        a.archive_id
    );
}
#[test]
fn missing_marker_not_reinitialized() {
    let r = support::TestRig::new();
    let s = store(&r);
    let id = s.marker().archive_id;
    let writer = s.marker().writer_id;
    drop(s);
    std::fs::remove_file(r.archive.join(".photo-sorter/archive.json")).unwrap();
    assert!(
        ArchiveStore::open(
            RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap(),
            id,
            writer
        )
        .is_err()
    );
    assert!(!r.archive.join(".photo-sorter/archive.json").exists());
}
#[test]
fn same_content_reuses_path() {
    let r = support::TestRig::new();
    let s = store(&r);
    publish(&s, &r, Some(b"original")).unwrap();
    assert_eq!(
        std::fs::read(r.archive.join("RAW/a.ORF")).unwrap(),
        b"original"
    );
}
#[test]
fn different_content_blocks_without_overwrite() {
    let r = support::TestRig::new();
    let s = store(&r);
    assert!(publish(&s, &r, Some(b"edited!!")).is_err());
    assert_eq!(
        std::fs::read(r.archive.join("RAW/a.ORF")).unwrap(),
        b"edited!!"
    );
}
#[test]
fn commit_gap_not_skipped() {
    let r = support::TestRig::new();
    let s = store(&r);
    publish(&s, &r, None).unwrap();
    let mut guard = s.acquire_publication().unwrap();
    let batch = s.journal_batch(&mut guard, 128).unwrap();
    assert_eq!(batch.entries.len(), 1);
    let commit = s.read_commit(&guard, &batch.entries[0].path).unwrap();
    assert_eq!(commit.writer_sequence, 1);
}
#[test]
fn crash_after_rename_before_commit() {
    let r = support::TestRig::new();
    let mut s = store(&r);
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    let staged = ArchivePath::parse(".photo-sorter/staging/crash").unwrap();
    root.write_new(&staged, b"original").unwrap();
    let member = OriginalMember {
        digest: Digest::of(b"original"),
        size: 8,
        path: ArchivePath::parse("RAW/crash.ORF").unwrap(),
        role: MediaRole::Raw,
    };
    let plan = PublicationPlan {
        commit: CommitEnvelope::new(
            s.marker().archive_id,
            s.marker().writer_id,
            1,
            vec![],
            vec![],
        )
        .unwrap(),
        staged: vec![StagedMember { member, staged }],
        attempt: OperationId::new(),
    };
    let mut guard = s.acquire_publication().unwrap();
    let batch = s.journal_batch(&mut guard, 128).unwrap();
    s.ack_journal(&mut guard, batch.token).unwrap();
    *r.faults.0.lock().unwrap() = Some(FaultPoint::OriginalPublished);
    assert!(
        s.publish_originals(&mut guard, plan.clone(), &r.faults)
            .is_err()
    );
    drop(guard);
    s.recover_lock(true, true).unwrap();
    let mut guard = s.acquire_publication().unwrap();
    let batch = s.journal_batch(&mut guard, 128).unwrap();
    s.ack_journal(&mut guard, batch.token).unwrap();
    s.publish_originals(&mut guard, plan, &NoFaults).unwrap();
    assert_eq!(
        std::fs::read(r.archive.join("RAW/crash.ORF")).unwrap(),
        b"original"
    );
}
#[test]
fn fresh_lock_state_canonicalizes_duplicate() {
    let r = support::TestRig::new();
    let s = store(&r);
    publish(&s, &r, None).unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = photo_sorter::catalog::Catalog::open(&state).unwrap();
    let provisional = c.create_group(false).unwrap();
    let mut guard = s.acquire_publication().unwrap();
    let batch = s.journal_batch(&mut guard, 128).unwrap();
    for entry in batch.entries {
        c.apply_commit(&s.read_commit(&guard, &entry.path).unwrap())
            .unwrap();
    }
    s.ack_journal(&mut guard, batch.token).unwrap();
    let member = OriginalMember {
        digest: Digest::of(b"original"),
        size: 8,
        path: ArchivePath::parse("RAW/a.ORF").unwrap(),
        role: MediaRole::Raw,
    };
    c.attach_member(
        provisional,
        member,
        photo_sorter::catalog::PairEvidence::UserSelected {
            reason: "fresh shared placement".into(),
        },
    )
    .unwrap();
    assert_ne!(c.canonical_group(provisional).unwrap(), provisional);
}
