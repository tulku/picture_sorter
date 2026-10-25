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
