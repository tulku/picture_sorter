mod support;
use photo_sorter::{
    catalog::Catalog,
    domain::*,
    fs::{RootHandle, RootPolicy},
    storage::*,
};
#[test]
fn unknown_file_after_db_loss_protected() {
    let r = support::TestRig::new();
    std::fs::write(r.local.join("unknown.ORF"), b"only bytes").unwrap();
    let c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    assert!(EvictionPlanner::candidates(&c, 100).unwrap().is_empty());
    assert_eq!(
        std::fs::read(r.local.join("unknown.ORF")).unwrap(),
        b"only bytes"
    );
}
#[test]
fn pin_or_member_change_cancels_eviction() {
    let r = support::TestRig::new();
    let mut c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    let group = c.create_group(false).unwrap();
    c.request_pin(group, OperationId::new()).unwrap();
    assert!(EvictionPlanner::candidates(&c, 1000).unwrap().is_empty());
}
#[test]
fn metadata_occupancy_can_block() {
    let r = support::TestRig::new();
    std::fs::write(r.local.join("retained.rrdata"), vec![0u8; 100]).unwrap();
    let root = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    let occupied = photo_sorter::fs::occupied_bytes(&root).unwrap();
    assert_eq!(
        admit(&AdmissionInput {
            budget: 100,
            occupied,
            reserved: 0,
            available: 1000,
            reserve: 0,
            required: 1,
            existing: 0,
            partial: 0,
            overhead: 0,
            nas_available: false,
            allow_direct: false
        }),
        AdmissionDecision::WaitingCapacity
    );
    assert!(r.local.join("retained.rrdata").exists());
}
fn archived_fixture(r: &support::TestRig) -> (photo_sorter::import::ImportCoordinator, GroupId) {
    use photo_sorter::{archive::*, config::Config, import::*, transfer::*};
    std::fs::write(r.source.join("a.ORF"), b"raw").unwrap();
    std::fs::write(r.source.join("a.JPG"), b"jpeg").unwrap();
    let mut config = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    config.reserve = 0;
    let archive = ArchiveId::new();
    ArchiveStore::initialize(
        &RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap(),
        InitArchive {
            archive_id: archive,
            writer_id: config.writer_id,
        },
    )
    .unwrap();
    config.archive_id = Some(archive);
    let c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    let mut engine = ImportCoordinator::new(config, c);
    let report = engine
        .prepare(ImportRequest {
            source_path: r.source.clone(),
            strict: false,
            dry_run: false,
            operation_id: OperationId::new(),
        })
        .unwrap();
    let job = report.job.unwrap();
    for _ in 0..3 {
        engine.advance(job).unwrap();
    }
    let mut worker =
        WorkerSupervisor::new_test(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let mut uploads = UploadDriver::default();
    for _ in 0..1000 {
        uploads.advance(&mut engine, &mut worker, job).unwrap();
        if engine.report(job).unwrap().archive_committed == 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let groups = engine.catalog.group_batch(None, 128).unwrap();
    let group = groups[0].id;
    let ids = groups.iter().map(|g| g.id).collect::<Vec<_>>();
    if ids.len() > 1 {
        engine
            .catalog
            .merge_groups(
                &ids,
                photo_sorter::catalog::PairEvidence::UserSelected {
                    reason: "fixture pair".into(),
                },
            )
            .unwrap();
    }
    let group = engine.catalog.canonical_group(group).unwrap();
    engine.catalog.refresh_protection(group).unwrap();
    (engine, group)
}
struct SaveBeforeUnlink(std::path::PathBuf);
impl FaultInjector for SaveBeforeUnlink {
    fn hit(&self, point: FaultPoint) -> photo_sorter::error::AppResult<()> {
        if point == FaultPoint::BeforeUnlink {
            std::fs::write(&self.0, br#"{"rating":5}"#)?;
        }
        Ok(())
    }
}
#[test]
fn late_save_survives_original_eviction() {
    let r = support::TestRig::new();
    let (mut engine, group) = archived_fixture(&r);
    let g = engine.catalog.group(group).unwrap();
    let sidecar = r
        .local
        .join(format!("{}.rrdata", g.members[0].path.as_str()));
    let intent = EvictionPlanner::candidates_with_hold(&engine.catalog, u64::MAX, 0)
        .unwrap()
        .remove(0);
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    let report = Evictor {
        catalog: &mut engine.catalog,
        local: &local,
    }
    .execute(intent, &SaveBeforeUnlink(sidecar.clone()))
    .unwrap();
    assert_eq!(report.removed, 2);
    assert_eq!(std::fs::read(sidecar).unwrap(), br#"{"rating":5}"#);
    for member in g.members {
        assert!(!r.local.join(member.path.as_str()).exists());
        assert!(r.archive.join(member.path.as_str()).exists());
    }
}
#[test]
fn eviction_crash_between_members() {
    let r = support::TestRig::new();
    let (mut engine, group) = archived_fixture(&r);
    let intent = EvictionPlanner::candidates_with_hold(&engine.catalog, u64::MAX, 0)
        .unwrap()
        .remove(0);
    *r.faults.0.lock().unwrap() = Some(FaultPoint::UnlinkMember);
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    assert!(
        Evictor {
            catalog: &mut engine.catalog,
            local: &local
        }
        .execute(intent.clone(), &r.faults)
        .is_err()
    );
    assert!(!engine.catalog.readiness(group).unwrap().originals_ready);
    Evictor {
        catalog: &mut engine.catalog,
        local: &local,
    }
    .execute(intent, &NoFaults)
    .unwrap();
    assert!(!engine.catalog.readiness(group).unwrap().originals_ready);
}
#[test]
fn only_verified_unreferenced_snapshots_collected() {
    let r = support::TestRig::new();
    let mut c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    state
        .mkdir_all(&ArchivePath::parse(".photo-sorter/snapshots").unwrap())
        .unwrap();
    for (bytes, archived) in [
        (b"archived".as_slice(), true),
        (b"pending".as_slice(), false),
    ] {
        let digest = Digest::of(bytes);
        let path = ArchivePath::parse(&format!(
            ".photo-sorter/snapshots/{}",
            &digest.as_str()[7..]
        ))
        .unwrap();
        state.write_new(&path, bytes).unwrap();
        let db = rusqlite::Connection::open(r.state.join("catalog.sqlite")).unwrap();
        db.execute(
            "INSERT INTO snapshots(digest,path,size,archived,refs) VALUES(?,?,?,?,0)",
            rusqlite::params![digest.as_str(), path.as_str(), bytes.len(), archived],
        )
        .unwrap();
    }
    let report = SnapshotGc { state: &state }.collect(&mut c).unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(report.bytes, 8);
}
