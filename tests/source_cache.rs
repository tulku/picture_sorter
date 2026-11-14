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
fn scan(e: &mut ImportCoordinator, r: &support::TestRig, strict: bool) -> ImportReport {
    e.prepare(ImportRequest {
        source_path: r.source.clone(),
        strict,
        dry_run: false,
        operation_id: OperationId::new(),
    })
    .unwrap()
}
#[test]
fn native_repeat_and_append_read_only_new_originals() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("a.ORF"), b"original").unwrap();
    let mut e = engine(&r);
    assert_eq!(scan(&mut e, &r, false).hash_bytes, 8);
    drop(e);
    let mut e = engine(&r);
    assert_eq!(scan(&mut e, &r, false).hash_bytes, 0);
    std::fs::write(r.source.join("b.ORF"), b"new").unwrap();
    assert_eq!(scan(&mut e, &r, false).hash_bytes, 3);
    assert_eq!(scan(&mut e, &r, true).hash_bytes, 11);
}
#[test]
fn replacement_source_directory_cannot_reuse_old_cache() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("a.ORF"), b"original").unwrap();
    let mut e = engine(&r);
    scan(&mut e, &r, false);
    std::fs::rename(&r.source, r.source.with_extension("old")).unwrap();
    std::fs::create_dir(&r.source).unwrap();
    std::fs::write(r.source.join("a.ORF"), b"replaced").unwrap();
    assert_eq!(scan(&mut e, &r, false).hash_bytes, 8);
}
#[test]
fn changed_metadata_misses_and_companions_always_read() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("a.ORF"), b"old").unwrap();
    std::fs::write(r.source.join("a.ORF.rrdata"), b"{}").unwrap();
    let mut e = engine(&r);
    scan(&mut e, &r, false);
    assert_eq!(scan(&mut e, &r, false).hash_bytes, 2);
    std::fs::write(r.source.join("a.ORF"), b"new photo").unwrap();
    assert_eq!(scan(&mut e, &r, false).hash_bytes, 11);
}
#[test]
fn strict_reads_hidden_same_size_same_time_changes() {
    let r = support::TestRig::new();
    let path = r.source.join("a.ORF");
    std::fs::write(&path, b"original").unwrap();
    let mut e = engine(&r);
    scan(&mut e, &r, false);
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, b"replaced").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
    assert_eq!(scan(&mut e, &r, false).hash_bytes, 0);
    assert_eq!(scan(&mut e, &r, true).hash_bytes, 8);
}
#[test]
fn adoption_requires_matching_complete_scan_and_current_metadata() {
    let r = support::TestRig::new();
    let path = r.source.join("a.ORF");
    std::fs::write(&path, b"original").unwrap();
    let mut e = engine(&r);
    let job = e
        .begin_scan(&ImportRequest {
            source_path: r.source.clone(),
            strict: false,
            dry_run: false,
            operation_id: OperationId::new(),
        })
        .unwrap();
    assert!(
        e.catalog
            .adopt_import_source_history(job, &r.source)
            .is_err()
    );
    let done = scan(&mut e, &r, false).job.unwrap();
    assert_eq!(
        e.catalog
            .adopt_import_source_history(done, &r.source)
            .unwrap(),
        1
    );
    assert!(
        e.catalog
            .adopt_import_source_history(done, &r.local)
            .is_err()
    );
    std::fs::write(&path, b"changed metadata").unwrap();
    assert!(
        e.catalog
            .adopt_import_source_history(done, &r.source)
            .is_err()
    );
}
#[test]
fn service_repeat_avoids_old_jpeg_and_raw_hashes() {
    let service = support::service::ServiceFixture::with_config(|config, _| {
        config.reserve = 0;
    });
    assert!(service.cli(&["archive", "init"]).status.success());
    std::fs::write(
        service.r.source.join("a.JPG"),
        include_bytes!("fixtures/jpeg/red.jpg"),
    )
    .unwrap();
    std::fs::write(service.r.source.join("b.ORF"), b"raw original").unwrap();
    let import = || {
        service.cli(&[
            "import",
            service.r.source.to_str().unwrap(),
            "--wait-for",
            "ingested",
        ])
    };
    let first = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stdout)
    );
    let before = service.status()["session_metrics"].clone();
    let second = import();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stdout)
    );
    let after = service.status()["session_metrics"].clone();
    assert_eq!(after["streamed_hash_bytes"], before["streamed_hash_bytes"]);
    assert_eq!(after["transfer_bytes"], before["transfer_bytes"]);
    std::fs::write(service.r.source.join("c.ORF"), b"new").unwrap();
    let appended = import();
    assert!(
        appended.status.success(),
        "{}",
        String::from_utf8_lossy(&appended.stdout)
    );
    let value: serde_json::Value = serde_json::from_slice(&appended.stdout).unwrap();
    assert_eq!(value["outcome"]["detail"]["hash_bytes"], 3);
}

#[test]
fn swap_between_scan_binding_and_inventory_is_rejected() {
    let r = support::TestRig::new();
    let path = r.source.join("a.ORF");
    std::fs::write(&path, b"original").unwrap();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let mut e = engine(&r);
    scan(&mut e, &r, false);
    let job = e
        .begin_scan(&ImportRequest {
            source_path: r.source.clone(),
            strict: false,
            dry_run: false,
            operation_id: OperationId::new(),
        })
        .unwrap();
    std::fs::rename(&r.source, r.source.with_extension("old")).unwrap();
    std::fs::create_dir(&r.source).unwrap();
    std::fs::write(&path, b"replaced").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
    let mut inventory =
        Inventory::open(RootHandle::open(&r.source, RootPolicy::Local).unwrap()).unwrap();
    let error = e
        .accept_scan_batch(job, &inventory.next_batch(128).unwrap(), None)
        .unwrap_err();
    assert_eq!(error.code, photo_sorter::error::ErrorCode::SourceChanged);
}
