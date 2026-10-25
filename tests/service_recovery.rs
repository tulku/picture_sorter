mod support;
use photo_sorter::{catalog::*, config::Config, domain::*, fs::*, import::*};
use support::TestRig;
#[test]
fn incomplete_scan_never_claims_ingested_and_resume_retains_selection() {
    let r = TestRig::new();
    for i in 0..3 {
        std::fs::write(r.source.join(format!("{i}.JPG")), format!("{i}")).unwrap();
    }
    let c = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut engine = ImportCoordinator::new(c.clone(), Catalog::open(&state).unwrap());
    let request = ImportRequest {
        source_path: r.source.clone(),
        strict: false,
        dry_run: false,
        operation_id: OperationId::new(),
    };
    let job = engine.begin_scan(&request).unwrap();
    let mut inventory =
        Inventory::open(RootHandle::open(&r.source, RootPolicy::Local).unwrap()).unwrap();
    let entries = inventory.next_batch(1).unwrap();
    engine.accept_scan_batch(job, &entries, None).unwrap();
    assert!(!engine.scan_complete(job).unwrap());
    engine.advance(job).unwrap();
    assert!(!engine.catalog.job(job).unwrap().local_ready);
    drop(engine);
    let mut engine = ImportCoordinator::new(c, Catalog::open(&state).unwrap());
    let report = engine.prepare(request).unwrap();
    assert_eq!(report.selected, 3);
    assert!(engine.scan_complete(job).unwrap());
}
#[test]
fn startup_reconciles_partials_reservations() {
    let r = TestRig::new();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut cat = Catalog::open(&state).unwrap();
    let job = cat
        .enqueue(DurableRequest {
            operation_id: OperationId::new(),
            kind: JobKind::Clean,
        })
        .unwrap();
    cat.reserve(job, photo_sorter::storage::FilesystemId(1), 42)
        .unwrap();
    let report = photo_sorter::service::ServiceRecovery::restore(&mut cat).unwrap();
    assert_eq!(report.retained_reservations, 1);
    assert_eq!(
        cat.remaining_reservations(photo_sorter::storage::FilesystemId(1))
            .unwrap(),
        42
    );
}
#[test]
fn actual_local_transfer_reserves_before_dispatch_and_reconciles_partial() {
    let r = TestRig::new();
    std::fs::write(r.source.join("A.JPG"), b"0123456789").unwrap();
    let c = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut engine = ImportCoordinator::new(c, Catalog::open(&state).unwrap());
    let job = engine
        .prepare(ImportRequest {
            source_path: r.source.clone(),
            strict: true,
            dry_run: false,
            operation_id: OperationId::new(),
        })
        .unwrap()
        .job
        .unwrap();
    let attempt = engine.prepare_local_attempt(job).unwrap().unwrap();
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    assert_eq!(
        engine
            .catalog
            .remaining_reservations(photo_sorter::storage::FilesystemId(local.device()))
            .unwrap(),
        10
    );
    let stage = &attempt.spec.staged_path;
    local
        .mkdir_all(&ArchivePath::parse(stage.as_str().rsplit_once('/').unwrap().0).unwrap())
        .unwrap();
    local.write_new(stage, b"01234").unwrap();
    engine.reconcile_local_reservation(&attempt).unwrap();
    assert_eq!(
        engine
            .catalog
            .remaining_reservations(photo_sorter::storage::FilesystemId(local.device()))
            .unwrap(),
        5
    );
    let receipt = photo_sorter::transfer::TransferEngine::stage(&attempt.spec).unwrap();
    engine.finish_local_attempt(attempt, receipt).unwrap();
    assert_eq!(
        engine
            .catalog
            .remaining_reservations(photo_sorter::storage::FilesystemId(local.device()))
            .unwrap(),
        0
    );
    assert!(engine.catalog.job(job).unwrap().local_ready);
}
#[test]
fn persistent_failure_backoff() {
    use photo_sorter::service::*;
    let config = Config::default();
    let mut scheduler = Scheduler::new(&config);
    let job = JobId::new();
    scheduler.tick(100).unwrap();
    scheduler.handle(ServiceEvent::Failed(job)).unwrap();
    assert!(!scheduler.may_retry(job));
    let (failures, at) = scheduler.retry_state(job).unwrap();
    let mut restored = Scheduler::new(&config);
    restored.restore_retry(job, failures, at);
    restored.tick(100).unwrap();
    assert!(!restored.may_retry(job));
    restored.tick(101).unwrap();
    assert!(restored.may_retry(job));
    restored.handle(ServiceEvent::Failed(job)).unwrap();
    assert_eq!(restored.retry_state(job).unwrap().1, 103);
}
#[test]
fn scheduler_runs_two_sixty_three_hundred_and_hold_policies() {
    use photo_sorter::service::*;
    let mut scheduler = Scheduler::new(&Config::default());
    let first = scheduler.tick(0).unwrap();
    assert!(first.reconcile && first.poll_ratings && first.observe_edits);
    assert_eq!(first.activity_hold_seconds, 600);
    let next = scheduler.tick(1).unwrap();
    assert!(!next.reconcile && !next.poll_ratings && !next.observe_edits);
    assert!(scheduler.tick(2).unwrap().observe_edits);
    assert!(scheduler.tick(60).unwrap().poll_ratings);
    assert!(scheduler.tick(300).unwrap().reconcile);
}
#[test]
fn archive_receipt_during_incomplete_scan_cannot_complete_import() {
    use photo_sorter::{archive::*, transfer::*};
    use std::time::{Duration, Instant};
    let r = TestRig::new();
    std::fs::write(r.source.join("A.JPG"), b"a").unwrap();
    let mut c = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    c.budget = 1;
    c.archive_id = Some(ArchiveId::new());
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    ArchiveStore::initialize(
        &root,
        InitArchive {
            archive_id: c.archive_id.unwrap(),
            writer_id: c.writer_id,
        },
    )
    .unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut engine = ImportCoordinator::new(c, Catalog::open(&state).unwrap());
    let request = ImportRequest {
        source_path: r.source.clone(),
        strict: true,
        dry_run: false,
        operation_id: OperationId::new(),
    };
    let job = engine.begin_scan(&request).unwrap();
    let mut inventory =
        Inventory::open(RootHandle::open(&r.source, RootPolicy::Local).unwrap()).unwrap();
    engine
        .accept_scan_batch(job, &inventory.next_batch(1).unwrap(), None)
        .unwrap();
    let mut worker =
        WorkerSupervisor::new(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let mut uploader = UploadDriver::default();
    let begin = Instant::now();
    loop {
        let report = uploader.advance(&mut engine, &mut worker, job).unwrap();
        if report.archive_committed == 1 {
            break;
        }
        assert!(begin.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(2));
    }
    let record = engine.catalog.job(job).unwrap();
    assert!(!record.archive_committed && !record.local_ready);
}
#[test]
fn local_observation_worker_retains_exact_saved_snapshot() {
    use photo_sorter::{edit::*, transfer::*};
    use std::time::{Duration, Instant};
    let r = TestRig::new();
    let bytes = br#"{"version":1,"rating":4,"adjustments":{"exposure":2}}"#;
    std::fs::write(r.local.join("A.ORF.rrdata"), bytes).unwrap();
    let mut worker =
        WorkerSupervisor::new_local(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let ticket = worker
        .submit(NasCommand::CaptureLocal {
            local: r.local.clone(),
            state: r.state.clone(),
            path: ArchivePath::parse("A.ORF.rrdata").unwrap(),
            kind: SidecarKind::RapidRaw,
            ceiling: 64 * 1024 * 1024,
        })
        .unwrap();
    let begin = Instant::now();
    loop {
        match worker.poll(ticket).unwrap() {
            WorkerState::Completed(NasReply::Captured(receipt)) => {
                assert_eq!(
                    receipt.body,
                    VersionBody::Content {
                        digest: Digest::of(bytes),
                        size: bytes.len() as u64
                    }
                );
                let path = receipt.snapshot.unwrap();
                assert_eq!(std::fs::read(r.state.join(path.as_str())).unwrap(), bytes);
                break;
            }
            WorkerState::Active => {}
            other => panic!("{other:?}"),
        };
        assert!(begin.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn independent_clients_choose_same_sidecar_identity_for_bound_archive() {
    let a = TestRig::new();
    let b = TestRig::new();
    let mut ca = Catalog::open(&RootHandle::open(&a.state, RootPolicy::Local).unwrap()).unwrap();
    let mut cb = Catalog::open(&RootHandle::open(&b.state, RootPolicy::Local).unwrap()).unwrap();
    let archive = ArchiveId::new();
    let envelope =
        photo_sorter::archive::CommitEnvelope::new(archive, WriterId::new(), 1, vec![], vec![])
            .unwrap();
    ca.apply_commit(&envelope).unwrap();
    cb.apply_commit(&envelope).unwrap();
    let path = ArchivePath::parse("RAW/A.ORF.rrdata").unwrap();
    assert_eq!(
        ca.managed_sidecar_id(&path).unwrap(),
        cb.managed_sidecar_id(&path).unwrap()
    );
}
#[test]
fn same_length_corrupt_local_original_cannot_satisfy_repeat_ingestion() {
    let r = TestRig::new();
    std::fs::write(r.source.join("A.ORF"), b"photo").unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    c.reserve = 0;
    let mut engine = ImportCoordinator::new(c, Catalog::open(&state).unwrap());
    let first = engine
        .prepare(ImportRequest {
            source_path: r.source.clone(),
            strict: false,
            dry_run: false,
            operation_id: OperationId::new(),
        })
        .unwrap()
        .job
        .unwrap();
    engine.advance(first).unwrap();
    let group = engine
        .catalog
        .group_batch(None, 128)
        .unwrap()
        .into_iter()
        .find(|g| !g.members.is_empty())
        .unwrap();
    std::fs::write(r.local.join(group.members[0].path.as_str()), b"other").unwrap();
    let second = engine
        .prepare(ImportRequest {
            source_path: r.source.clone(),
            strict: false,
            dry_run: false,
            operation_id: OperationId::new(),
        })
        .unwrap()
        .job
        .unwrap();
    let report = engine.advance(second).unwrap();
    assert_eq!(report.local_ready, 0);
    assert_eq!(report.conflicting, 1);
    assert_eq!(std::fs::read(r.source.join("A.ORF")).unwrap(), b"photo");
    assert_eq!(
        std::fs::read(r.local.join(group.members[0].path.as_str())).unwrap(),
        b"other"
    );
}
#[test]
fn durable_backoff_and_job_pages_survive_restart() {
    let r = TestRig::new();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut cat = Catalog::open(&state).unwrap();
    let mut ids = vec![];
    for _ in 0..260 {
        ids.push(
            cat.enqueue(DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::Clean,
            })
            .unwrap(),
        );
    }
    cat.defer_job(ids[0], 100, "fixture unavailable").unwrap();
    assert!(!cat.job_is_due(ids[0], 100).unwrap());
    drop(cat);
    let cat = Catalog::open(&state).unwrap();
    assert!(!cat.job_is_due(ids[0], 100).unwrap());
    assert!(cat.job_is_due(ids[0], 101).unwrap());
    let first = cat.jobs_page(None, 128, true).unwrap();
    assert_eq!(first.len(), 128);
    let next = cat
        .jobs_page(first.last().map(|j| j.id), 128, true)
        .unwrap();
    assert_eq!(next.len(), 128);
    assert!(first.iter().all(|a| next.iter().all(|b| a.id != b.id)));
}
#[test]
fn original_publication_carries_cached_metadata_to_fresh_client() {
    use photo_sorter::{archive::*, transfer::*};
    use std::time::{Duration, Instant};
    let r = TestRig::new();
    std::fs::write(r.source.join("A.JPG"), b"jpeg").unwrap();
    let mut config = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    config.archive_id = Some(ArchiveId::new());
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    ArchiveStore::initialize(
        &root,
        InitArchive {
            archive_id: config.archive_id.unwrap(),
            writer_id: config.writer_id,
        },
    )
    .unwrap();
    let mut engine = ImportCoordinator::new(
        config.clone(),
        Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap(),
    );
    let job = engine
        .prepare(ImportRequest {
            source_path: r.source,
            strict: true,
            dry_run: false,
            operation_id: OperationId::new(),
        })
        .unwrap()
        .job
        .unwrap();
    let mut worker =
        WorkerSupervisor::new(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let mut driver = UploadDriver::default();
    let begin = Instant::now();
    loop {
        if driver
            .advance(&mut engine, &mut worker, job)
            .unwrap()
            .archive_committed
            == 1
        {
            break;
        }
        assert!(begin.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(2));
    }
    let other = tempfile::tempdir().unwrap();
    let mut target =
        Catalog::open(&RootHandle::open(other.path(), RootPolicy::Local).unwrap()).unwrap();
    let store = ArchiveStore::open(root, config.archive_id.unwrap(), config.writer_id).unwrap();
    let mut journal = store.read_only_journal().unwrap();
    loop {
        let batch = journal.next_batch(128).unwrap();
        for entry in batch.entries {
            target
                .apply_commit(&journal.read(&entry.path).unwrap())
                .unwrap();
        }
        if batch.end {
            break;
        }
    }
    assert!(target.metadata(&Digest::of(b"jpeg")).unwrap().is_some());
}
#[test]
fn interrupted_capture_has_owned_bytes_and_recovers_after_live_overwrite() {
    use photo_sorter::{edit::*, transfer::*};
    let r = TestRig::new();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let path = ArchivePath::parse("A.ORF.rrdata").unwrap();
    std::fs::write(r.local.join(path.as_str()), b"first saved edit").unwrap();
    let context = CaptureContext {
        operation: OperationId::new(),
        owner: SidecarOwner {
            id: SidecarId::new(),
            group: None,
            path: path.clone(),
            kind: SidecarKind::Opaque,
            virtual_copy: None,
        },
        previous: None,
    };
    context.persist(&state).unwrap();
    let mut worker =
        WorkerSupervisor::new_local(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let ticket = worker
        .submit(NasCommand::CaptureOwned {
            local: r.local.clone(),
            state: r.state.clone(),
            context: context.clone(),
            ceiling: 1024,
        })
        .unwrap();
    loop {
        match worker.poll(ticket).unwrap() {
            WorkerState::Completed(NasReply::Captured(_)) => break,
            WorkerState::Active => std::thread::sleep(std::time::Duration::from_millis(10)),
            other => panic!("{other:?}"),
        }
    }
    drop(worker);
    std::fs::write(r.local.join(path.as_str()), b"later saved edit").unwrap();
    let mut worker =
        WorkerSupervisor::new_local(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let ticket = worker
        .submit(NasCommand::RecoverCapture {
            state: r.state.clone(),
            context: context.clone(),
            ceiling: 1024,
        })
        .unwrap();
    let receipt = loop {
        match worker.poll(ticket).unwrap() {
            WorkerState::Completed(NasReply::Captured(r)) => break r,
            WorkerState::Active => std::thread::sleep(std::time::Duration::from_millis(10)),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(receipt.path, path);
    assert_eq!(
        receipt.body,
        VersionBody::Content {
            digest: Digest::of(b"first saved edit"),
            size: 16
        }
    );
    assert_eq!(
        std::fs::read(r.local.join(path.as_str())).unwrap(),
        b"later saved edit"
    );
}
#[test]
fn startup_invalidates_physical_readiness_until_local_worker_checks_bytes() {
    let r = TestRig::new();
    std::fs::write(r.source.join("A.JPG"), b"jpeg").unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut engine = ImportCoordinator::new(
        Config::new(r.local.clone(), r.state.clone(), r.archive.clone()),
        Catalog::open(&state).unwrap(),
    );
    let report = engine
        .prepare(ImportRequest {
            source_path: r.source.clone(),
            strict: true,
            dry_run: false,
            operation_id: OperationId::new(),
        })
        .unwrap();
    engine.advance(report.job.unwrap()).unwrap();
    let group = engine.catalog.group_batch(None, 1).unwrap()[0].id;
    assert!(engine.catalog.readiness(group).unwrap().originals_ready);
    photo_sorter::service::ServiceRecovery::restore(&mut engine.catalog).unwrap();
    assert!(!engine.catalog.readiness(group).unwrap().originals_ready);
}
