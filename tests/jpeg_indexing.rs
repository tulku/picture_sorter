mod support;
use photo_sorter::{
    catalog::Catalog,
    domain::Digest,
    fs::{RootHandle, RootPolicy},
    import::inspect_jpeg,
};
use support::service::ServiceFixture;
const JPEG: &[u8] = include_bytes!("fixtures/jpeg/red.jpg");
fn bootstrap_fixture() -> ServiceFixture {
    let fixture = ServiceFixture::new();
    std::fs::create_dir_all(fixture.r.archive.join("JPEG/2026/11/14")).unwrap();
    std::fs::write(fixture.r.archive.join("JPEG/2026/11/14/OLD.JPG"), JPEG).unwrap();
    assert!(fixture.cli(&["archive", "init"]).status.success());
    assert!(fixture.cli(&["bootstrap"]).status.success());
    fixture
}
#[test]
fn bootstrap_records_jpeg_image_identity_in_its_existing_read_pass() {
    let mut fixture = bootstrap_fixture();
    fixture.stop();
    let catalog =
        Catalog::open(&RootHandle::open(&fixture.r.state, RootPolicy::Local).unwrap()).unwrap();
    let expected = inspect_jpeg(std::io::Cursor::new(JPEG)).unwrap().image;
    assert_eq!(
        catalog.jpeg_fingerprint(&Digest::of(JPEG)).unwrap(),
        Some(expected)
    );
    assert_eq!(
        std::fs::read(fixture.r.archive.join("JPEG/2026/11/14/OLD.JPG")).unwrap(),
        JPEG
    );
}
#[test]
fn offline_image_index_resumes_and_rejects_changed_catalogued_bytes() {
    let mut fixture = bootstrap_fixture();
    fixture.stop();
    let c = rusqlite::Connection::open(fixture.r.state.join("catalog.sqlite")).unwrap();
    c.execute("DELETE FROM jpeg_fingerprints", []).unwrap();
    drop(c);
    let first = fixture.cli(&["catalog", "index-jpegs", "--offline"]);
    assert!(
        first.status.success(),
        "{} {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(report["indexed"], 1);
    let again = fixture.cli(&["catalog", "index-jpegs", "--offline"]);
    assert!(again.status.success());
    let report: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(report["indexed"], 0);
    assert_eq!(report["bytes"], 0);
    let c = rusqlite::Connection::open(fixture.r.state.join("catalog.sqlite")).unwrap();
    c.execute("DELETE FROM jpeg_fingerprints", []).unwrap();
    drop(c);
    std::fs::write(
        fixture.r.archive.join("JPEG/2026/11/14/OLD.JPG"),
        b"changed archived bytes",
    )
    .unwrap();
    let changed = fixture.cli(&["catalog", "index-jpegs", "--offline"]);
    assert_eq!(changed.status.code(), Some(4));
    assert_eq!(
        std::fs::read(fixture.r.archive.join("JPEG/2026/11/14/OLD.JPG")).unwrap(),
        b"changed archived bytes"
    );
}

#[test]
fn offline_source_preview_uses_cache_without_creating_jobs_or_reading_new_bytes() {
    let mut fixture = bootstrap_fixture();
    std::fs::write(fixture.r.source.join("OLD.JPG"), JPEG).unwrap();
    let imported = fixture.cli(&[
        "import",
        fixture.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stdout)
    );
    let response: serde_json::Value = serde_json::from_slice(&imported.stdout).unwrap();
    let job = response["outcome"]["job"]["id"].as_str().unwrap();
    std::fs::write(fixture.r.source.join("NEW.ORF"), b"new photo").unwrap();
    fixture.stop();
    let count = || {
        rusqlite::Connection::open(fixture.r.state.join("catalog.sqlite"))
            .unwrap()
            .query_row("select count(*) from jobs", [], |row| row.get::<_, u64>(0))
            .unwrap()
    };
    let before = count();
    let output = fixture.cli(&[
        "catalog",
        "preview-source",
        job,
        fixture.r.source.to_str().unwrap(),
        "--offline",
    ]);
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["selected"], 2);
    assert_eq!(report["cached"], 1);
    assert_eq!(report["archive_present"], 1);
    assert_eq!(report["uncached"], 1);
    assert_eq!(report["hash_bytes"], 0);
    assert_eq!(report["transfer_bytes"], 0);
    assert_eq!(count(), before);
    assert_eq!(
        std::fs::read(fixture.r.source.join("NEW.ORF")).unwrap(),
        b"new photo"
    );
}
