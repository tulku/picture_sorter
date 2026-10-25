mod support;
use photo_sorter::{catalog::Catalog, config::Config, domain::*, fs::*, import::*};
#[test]
fn repeat_import_zero_exif_zero_copy_measures_real_work() {
    let r = support::TestRig::new();
    for n in 0..3 {
        std::fs::write(r.source.join(format!("{n}.JPG")), vec![n; 8192]).unwrap();
    }
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut config = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    config.reserve = 0;
    let mut engine = ImportCoordinator::new(config, Catalog::open(&root).unwrap());
    let mut run = || {
        let job = engine
            .prepare(ImportRequest {
                source_path: r.source.clone(),
                strict: false,
                dry_run: false,
                operation_id: OperationId::new(),
            })
            .unwrap()
            .job
            .unwrap();
        for _ in 0..4 {
            engine.advance(job).unwrap();
        }
        engine.report(job).unwrap()
    };
    let first = run();
    assert!(first.exif_invocations > 0);
    assert_eq!(first.transfer_bytes, 3 * 8192);
    let before = photo_sorter::metrics::snapshot();
    let repeat = run();
    let measured = photo_sorter::metrics::snapshot().difference(&before);
    assert_eq!(repeat.exif_invocations, 0);
    assert_eq!(repeat.transfer_bytes, 0);
    assert_eq!(measured.exif_invocations, 0);
    assert_eq!(measured.transfer_bytes, 0);
    assert!(measured.streamed_hash_bytes >= 3 * 8192);
}
#[test]
fn bounded_inventory_batches_cover_large_generated_tree() {
    let r = support::TestRig::new();
    for n in 0..513 {
        std::fs::write(r.source.join(format!("{n:04}.JPG")), [n as u8]).unwrap();
    }
    let mut inventory =
        Inventory::open(RootHandle::open(&r.source, RootPolicy::Local).unwrap()).unwrap();
    let mut total = 0;
    let mut batches = 0;
    loop {
        let entries = inventory.next_batch(128).unwrap();
        assert!(entries.len() <= 128);
        if entries.is_empty() {
            break;
        }
        total += entries.len();
        batches += 1;
    }
    assert_eq!(total, 513);
    assert_eq!(batches, 5);
    assert!(inventory.next_batch(129).is_err());
}
#[test]
fn conflict_pages_bound_memory_and_preserve_every_record() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut catalog = Catalog::open(&root).unwrap();
    for n in 0..260 {
        catalog
            .record_ambiguity("fixture", None, serde_json::json!({"index":n}))
            .unwrap();
    }
    let mut after = None;
    let mut ids = std::collections::BTreeSet::new();
    loop {
        let page = catalog.conflicts_page(after, 128, false).unwrap();
        assert!(page.len() <= 128);
        if page.is_empty() {
            break;
        }
        after = page.last().map(|c| c.id);
        for conflict in page {
            assert!(ids.insert(conflict.id));
        }
    }
    assert_eq!(ids.len(), 260);
    assert!(catalog.conflicts_page(None, 129, false).is_err());
}
