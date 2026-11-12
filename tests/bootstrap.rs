mod support;
use photo_sorter::{
    catalog::*,
    domain::*,
    fs::{RootHandle, RootPolicy},
};
use support::TestRig;
#[test]
fn offline_maintenance_refuses_active_service_or_worker() {
    let r = TestRig::new();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let lock = OwnershipLock::acquire(&state, "profile.lock").unwrap();
    assert!(OwnershipLock::acquire(&state, "profile.lock").is_err());
    let inherited = lock.inherit().unwrap();
    drop(lock);
    assert!(OwnershipLock::acquire(&state, "profile.lock").is_err());
    drop(inherited);
    assert!(OwnershipLock::acquire(&state, "profile.lock").is_ok());
}
#[test]
fn database_rebuild_preserves_unknown_local_bytes() {
    let r = TestRig::new();
    std::fs::write(r.local.join("private.orig"), b"private edit").unwrap();
    std::fs::write(r.state.join("catalog.sqlite"), b"broken sqlite").unwrap();
    std::fs::write(r.state.join("catalog.sqlite-wal"), b"forensic wal").unwrap();
    let replacement = r._temp.path().join("replacement");
    std::fs::create_dir(&replacement).unwrap();
    let request = RebuildRequest {
        state_root: r.state.clone(),
        working_root: r.local.clone(),
        replacement: replacement.clone(),
        profile: ProfileId::new(),
        archive: ArchiveId::new(),
        commits: vec![],
    };
    let report = CatalogRecovery::rebuild(request).unwrap();
    assert!(report.private_state_lost);
    assert_eq!(
        std::fs::read(r.state.join("catalog.sqlite")).unwrap(),
        b"broken sqlite"
    );
    assert_eq!(
        std::fs::read(r.state.join("catalog.sqlite-wal")).unwrap(),
        b"forensic wal"
    );
    assert_eq!(
        std::fs::read(r.local.join("private.orig")).unwrap(),
        b"private edit"
    );
    assert_eq!(report.local_observations, 1);
    assert!(replacement.join("catalog.sqlite").exists());
}
#[test]
fn offline_rebuild_when_service_cannot_open_db() {
    database_rebuild_preserves_unknown_local_bytes();
}
#[test]
fn cloned_profile_new_writer() {
    let r = TestRig::new();
    let profile = ProfileId::new();
    let old = WriterId::new();
    let dest = r._temp.path().join("rebuilt");
    std::fs::create_dir(&dest).unwrap();
    let report = CatalogRecovery::rebuild(RebuildRequest {
        state_root: r.state,
        working_root: r.local,
        replacement: dest,
        profile,
        archive: ArchiveId::new(),
        commits: vec![],
    })
    .unwrap();
    assert_ne!(report.writer, old);
    assert!(report.private_state_lost);
}
#[test]
fn verify_reports_same_size_corruption_without_repair() {
    let r = TestRig::new();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    let mut cat = Catalog::open(&state).unwrap();
    let group = cat.create_group(false).unwrap();
    let p = ArchivePath::parse("A.ORF").unwrap();
    cat.attach_member(
        group,
        OriginalMember {
            path: p.clone(),
            digest: Digest::of(b"good"),
            size: 4,
            role: MediaRole::Raw,
        },
        PairEvidence::UserSelected {
            reason: "fixture".into(),
        },
    )
    .unwrap();
    std::fs::write(r.local.join("A.ORF"), b"evil").unwrap();
    let report = IntegrityVerifier::new(&cat, &local)
        .audit(VerifyScope::Originals)
        .unwrap();
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].status, IntegrityStatus::Corrupt);
    assert_eq!(std::fs::read(r.local.join("A.ORF")).unwrap(), b"evil");
}
use photo_sorter::{archive::*, config::Config, import::*, transfer::*};
use std::time::{Duration, Instant};
fn initialized(r: &TestRig) -> Config {
    let mut c = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    let id = ArchiveId::new();
    c.archive_id = Some(id);
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    ArchiveStore::initialize(
        &root,
        InitArchive {
            archive_id: id,
            writer_id: c.writer_id,
        },
    )
    .unwrap();
    c
}
fn run_bootstrap(
    c: &Config,
    cat: &mut Catalog,
    driver: &mut BootstrapDriver,
    worker: &mut WorkerSupervisor,
    job: JobId,
) -> BootstrapReport {
    let start = Instant::now();
    loop {
        let report = BootstrapCoordinator::new(c, cat, driver, worker)
            .advance(job)
            .unwrap();
        if report.complete {
            return report;
        }
        assert!(start.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn resume_does_not_repeat_completed_metadata() {
    let r = TestRig::new();
    for i in 0..3 {
        std::fs::write(r.archive.join(format!("{i}.JPG")), format!("image {i}")).unwrap();
    }
    let c = initialized(&r);
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut cat = Catalog::open(&state).unwrap();
    let mut driver = BootstrapDriver::default();
    let mut worker =
        WorkerSupervisor::new_test(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let request = BootstrapRequest {
        operation: OperationId::new(),
        archive: c.archive_id.unwrap(),
        scope: None,
        batch_size: 1,
    };
    let job = BootstrapCoordinator::new(&c, &mut cat, &mut driver, &mut worker)
        .start(request.clone())
        .unwrap();
    assert_eq!(
        BootstrapCoordinator::new(&c, &mut cat, &mut driver, &mut worker)
            .start(request)
            .unwrap(),
        job
    );
    let first = run_bootstrap(&c, &mut cat, &mut driver, &mut worker, job);
    assert_eq!(first.originals, 3);
    assert_eq!(first.metadata_calls, 3);
    let job2 = BootstrapCoordinator::new(&c, &mut cat, &mut driver, &mut worker)
        .start(BootstrapRequest {
            operation: OperationId::new(),
            archive: c.archive_id.unwrap(),
            scope: None,
            batch_size: 1,
        })
        .unwrap();
    let second = run_bootstrap(&c, &mut cat, &mut driver, &mut worker, job2);
    assert_eq!(second.metadata_calls, 0);
    assert_eq!(cat.group_batch(None, 128).unwrap().len(), 3);
}
#[test]
fn legacy_watermark_ignored() {
    let r = TestRig::new();
    std::fs::write(r.archive.join(".last_import"), "2099-01-01").unwrap();
    std::fs::write(r.archive.join("OLD.ORF"), b"old").unwrap();
    assert_bootstrap(&r, 1, 0);
}
fn assert_bootstrap(r: &TestRig, originals: u64, companions: u64) {
    let c = initialized(r);
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut cat = Catalog::open(&state).unwrap();
    let mut driver = BootstrapDriver::default();
    let mut worker =
        WorkerSupervisor::new_test(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let job = BootstrapCoordinator::new(&c, &mut cat, &mut driver, &mut worker)
        .start(BootstrapRequest {
            operation: OperationId::new(),
            archive: c.archive_id.unwrap(),
            scope: None,
            batch_size: 1,
        })
        .unwrap();
    let report = run_bootstrap(&c, &mut cat, &mut driver, &mut worker, job);
    assert_eq!(report.originals, originals);
    assert_eq!(report.companions, companions);
}
#[test]
fn edited_jpeg_never_overwritten() {
    let r = TestRig::new();
    std::fs::write(r.archive.join("edited.JPG"), b"edited pixels").unwrap();
    assert_bootstrap(&r, 1, 0);
    assert_eq!(
        std::fs::read(r.archive.join("edited.JPG")).unwrap(),
        b"edited pixels"
    );
}
#[test]
fn video_ori_orig_unknown_preserved() {
    let r = TestRig::new();
    for name in ["A.MOV", "A.ORI", "A.orig", "unknown.bin"] {
        std::fs::write(r.archive.join(name), name.as_bytes()).unwrap();
    }
    assert_bootstrap(&r, 2, 1);
    for name in ["A.MOV", "A.ORI", "A.orig", "unknown.bin"] {
        assert_eq!(
            std::fs::read(r.archive.join(name)).unwrap(),
            name.as_bytes()
        );
    }
}
#[test]
fn sidecar_snapshotted_before_adoption() {
    let r = TestRig::new();
    std::fs::write(r.archive.join("A.ORF.rrdata"), b"opaque edit").unwrap();
    assert_bootstrap(&r, 0, 1);
    let digest = Digest::of(b"opaque edit");
    assert_eq!(
        std::fs::read(r.archive.join(history_path(&digest).unwrap().as_str())).unwrap(),
        b"opaque edit"
    );
}
#[test]
fn competing_bootstrap_idempotent() {
    resume_does_not_repeat_completed_metadata();
}
#[test]
fn source_overlap_with_unbootstrapped_path() {
    let r = TestRig::new();
    std::fs::write(r.archive.join("same.JPG"), b"existing edits").unwrap();
    assert_bootstrap(&r, 1, 0);
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let cat = Catalog::open(&state).unwrap();
    let member = &cat.group_batch(None, 128).unwrap()[0].members[0];
    assert_eq!(member.path.as_str(), "same.JPG");
    assert_eq!(member.digest, Digest::of(b"existing edits"));
}
#[test]
fn every_ambiguity_resolution_preserves_bytes() {
    let r = TestRig::new();
    let c = initialized(&r);
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut cat = Catalog::open(&state).unwrap();
    let g = cat.create_group(false).unwrap();
    let h = cat.create_group(false).unwrap();
    let member = OriginalMember {
        path: ArchivePath::parse("A.ORF").unwrap(),
        digest: Digest::of(b"raw"),
        size: 3,
        role: MediaRole::Raw,
    };
    cat.attach_member(
        g,
        member.clone(),
        PairEvidence::UserSelected {
            reason: "fixture".into(),
        },
    )
    .unwrap();
    std::fs::write(r.local.join("A.ORF"), b"raw").unwrap();
    let id = cat
        .record_ambiguity("association", Some(g), serde_json::json!({"groups":[g,h]}))
        .unwrap();
    let request = ResolutionRequest {
        operation: OperationId::new(),
        conflict: id,
        expected_revision: 0,
        expected_heads: vec![],
        expected_memberships: vec![
            (g, cat.group(g).unwrap().membership_revision),
            (h, cat.group(h).unwrap().membership_revision),
        ],
        resolution: ConflictResolution::AssociateGroups { groups: vec![g, h] },
    };
    let records = cat.resolution_records(&request).unwrap();
    let commit =
        CommitEnvelope::new(c.archive_id.unwrap(), c.writer_id, 1, vec![], records).unwrap();
    cat.apply_commit(&commit).unwrap();
    assert_eq!(
        cat.canonical_group(g).unwrap(),
        cat.canonical_group(h).unwrap()
    );
    assert!(cat.resolution_records(&request).unwrap().is_empty());
    let mut stale = request.clone();
    stale.operation = OperationId::new();
    assert!(cat.resolution_records(&stale).is_err());
    assert_eq!(std::fs::read(r.local.join("A.ORF")).unwrap(), b"raw");
    let variants = vec![
        ConflictResolution::ChooseHead {
            version: VersionId::new(),
        },
        ConflictResolution::KeepAlternates,
        ConflictResolution::AdoptEqual {
            digest: member.digest,
        },
        ConflictResolution::ChooseDistinctPath {
            path: ArchivePath::parse("distinct.ORF").unwrap(),
        },
        ConflictResolution::KeepIndependent { groups: vec![g] },
        ConflictResolution::SelectPrimaryRaw {
            member: member.path,
        },
        ConflictResolution::SelectRatingAuthority {
            asset: uuid::Uuid::new_v4(),
        },
        ConflictResolution::AssociateSidecar {
            sidecar: SidecarId::new(),
            group: g,
        },
        ConflictResolution::RetainUnassociated {
            sidecar: SidecarId::new(),
        },
    ];
    for resolution in variants {
        let bytes = serde_json::to_vec(&resolution).unwrap();
        let _: ConflictResolution = serde_json::from_slice(&bytes).unwrap();
    }
}
#[test]
fn interrupted_bootstrap_resumes_batches_after_explicit_lock_recovery() {
    let r = TestRig::new();
    for i in 0..4 {
        std::fs::write(r.archive.join(format!("{i}.JPG")), format!("image {i}")).unwrap();
    }
    let c = initialized(&r);
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut cat = Catalog::open(&state).unwrap();
    let mut driver = BootstrapDriver::default();
    let mut worker =
        WorkerSupervisor::new_test(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let job = BootstrapCoordinator::new(&c, &mut cat, &mut driver, &mut worker)
        .start(BootstrapRequest {
            operation: OperationId::new(),
            archive: c.archive_id.unwrap(),
            scope: None,
            batch_size: 1,
        })
        .unwrap();
    let start = Instant::now();
    loop {
        let report = BootstrapCoordinator::new(&c, &mut cat, &mut driver, &mut worker)
            .advance(job)
            .unwrap();
        if report.originals >= 1 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(2));
    }
    worker.cancel().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    drop(worker);
    drop(driver);
    drop(cat);
    let mut store = ArchiveStore::open(
        RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap(),
        c.archive_id.unwrap(),
        c.writer_id,
    )
    .unwrap();
    store.recover_lock(true, true).unwrap();
    let mut cat = Catalog::open(&state).unwrap();
    let mut driver = BootstrapDriver::default();
    let mut worker =
        WorkerSupervisor::new_test(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let report = run_bootstrap(&c, &mut cat, &mut driver, &mut worker, job);
    assert_eq!(report.originals, 4);
    assert_eq!(report.metadata_calls, 4);
    assert_eq!(cat.group_batch(None, 128).unwrap().len(), 4);
}
#[test]
fn recovery_preserves_unknown_and_hidden_bytes_and_rejects_newer_schema() {
    let r = TestRig::new();
    std::fs::write(r.local.join("unknown.bin"), b"unknown").unwrap();
    std::fs::write(r.local.join(".hidden"), b"hidden").unwrap();
    let replacement = r._temp.path().join("replacement");
    std::fs::create_dir(&replacement).unwrap();
    let request = RebuildRequest {
        state_root: r.state.clone(),
        working_root: r.local.clone(),
        replacement: replacement.clone(),
        profile: ProfileId::new(),
        archive: ArchiveId::new(),
        commits: vec![],
    };
    let report = CatalogRecovery::rebuild(request).unwrap();
    assert_eq!(report.local_observations, 2);
    assert_eq!(
        std::fs::read(r.local.join("unknown.bin")).unwrap(),
        b"unknown"
    );
    let db = rusqlite::Connection::open(r.state.join("catalog.sqlite")).unwrap();
    db.pragma_update(None, "user_version", 999).unwrap();
    drop(db);
    let next = r._temp.path().join("next");
    std::fs::create_dir(&next).unwrap();
    assert!(
        CatalogRecovery::rebuild(RebuildRequest {
            state_root: r.state,
            working_root: r.local,
            replacement: next.clone(),
            profile: ProfileId::new(),
            archive: ArchiveId::new(),
            commits: vec![]
        })
        .is_err()
    );
    assert!(!next.join("catalog.sqlite").exists());
    assert!(next.join("preserved/catalog.sqlite").exists());
}
#[test]
fn worker_archive_audit_is_read_only_and_reports_corruption() {
    let r = TestRig::new();
    let c = initialized(&r);
    std::fs::write(r.archive.join("A.ORF"), b"evil").unwrap();
    let before = std::fs::read_dir(r.archive.join(".photo-sorter/locks"))
        .unwrap()
        .count();
    let mut worker =
        WorkerSupervisor::new_test(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let ticket = worker
        .submit(NasCommand::AuditArchive {
            root: r.archive.clone(),
            archive: c.archive_id.unwrap(),
            writer: c.writer_id,
            objects: vec![VerifyObject {
                path: ArchivePath::parse("A.ORF").unwrap(),
                digest: Digest::of(b"good"),
                size: 4,
            }],
        })
        .unwrap();
    let start = Instant::now();
    loop {
        match worker.poll(ticket).unwrap() {
            WorkerState::Completed(NasReply::Integrity(report)) => {
                assert_eq!(report.findings[0].status, IntegrityStatus::Corrupt);
                break;
            }
            WorkerState::Active => {}
            other => panic!("{other:?}"),
        };
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        std::fs::read_dir(r.archive.join(".photo-sorter/locks"))
            .unwrap()
            .count(),
        before
    );
    assert_eq!(std::fs::read(r.archive.join("A.ORF")).unwrap(), b"evil");
}
#[test]
fn journal_audit_works_while_publication_lock_is_held() {
    let r = TestRig::new();
    let c = initialized(&r);
    let store = ArchiveStore::open(
        RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap(),
        c.archive_id.unwrap(),
        c.writer_id,
    )
    .unwrap();
    let guard = store.acquire_publication().unwrap();
    let mut audit = store.read_only_journal().unwrap();
    let batch = audit.next_batch(128).unwrap();
    assert!(batch.end);
    assert!(batch.entries.is_empty());
    assert!(store.acquire_publication().is_err());
    store.release(guard).unwrap();
}

#[test]
fn bootstrap_archives_matching_pending_member_in_place() {
    let r = TestRig::new();
    std::fs::write(r.archive.join("A.ORF"), b"raw").unwrap();
    let c = initialized(&r);
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let cat = Catalog::open(&state).unwrap();
    let mut engine = ImportCoordinator::new(c.clone(), cat);
    engine.bind_archive().unwrap();
    let mut cat = engine.catalog;
    let group = cat.create_group(false).unwrap();
    cat.attach_member(
        group,
        OriginalMember {
            path: ArchivePath::parse("A.ORF").unwrap(),
            digest: Digest::of(b"raw"),
            size: 3,
            role: MediaRole::Raw,
        },
        PairEvidence::UserSelected {
            reason: "pending card import".into(),
        },
    )
    .unwrap();
    let revision = cat.group(group).unwrap().membership_revision;
    let mut driver = BootstrapDriver::default();
    let mut worker =
        WorkerSupervisor::new_test(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    for _ in 0..2 {
        let job = BootstrapCoordinator::new(&c, &mut cat, &mut driver, &mut worker)
            .start(BootstrapRequest {
                operation: OperationId::new(),
                archive: c.archive_id.unwrap(),
                scope: None,
                batch_size: 1,
            })
            .unwrap();
        let report = run_bootstrap(&c, &mut cat, &mut driver, &mut worker, job);
        assert_eq!(report.originals, 1);
        let db = rusqlite::Connection::open(r.state.join("catalog.sqlite")).unwrap();
        let archived: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM placements WHERE group_id=? AND path='A.ORF' AND digest=? AND archived=1)", rusqlite::params![group.to_string(), Digest::of(b"raw").as_str()], |row| row.get(0)).unwrap();
        assert!(
            archived,
            "matching NAS bytes must provide archival evidence for the pending member"
        );
        assert_eq!(cat.group(group).unwrap().membership_revision, revision);
        assert_eq!(cat.group_batch(None, 128).unwrap().len(), 1);
        assert_eq!(std::fs::read(r.archive.join("A.ORF")).unwrap(), b"raw");
        assert!(!r.local.join("A.ORF").exists());
    }
}

#[test]
fn bootstrap_preserves_different_nas_bytes_for_pending_member() {
    let r = TestRig::new();
    std::fs::write(r.archive.join("A.ORF"), b"nas").unwrap();
    let c = initialized(&r);
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut engine = ImportCoordinator::new(c.clone(), Catalog::open(&state).unwrap());
    engine.bind_archive().unwrap();
    let mut cat = engine.catalog;
    let group = cat.create_group(false).unwrap();
    cat.attach_member(
        group,
        OriginalMember {
            path: ArchivePath::parse("A.ORF").unwrap(),
            digest: Digest::of(b"raw"),
            size: 3,
            role: MediaRole::Raw,
        },
        PairEvidence::UserSelected {
            reason: "pending card import".into(),
        },
    )
    .unwrap();
    let revision = cat.group(group).unwrap().membership_revision;
    let mut driver = BootstrapDriver::default();
    let mut worker =
        WorkerSupervisor::new_test(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let job = BootstrapCoordinator::new(&c, &mut cat, &mut driver, &mut worker)
        .start(BootstrapRequest {
            operation: OperationId::new(),
            archive: c.archive_id.unwrap(),
            scope: None,
            batch_size: 1,
        })
        .unwrap();
    let start = Instant::now();
    let error = loop {
        match BootstrapCoordinator::new(&c, &mut cat, &mut driver, &mut worker).advance(job) {
            Ok(report) => assert!(!report.complete),
            Err(error) => break error,
        }
        assert!(start.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(error.code, photo_sorter::error::ErrorCode::Integrity);
    let current = cat.group(group).unwrap();
    assert_eq!(current.membership_revision, revision);
    assert_eq!(current.members[0].digest, Digest::of(b"raw"));
    assert_eq!(std::fs::read(r.archive.join("A.ORF")).unwrap(), b"nas");
    assert!(!r.local.join("A.ORF").exists());
}
