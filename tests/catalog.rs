mod support;
use photo_sorter::{
    catalog::*,
    domain::*,
    fs::{RootHandle, RootPolicy},
};
fn req() -> DurableRequest {
    DurableRequest {
        operation_id: OperationId::new(),
        kind: JobKind::Clean,
    }
}
#[test]
fn enqueue_idempotent() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let request = req();
    let a = c.enqueue(request.clone()).unwrap();
    assert_eq!(a, c.enqueue(request).unwrap());
}
#[test]
fn restart_pending_job() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let id = {
        let mut c = Catalog::open(&root).unwrap();
        c.enqueue(req()).unwrap()
    };
    let c = Catalog::open(&root).unwrap();
    assert_eq!(c.job(id).unwrap().state, JobState::Queued);
}
#[test]
fn obsolete_attempt_cannot_ack() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let id = c.enqueue(req()).unwrap();
    c.transition(id, 0, JobTransition::Start).unwrap();
    assert!(c.transition(id, 0, JobTransition::Complete).is_err());
    assert_eq!(c.job(id).unwrap().state, JobState::Running);
    c.transition(id, 1, JobTransition::RequestCancel).unwrap();
    assert!(c.transition(id, 1, JobTransition::Complete).is_err());
}
#[test]
fn migration_backup_includes_wal() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let id = c.enqueue(req()).unwrap();
    let dest = r.state.join("backup.sqlite");
    c.backup(&dest).unwrap();
    let backup = rusqlite::Connection::open(dest).unwrap();
    let count: i64 = backup
        .query_row(
            "SELECT count(*) FROM jobs WHERE id=?",
            [id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}
#[test]
fn newer_schema_refused() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let db = rusqlite::Connection::open(r.state.join("catalog.sqlite")).unwrap();
    db.pragma_update(None, "user_version", 999).unwrap();
    drop(db);
    let before = std::fs::read(r.state.join("catalog.sqlite")).unwrap();
    assert!(Catalog::open(&root).is_err());
    assert_eq!(
        std::fs::read(r.state.join("catalog.sqlite")).unwrap(),
        before
    );
}
#[test]
fn catalog_wal_stays_beneath_held_root() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let preserved = r.local.join("preserved-state");
    std::fs::rename(&r.state, &preserved).unwrap();
    std::fs::create_dir(&r.state).unwrap();
    let _ = c.enqueue(req());
    assert!(!r.state.join("catalog.sqlite").exists());
    assert!(preserved.join("catalog.sqlite").exists());
}
