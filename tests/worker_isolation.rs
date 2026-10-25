mod support;
use photo_sorter::{domain::*, transfer::*};
use std::{
    thread,
    time::{Duration, Instant},
};
fn wait(worker: &mut WorkerSupervisor, ticket: WorkerTicket) -> WorkerState {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = worker.poll(ticket).unwrap();
        if !matches!(state, WorkerState::Active) {
            return state;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn hung_nas_does_not_block_local_work() {
    let r = support::TestRig::new();
    let mut w =
        WorkerSupervisor::new_test(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let id = w
        .submit(NasCommand::Inspect {
            root: r.archive.clone(),
        })
        .unwrap();
    assert!(matches!(wait(&mut w, id), WorkerState::Completed(_)));
    unsafe {
        libc::kill(w.pid() as i32, libc::SIGSTOP);
    };
    let id = w
        .submit(NasCommand::Inspect {
            root: r.archive.clone(),
        })
        .unwrap();
    assert!(matches!(w.poll(id).unwrap(), WorkerState::Active));
    std::fs::write(r.local.join("saved.rrdata"), b"saved offline").unwrap();
    assert_eq!(
        std::fs::read(r.local.join("saved.rrdata")).unwrap(),
        b"saved offline"
    );
    w.timeout(Duration::ZERO);
    assert!(matches!(
        w.poll(id).unwrap(),
        WorkerState::TimedOutStillAlive
    ));
}
#[test]
fn timed_out_worker_not_replaced() {
    let r = support::TestRig::new();
    let mut w =
        WorkerSupervisor::new_test(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    unsafe {
        libc::kill(w.pid() as i32, libc::SIGSTOP);
    };
    let id = w
        .submit(NasCommand::Inspect {
            root: r.archive.clone(),
        })
        .unwrap();
    w.timeout(Duration::ZERO);
    let pid = w.pid();
    assert!(
        w.submit(NasCommand::Inspect {
            root: r.archive.clone()
        })
        .is_err()
    );
    assert_eq!(w.pid(), pid);
    assert!(matches!(
        w.poll(id).unwrap(),
        WorkerState::TimedOutStillAlive
    ));
}
#[test]
fn cancel_uncertain_worker_keeps_reservation() {
    let r = support::TestRig::new();
    let root =
        photo_sorter::fs::RootHandle::open(&r.state, photo_sorter::fs::RootPolicy::Local).unwrap();
    let mut c = photo_sorter::catalog::Catalog::open(&root).unwrap();
    let job = c
        .enqueue(photo_sorter::catalog::DurableRequest {
            operation_id: OperationId::new(),
            kind: photo_sorter::catalog::JobKind::Clean,
        })
        .unwrap();
    c.reserve(job, photo_sorter::storage::FilesystemId(1), 14)
        .unwrap();
    c.transition(job, 0, photo_sorter::catalog::JobTransition::RequestCancel)
        .unwrap();
    assert_eq!(
        c.remaining_reservations(photo_sorter::storage::FilesystemId(1))
            .unwrap(),
        14
    );
}
#[test]
fn exact_late_reply_can_confirm_same_worker_after_deadline() {
    let r = support::TestRig::new();
    let mut worker =
        WorkerSupervisor::new_test(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    unsafe {
        libc::kill(worker.pid() as i32, libc::SIGSTOP);
    };
    let ticket = worker
        .submit(NasCommand::Inspect { root: r.archive })
        .unwrap();
    worker.timeout(Duration::ZERO);
    assert!(matches!(
        worker.poll(ticket).unwrap(),
        WorkerState::TimedOutStillAlive
    ));
    let pid = worker.pid();
    unsafe {
        libc::kill(pid as i32, libc::SIGCONT);
    };
    let start = Instant::now();
    loop {
        if matches!(worker.poll(ticket).unwrap(), WorkerState::Completed(_)) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(3));
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(worker.pid(), pid);
    assert!(worker.available().unwrap());
}
#[test]
fn configured_local_worker_refuses_replaced_profile_root_before_writing() {
    let r = support::TestRig::new();
    let config =
        photo_sorter::config::Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    let mut worker = WorkerSupervisor::new_local_configured(
        env!("CARGO_BIN_EXE_photo_sorter").into(),
        vec![],
        &config,
    )
    .unwrap();
    let held = r._temp.path().join("original-working-root");
    std::fs::rename(&r.local, &held).unwrap();
    std::fs::create_dir(&r.local).unwrap();
    std::fs::write(r.local.join("sentinel"), b"foreign directory").unwrap();
    std::fs::write(r.source.join("A.ORF"), b"raw").unwrap();
    let job = JobId::new();
    let ticket = worker
        .submit(NasCommand::LocalTransfer {
            spec: TransferSpec {
                job,
                generation: 1,
                source: SourceLocation {
                    root: r.source.clone(),
                    path: ArchivePath::parse("A.ORF").unwrap(),
                },
                destination_root: r.local.clone(),
                staged_path: staging_path(job, 1),
                expected_digest: Digest::of(b"raw"),
                expected_size: 3,
            },
            destination: ArchivePath::parse("RAW/A.ORF").unwrap(),
        })
        .unwrap();
    assert!(matches!(wait(&mut worker, ticket), WorkerState::Failed(_)));
    assert!(!r.local.join("RAW").exists());
    assert_eq!(
        std::fs::read(r.local.join("sentinel")).unwrap(),
        b"foreign directory"
    );
    assert_eq!(std::fs::read_dir(&held).unwrap().count(), 0);
}
#[test]
fn configured_nas_worker_refuses_changed_state_root_before_archive_staging() {
    use photo_sorter::{archive::*, config::Config, fs::*};
    let r = support::TestRig::new();
    let mut config = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    config.archive_id = Some(ArchiveId::new());
    config.local_test_backend = true;
    ArchiveStore::initialize(
        &RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap(),
        InitArchive {
            archive_id: config.archive_id.unwrap(),
            writer_id: config.writer_id,
        },
    )
    .unwrap();
    let mut worker = WorkerSupervisor::new_configured(
        env!("CARGO_BIN_EXE_photo_sorter").into(),
        vec![],
        &config,
    )
    .unwrap();
    std::fs::rename(&r.state, r._temp.path().join("original-state")).unwrap();
    std::fs::create_dir(&r.state).unwrap();
    std::fs::write(r.source.join("A.ORF"), b"raw").unwrap();
    let job = JobId::new();
    let staged = staging_path(job, 1);
    let ticket = worker
        .submit(NasCommand::StageArchive {
            archive: config.archive_id.unwrap(),
            writer: config.writer_id,
            spec: TransferSpec {
                job,
                generation: 1,
                source: SourceLocation {
                    root: r.source.clone(),
                    path: ArchivePath::parse("A.ORF").unwrap(),
                },
                destination_root: r.archive.clone(),
                staged_path: staged.clone(),
                expected_digest: Digest::of(b"raw"),
                expected_size: 3,
            },
        })
        .unwrap();
    assert!(matches!(wait(&mut worker, ticket), WorkerState::Failed(_)));
    assert!(!r.archive.join(staged.as_str()).exists());
    assert_eq!(std::fs::read(r.source.join("A.ORF")).unwrap(), b"raw");
}
