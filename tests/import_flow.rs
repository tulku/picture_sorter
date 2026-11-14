mod support;
use photo_sorter::{
    catalog::Catalog,
    config::Config,
    domain::*,
    fs::{RootHandle, RootPolicy},
    import::*,
};
fn engine(r: &support::TestRig) -> ImportCoordinator {
    let mut config = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    config.reserve = 0;
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    ImportCoordinator::new(config, Catalog::open(&root).unwrap())
}
fn request(r: &support::TestRig) -> ImportRequest {
    ImportRequest {
        source_path: r.source.clone(),
        strict: false,
        dry_run: false,
        operation_id: OperationId::new(),
    }
}
fn drive(e: &mut ImportCoordinator, job: JobId) -> ImportReport {
    for _ in 0..30 {
        let report = e.advance(job).unwrap();
        if report.local_ready + report.archive_committed + report.conflicting >= report.selected
            && report.selected > 0
        {
            return report;
        }
    }
    e.report(job).unwrap()
}
#[test]
fn repeat_zero_exif_zero_copy() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("a.ORF"), b"photo").unwrap();
    let mut e = engine(&r);
    let initial = e.prepare(request(&r)).unwrap();
    drive(&mut e, initial.job.unwrap());
    let report = e.prepare(request(&r)).unwrap();
    assert_eq!(report.selected, 1);
    assert_eq!(report.recognized, 1);
    assert_eq!(report.pending_archive, 1);
    let value = serde_json::to_value(&report).unwrap();
    assert_eq!(value["archive_reused"], 0);
    let pending_work =
        report.selected - value["archive_reused"].as_u64().unwrap() - report.image_duplicates;
    assert_eq!(pending_work, 1);
    let final_report = drive(&mut e, report.job.unwrap());
    assert_eq!(final_report.exif_invocations, 0);
    assert_eq!(final_report.transfer_bytes, 0);
    assert_eq!(final_report.local_ready, 1);
    assert_eq!(final_report.copy_ready, 1);
}
#[test]
fn offline_capacity_pause_then_resume() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("a.ORF"), b"photo").unwrap();
    let mut e = engine(&r);
    e.config.budget = 1;
    let report = e.prepare(request(&r)).unwrap();
    let progress = e.advance(report.job.unwrap()).unwrap();
    assert_eq!(progress.waiting, 1);
    assert_eq!(std::fs::read(r.source.join("a.ORF")).unwrap(), b"photo");
    e.config.budget = 10_000_000;
    assert_eq!(drive(&mut e, report.job.unwrap()).local_ready, 1);
}
#[test]
fn card_swap_cannot_resume_wrong_file() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("a.ORF"), b"photo").unwrap();
    let mut e = engine(&r);
    let report = e.prepare(request(&r)).unwrap();
    std::fs::write(r.source.join("a.ORF"), b"other").unwrap();
    let after = e.advance(report.job.unwrap()).unwrap();
    assert_eq!(after.local_ready, 0);
    assert_eq!(after.waiting, 1);
}
#[test]
fn dry_run_changes_no_state() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("a.ORF"), b"photo").unwrap();
    let mut e = engine(&r);
    let before = e.catalog.jobs().unwrap().len();
    let mut req = request(&r);
    req.dry_run = true;
    let report = e.prepare(req).unwrap();
    assert!(report.job.is_none());
    assert_eq!(e.catalog.jobs().unwrap().len(), before);
    assert!(!r.local.join(".photo-sorter").exists());
}
#[test]
fn source_rrdata_rrexif_and_full_size_orig_preserved() {
    let r = support::TestRig::new();
    for (name, bytes) in [
        ("a.ORF", b"photo".as_slice()),
        ("a.ORF.rrdata", b"{\"rating\":3}"),
        ("a.ORF.rrexif", b"\x00\xffbinary"),
        ("a.ORF.orig", b"full original"),
    ] {
        std::fs::write(r.source.join(name), bytes).unwrap();
    }
    let mut e = engine(&r);
    let report = e.prepare(request(&r)).unwrap();
    let final_report = drive(&mut e, report.job.unwrap());
    assert_eq!(final_report.local_ready, 4);
    assert!(final_report.pending_archive >= 4);
}
#[test]
fn mutable_source_companion_changed_on_repeat_import() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("a.ORF.rrdata"), b"{\"rating\":3}").unwrap();
    let mut e = engine(&r);
    let report = e.prepare(request(&r)).unwrap();
    drive(&mut e, report.job.unwrap());
    std::fs::write(r.source.join("a.ORF.rrdata"), b"{\"rating\":5}").unwrap();
    let report = e.prepare(request(&r)).unwrap();
    let after = drive(&mut e, report.job.unwrap());
    assert_eq!(after.local_ready, 1);
    assert!(after.transfer_bytes > 0);
}
#[test]
fn orphan_companion_preserved_until_resolution() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("orphan.rrexif"), b"opaque").unwrap();
    let mut e = engine(&r);
    let report = e.prepare(request(&r)).unwrap();
    let after = drive(&mut e, report.job.unwrap());
    assert_eq!(after.local_ready, 1);
    assert!(after.association_warnings > 0);
}
#[test]
fn larger_than_budget_direct_nas() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("large.ORF"), vec![7u8; 4096]).unwrap();
    let mut e = engine(&r);
    e.config.budget = 1;
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    let archive = ArchiveId::new();
    photo_sorter::archive::ArchiveStore::initialize(
        &root,
        photo_sorter::archive::InitArchive {
            archive_id: archive,
            writer_id: e.config.writer_id,
        },
    )
    .unwrap();
    e.config.archive_id = Some(archive);
    let report = e.prepare(request(&r)).unwrap();
    let mut worker = photo_sorter::transfer::WorkerSupervisor::new_test(
        env!("CARGO_BIN_EXE_photo_sorter").into(),
        vec![],
    )
    .unwrap();
    let mut uploads = UploadDriver::default();
    for _ in 0..1000 {
        uploads
            .advance(&mut e, &mut worker, report.job.unwrap())
            .unwrap();
        if e.report(report.job.unwrap()).unwrap().archive_committed == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let after = e.report(report.job.unwrap()).unwrap();
    assert_eq!(after.archive_committed, 1);
    assert_eq!(after.local_ready, 0);
    assert!(std::fs::read_dir(&r.local).unwrap().next().is_none());
}
#[test]
fn overlapping_cards_both_orders_keep_unique_old_photo() {
    for reverse in [false, true] {
        let r = support::TestRig::new();
        let mut e = engine(&r);
        let names = if reverse {
            ["old.ORF", "new.ORF"]
        } else {
            ["new.ORF", "old.ORF"]
        };
        for name in names {
            std::fs::write(r.source.join(name), name.as_bytes()).unwrap();
            let report = e.prepare(request(&r)).unwrap();
            drive(&mut e, report.job.unwrap());
        }
        let report = e.prepare(request(&r)).unwrap();
        let after = drive(&mut e, report.job.unwrap());
        assert_eq!(after.local_ready, 2);
        assert_eq!(after.transfer_bytes, 0);
        assert_eq!(after.exif_invocations, 0);
    }
}
#[test]
fn conflict_group_does_not_block_unrelated() {
    let r = support::TestRig::new();
    let mut e = engine(&r);
    std::fs::write(r.source.join("a.ORF"), b"first").unwrap();
    let report = e.prepare(request(&r)).unwrap();
    drive(&mut e, report.job.unwrap());
    std::fs::write(r.source.join("a.ORF"), b"other").unwrap();
    std::fs::write(r.source.join("b.ORF"), b"unique").unwrap();
    let report = e.prepare(request(&r)).unwrap();
    let after = drive(&mut e, report.job.unwrap());
    assert_eq!(after.conflicting, 1);
    assert_eq!(after.local_ready, 1);
}
#[test]
fn evicted_known_original_not_reimported() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("a.ORF"), b"photo").unwrap();
    let mut e = engine(&r);
    let archive = ArchiveId::new();
    photo_sorter::archive::ArchiveStore::initialize(
        &RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap(),
        photo_sorter::archive::InitArchive {
            archive_id: archive,
            writer_id: e.config.writer_id,
        },
    )
    .unwrap();
    e.config.archive_id = Some(archive);
    let initial = e.prepare(request(&r)).unwrap();
    drive(&mut e, initial.job.unwrap());
    let mut worker = photo_sorter::transfer::WorkerSupervisor::new_test(
        env!("CARGO_BIN_EXE_photo_sorter").into(),
        vec![],
    )
    .unwrap();
    let mut uploads = UploadDriver::default();
    for _ in 0..1000 {
        uploads
            .advance(&mut e, &mut worker, initial.job.unwrap())
            .unwrap();
        if e.report(initial.job.unwrap()).unwrap().archive_committed == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let uploaded = e.report(initial.job.unwrap()).unwrap();
    assert_eq!(uploaded.archive_committed, 1);
    assert_eq!(
        serde_json::to_value(&uploaded).unwrap()["archive_reused"],
        0
    );
    let group = e.catalog.group_batch(None, 128).unwrap().remove(0);
    std::fs::remove_file(r.local.join(group.members[0].path.as_str())).unwrap();
    let report = e.prepare(request(&r)).unwrap();
    assert_eq!(report.archive_committed, 1);
    assert_eq!(serde_json::to_value(&report).unwrap()["archive_reused"], 1);
    assert_eq!(report.local_ready, 0);
    assert_eq!(report.transfer_bytes, 0);
    assert_eq!(report.exif_invocations, 0);
}
struct SyntheticCamera;
impl MetadataExtractor for SyntheticCamera {
    fn extract(
        &self,
        entries: &[SourceEntry],
    ) -> photo_sorter::error::AppResult<Vec<MetadataResult>> {
        entries.iter().map(|e|Ok(MetadataResult::from_json(hash_source(e)?,serde_json::json!({"DateTimeOriginal":"2026:05:17 13:58:51","Make":"fixture","Model":"fixture-camera"})))).collect()
    }
}
#[test]
fn split_pair_expands_pin() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("a.ORF"), b"raw").unwrap();
    let mut e = engine(&r).with_extractor(Box::new(SyntheticCamera));
    let report = e.prepare(request(&r)).unwrap();
    drive(&mut e, report.job.unwrap());
    let group = e.catalog.group_batch(None, 128).unwrap().remove(0);
    e.catalog.request_pin(group.id, OperationId::new()).unwrap();
    let old = e.catalog.readiness(group.id).unwrap().revision;
    std::fs::write(r.source.join("a.JPG"), b"jpeg").unwrap();
    e.prepare(request(&r)).unwrap();
    let current = e.catalog.group(group.id).unwrap();
    assert_eq!(current.members.len(), 2);
    assert!(current.membership_revision.0 > old.0);
    assert!(e.catalog.protection(group.id).unwrap().pinned);
    assert!(!e.catalog.readiness(group.id).unwrap().originals_ready);
}

#[test]
fn native_import_preserves_ambiguous_raws_as_independent_groups() {
    let r = support::TestRig::new();
    for (name, body) in [("a.ORF", b"raw-a"), ("a.NEF", b"raw-b")] {
        std::fs::write(r.source.join(name), body).unwrap();
    }
    let mut e = engine(&r).with_extractor(Box::new(SyntheticCamera));
    e.prepare(request(&r)).unwrap();
    let groups = e.catalog.group_batch(None, 128).unwrap();
    assert_eq!(groups.len(), 2);
    assert!(groups.iter().all(|g| g.members.len() == 1));
    assert!(
        groups
            .iter()
            .all(|g| e.catalog.protection(g.id).unwrap().conflict)
    );
    assert!(
        e.catalog
            .conflicts(false)
            .unwrap()
            .iter()
            .any(|c| c.kind == "association")
    );
}

#[test]
fn native_import_selects_orf_primary_over_ori() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("a.ORF"), b"raw-a").unwrap();
    std::fs::write(r.source.join("a.ORI"), b"raw-b").unwrap();
    let mut e = engine(&r).with_extractor(Box::new(SyntheticCamera));
    e.prepare(request(&r)).unwrap();
    let groups = e.catalog.group_batch(None, 128).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].members.len(), 2);
    assert!(
        groups[0]
            .primary_raw
            .as_ref()
            .is_some_and(|p| p.as_str().ends_with(".ORF"))
    );
}
