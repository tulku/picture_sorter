mod support;
use photo_sorter::domain::Digest;
use serde_json::Value;
use support::service::ServiceFixture;

const RED: &[u8] = include_bytes!("fixtures/jpeg/red.jpg");
const BLUE: &[u8] = include_bytes!("fixtures/jpeg/blue.jpg");
const DESTINATION: &str = "JPEG/2026/05/15/A.JPG";
fn block(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0xff, marker];
    bytes.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}
fn picture(base: &[u8], geotag: bool, orientation: u16) -> Vec<u8> {
    fn entry(bytes: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: [u8; 4]) {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend_from_slice(&value);
    }
    let mut exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x04\0".to_vec();
    entry(&mut exif, 0x010f, 2, 3, *b"OM\0\0");
    entry(&mut exif, 0x0110, 2, 2, *b"T\0\0\0");
    let mut rotation = [0; 4];
    rotation[..2].copy_from_slice(&orientation.to_le_bytes());
    entry(&mut exif, 0x0112, 3, 1, rotation);
    entry(&mut exif, 0x8769, 4, 1, 62u32.to_le_bytes());
    exif.extend_from_slice(&[0; 4]);
    exif.extend_from_slice(&2u16.to_le_bytes());
    entry(&mut exif, 0x9003, 2, 20, 92u32.to_le_bytes());
    entry(&mut exif, 0xa001, 3, 1, 1u32.to_le_bytes());
    exif.extend_from_slice(&[0; 4]);
    exif.extend_from_slice(b"2026:05:15 12:00:00\0");
    let mut result = base[..2].to_vec();
    result.extend_from_slice(&block(0xe1, &exif));
    if geotag {
        result.extend_from_slice(&block(0xe1,b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description xmlns:exif='http://ns.adobe.com/exif/1.0/' exif:GPSLatitude='51,12N' exif:GPSLongitude='1,30E'/></rdf:RDF></x:xmpmeta>"));
    }
    result.extend_from_slice(&base[2..]);
    result
}
fn parsed(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}
fn fixture(nas: &[u8]) -> ServiceFixture {
    let service = ServiceFixture::new();
    std::fs::create_dir_all(service.r.archive.join("JPEG/2026/05/15")).unwrap();
    std::fs::write(service.r.archive.join(DESTINATION), nas).unwrap();
    let init = service.cli(&["archive", "init"]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stdout)
    );
    let bootstrap = service.cli(&["bootstrap", "--wait-for", "archived"]);
    assert!(
        bootstrap.status.success(),
        "{}",
        String::from_utf8_lossy(&bootstrap.stdout)
    );
    service
}
#[test]
fn metadata_only_jpeg_is_recognised_without_copying_or_false_byte_archival() {
    let nas = picture(RED, true, 1);
    let card = picture(RED, false, 1);
    let service = fixture(&nas);
    std::fs::write(service.r.source.join("A.JPG"), &card).unwrap();
    for _ in 0..2 {
        let output = service.cli(&[
            "import",
            service.r.source.to_str().unwrap(),
            "--wait-for",
            "archived",
        ]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let outcome = parsed(&output);
        let report = &outcome["outcome"]["detail"];
        assert_eq!(report["selected"], 1);
        assert_eq!(report["image_duplicates"], 1);
        assert_eq!(report["archive_committed"], 0);
        assert_eq!(report["pending_archive"], 0);
        assert_eq!(report["transfer_bytes"], 0);
        assert_eq!(report["conflicting"], 0);
        assert!(!service.r.local.join(DESTINATION).exists());
        assert_eq!(
            std::fs::read(service.r.archive.join(DESTINATION)).unwrap(),
            nas
        );
        assert_eq!(std::fs::read(service.r.source.join("A.JPG")).unwrap(), card);
    }
    let db = rusqlite::Connection::open(service.r.state.join("catalog.sqlite")).unwrap();
    let archived: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM placements WHERE digest=? AND archived=1)",
            [Digest::of(&card).as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        !archived,
        "metadata-only source bytes must not be falsely recorded as archived"
    );
    let receipts: u64 = db
        .query_row(
            "SELECT count(*) FROM jpeg_duplicates WHERE canonical_digest=?",
            [Digest::of(&nas).as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(receipts, 2);
}
#[test]
fn pixel_and_orientation_changes_remain_conflicts() {
    let base = picture(RED, false, 1);
    let mut xmp_rotated = base[..2].to_vec();
    xmp_rotated.extend_from_slice(&block(0xe1, b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description xmlns:tiff='http://ns.adobe.com/tiff/1.0/' tiff:Orientation='6'/></rdf:RDF></x:xmpmeta>"));
    xmp_rotated.extend_from_slice(&base[2..]);
    for card in [picture(BLUE, false, 1), picture(RED, false, 6), xmp_rotated] {
        let nas = picture(RED, true, 1);
        let service = fixture(&nas);
        std::fs::write(service.r.source.join("A.JPG"), &card).unwrap();
        let output = service.cli(&[
            "import",
            service.r.source.to_str().unwrap(),
            "--wait-for",
            "archived",
        ]);
        assert_eq!(
            output.status.code(),
            Some(4),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(parsed(&output)["outcome"]["detail"]["image_duplicates"], 0);
        assert_eq!(
            std::fs::read(service.r.archive.join(DESTINATION)).unwrap(),
            nas
        );
    }
}
#[test]
fn nas_bytes_changed_after_cataloguing_are_not_acknowledged_as_duplicates() {
    let nas = picture(RED, true, 1);
    let service = fixture(&nas);
    let mut changed = nas.clone();
    changed.extend_from_slice(b"changed after catalogue");
    std::fs::write(service.r.archive.join(DESTINATION), &changed).unwrap();
    std::fs::write(service.r.source.join("A.JPG"), picture(RED, false, 1)).unwrap();
    let output = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--strict",
        "--wait-for",
        "archived",
    ]);
    assert!(
        !output.status.success(),
        "changed NAS evidence must not be accepted"
    );
    assert_eq!(parsed(&output)["outcome"]["detail"]["image_duplicates"], 0);
    assert_eq!(
        std::fs::read(service.r.archive.join(DESTINATION)).unwrap(),
        changed
    );
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !predicate() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "JPEG test condition timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
fn pending_import(service: &ServiceFixture) -> String {
    let output = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--strict",
        "--no-wait",
    ]);
    assert!(output.status.success());
    let job = parsed(&output)["outcome"]["job"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_until(|| {
        let db = rusqlite::Connection::open_with_flags(
            service.r.state.join("catalog.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM import_items WHERE parent=? AND state='jpeg_pending')",
            [&job],
            |r| r.get::<_, bool>(0),
        )
        .unwrap()
    });
    job
}
#[test]
fn source_mutation_during_comparison_cannot_be_acknowledged() {
    let service = fixture(&picture(RED, true, 1));
    std::fs::write(service.r.source.join("A.JPG"), picture(RED, false, 1)).unwrap();
    let pid = service.status()["worker"]["pid"].as_u64().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    let job = pending_import(&service);
    std::fs::write(service.r.source.join("A.JPG"), picture(BLUE, false, 1)).unwrap();
    assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);
    let mut outcome = Value::Null;
    wait_until(|| {
        outcome = parsed(&service.cli(&["jobs", "show", &job]));
        outcome["outcome"]["job"]["state"] == "Conflict"
    });
    assert_eq!(outcome["outcome"]["detail"]["image_duplicates"], 0);
    assert_eq!(outcome["outcome"]["detail"]["transfer_bytes"], 0);
    assert_eq!(outcome["outcome"]["detail"]["conflicting"], 1);
    assert!(!service.r.local.join(DESTINATION).exists());
    assert_eq!(
        std::fs::read(service.r.source.join("A.JPG")).unwrap(),
        picture(BLUE, false, 1)
    );
}
#[test]
fn cancelled_comparison_never_acknowledges_a_duplicate() {
    let service = fixture(&picture(RED, true, 1));
    std::fs::write(service.r.source.join("A.JPG"), picture(RED, false, 1)).unwrap();
    let pid = service.status()["worker"]["pid"].as_u64().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    let job = pending_import(&service);
    service.cli(&["jobs", "cancel", &job, "--no-wait"]);
    assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);
    let mut outcome = Value::Null;
    wait_until(|| {
        outcome = parsed(&service.cli(&["jobs", "show", &job]));
        outcome["outcome"]["job"]["state"] == "Cancelled"
    });
    assert_eq!(outcome["outcome"]["detail"]["image_duplicates"], 0);
    assert_eq!(outcome["outcome"]["detail"]["transfer_bytes"], 0);
}
#[test]
fn deferred_comparison_resumes_after_service_restart_without_copying() {
    let mut service = fixture(&picture(RED, true, 1));
    std::fs::write(service.r.source.join("A.JPG"), picture(RED, false, 1)).unwrap();
    let pid = service.status()["worker"]["pid"].as_u64().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    let job = pending_import(&service);
    service.stop();
    service.restart();
    let mut outcome = Value::Null;
    wait_until(|| {
        outcome = parsed(&service.cli(&["jobs", "show", &job]));
        outcome["outcome"]["job"]["state"] == "Complete"
    });
    assert_eq!(outcome["outcome"]["detail"]["image_duplicates"], 1);
    assert_eq!(outcome["outcome"]["detail"]["transfer_bytes"], 0);
}
#[test]
fn new_images_still_upload_while_matching_images_are_recognised() {
    let nas = picture(RED, true, 1);
    let new = picture(BLUE, false, 1);
    let service = fixture(&nas);
    std::fs::write(service.r.source.join("A.JPG"), picture(RED, false, 1)).unwrap();
    std::fs::write(service.r.source.join("B.JPG"), &new).unwrap();
    let output = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let result = parsed(&output);
    let report = &result["outcome"]["detail"];
    assert_eq!(report["selected"], 2);
    assert_eq!(report["image_duplicates"], 1);
    assert_eq!(report["archive_committed"], 1);
    assert!(!service.r.local.join(DESTINATION).exists());
    assert_eq!(
        std::fs::read(service.r.archive.join(DESTINATION)).unwrap(),
        nas
    );
    assert_eq!(
        std::fs::read(service.r.archive.join("JPEG/2026/05/15/B.JPG")).unwrap(),
        new
    );
}
#[test]
fn jpeg_sidecar_keeps_the_canonical_nas_owner() {
    let nas = picture(RED, true, 1);
    let service = fixture(&nas);
    std::fs::write(service.r.source.join("A.JPG"), picture(RED, false, 1)).unwrap();
    let xmp = b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>";
    std::fs::write(service.r.source.join("A.JPG.xmp"), xmp).unwrap();
    let output = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(parsed(&output)["outcome"]["detail"]["image_duplicates"], 1);
    let history = photo_sorter::archive::history_path(&Digest::of(xmp)).unwrap();
    assert_eq!(
        std::fs::read(service.r.archive.join(history.as_str())).unwrap(),
        xmp
    );
    let db = rusqlite::Connection::open(service.r.state.join("catalog.sqlite")).unwrap();
    let owned: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sidecars s JOIN members m ON m.group_id=s.group_id WHERE s.path='JPEG/2026/05/15/A.JPG.xmp' AND s.owner='jpeg-xmp-external' AND m.path=? AND m.digest=?)",rusqlite::params![DESTINATION,Digest::of(&nas).as_str()],|r|r.get(0)).unwrap();
    assert!(
        owned,
        "preserved sidecar history must refer to the canonical NAS JPEG"
    );
    assert_eq!(
        std::fs::read(service.r.archive.join(DESTINATION)).unwrap(),
        nas
    );
}

fn seed_reserved_destination(service: &mut ServiceFixture, card: &[u8], histories: &[&str]) {
    use photo_sorter::{
        catalog::{Catalog, DurableRequest, JobKind, JobTransition, PairEvidence},
        domain::*,
        fs::{RootHandle, RootPolicy},
    };
    service.stop();
    let root = RootHandle::open(&service.r.state, RootPolicy::Local).unwrap();
    let mut catalog = Catalog::open(&root).unwrap();
    let group = catalog.create_group(false).unwrap();
    let reserved = "JPEG/2026/05/15/A_CAMERA_ORIGINAL.JPG";
    catalog
        .attach_member(
            group,
            OriginalMember {
                path: ArchivePath::parse(reserved).unwrap(),
                digest: Digest::of(card),
                size: card.len() as u64,
                role: MediaRole::Jpeg,
            },
            PairEvidence::UserSelected {
                reason: "existing reserved card version".into(),
            },
        )
        .unwrap();
    let group = catalog.canonical_group(group).unwrap();
    let job = catalog
        .enqueue(DurableRequest {
            operation_id: OperationId::new(),
            kind: JobKind::Import {
                source: service.r.source.clone(),
                strict: false,
            },
        })
        .unwrap();
    catalog
        .transition(job, 0, JobTransition::RequestCancel)
        .unwrap();
    catalog
        .transition(job, 0, JobTransition::ConfirmCancelled)
        .unwrap();
    drop(catalog);
    let db = rusqlite::Connection::open(service.r.state.join("catalog.sqlite")).unwrap();
    db.execute(
        "INSERT INTO import_scans(job,complete) VALUES(?,1)",
        [job.to_string()],
    )
    .unwrap();
    db.execute("INSERT INTO import_stats(job) VALUES(?)", [job.to_string()])
        .unwrap();
    db.execute("INSERT INTO group_facts(context,stem,camera,capture,group_id) VALUES('','A','OM/T','2026:05:15 12:00:00',?)",[group.to_string()]).unwrap();
    for (index, history) in histories.iter().enumerate() {
        let item = JobId::new();
        db.execute("INSERT INTO import_items(id,parent,source_root,source_path,digest,size,role,group_id,destination,state) VALUES(?,?,?,?,?,?,?,?,?,'pending')",rusqlite::params![item.to_string(),job.to_string(),service.r.source.to_str().unwrap(),format!("legacy/{index}/A.JPG"),Digest::of(card).as_str(),card.len() as u64,serde_json::to_string(&photo_sorter::import::SourceRole::Original(MediaRole::Jpeg)).unwrap(),group.to_string(),reserved]).unwrap();
        db.execute("INSERT INTO retired_import_plans(item,replacement,operation,previous) VALUES(?,?,?,?)",rusqlite::params![JobId::new().to_string(),item.to_string(),OperationId::new().to_string(),serde_json::json!({"destination":history,"digest":Digest::of(card),"size":card.len(),"local_ready":false}).to_string()]).unwrap();
    }
    drop(db);
    drop(root);
    service.restart();
}
#[test]
fn reserved_camera_original_destination_does_not_defeat_canonical_recognition() {
    let nas = picture(RED, true, 1);
    let card = picture(RED, false, 1);
    let mut service = fixture(&nas);
    seed_reserved_destination(&mut service, &card, &[DESTINATION]);
    std::fs::write(service.r.source.join("A.JPG"), &card).unwrap();
    let output = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(parsed(&output)["outcome"]["detail"]["image_duplicates"], 1);
    assert_eq!(parsed(&output)["outcome"]["detail"]["transfer_bytes"], 0);
    assert!(
        !service
            .r
            .archive
            .join("JPEG/2026/05/15/A_CAMERA_ORIGINAL.JPG")
            .exists()
    );
    assert_eq!(
        std::fs::read(service.r.archive.join(DESTINATION)).unwrap(),
        nas
    );
}
#[test]
fn multiple_recorded_archive_candidates_remain_ambiguous() {
    let nas = picture(RED, true, 1);
    let card = picture(RED, false, 1);
    let mut service = fixture(&nas);
    let other = "JPEG/2026/05/15/Other/A.JPG";
    std::fs::create_dir_all(service.r.archive.join("JPEG/2026/05/15/Other")).unwrap();
    std::fs::write(service.r.archive.join(other), &nas).unwrap();
    assert!(
        service
            .cli(&["bootstrap", "--wait-for", "archived"])
            .status
            .success()
    );
    seed_reserved_destination(&mut service, &card, &[DESTINATION, other]);
    std::fs::write(service.r.source.join("A.JPG"), &card).unwrap();
    let output = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        !output.status.success(),
        "multiple candidates need individual review"
    );
    assert_eq!(parsed(&output)["outcome"]["job"]["state"], "Conflict");
    assert_eq!(parsed(&output)["outcome"]["detail"]["image_duplicates"], 0);
    assert_eq!(parsed(&output)["outcome"]["detail"]["transfer_bytes"], 0);
    assert_eq!(
        std::fs::read(service.r.archive.join(DESTINATION)).unwrap(),
        nas
    );
    assert_eq!(std::fs::read(service.r.archive.join(other)).unwrap(), nas);
}
#[test]
fn unsupported_jpeg_keeps_existing_exact_file_conflict_behaviour() {
    let nas = picture(RED, true, 1);
    let service = fixture(&nas);
    let mut card = picture(RED, false, 1);
    card.truncate(card.len() - 2);
    std::fs::write(service.r.source.join("A.JPG"), &card).unwrap();
    let output = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(!output.status.success());
    assert_eq!(parsed(&output)["outcome"]["detail"]["image_duplicates"], 0);
    assert_eq!(
        std::fs::read(service.r.archive.join(DESTINATION)).unwrap(),
        nas
    );
}

#[test]
fn genuine_image_collision_can_still_publish_a_distinct_version() {
    let nas = picture(RED, true, 1);
    let card = picture(BLUE, false, 1);
    let service = fixture(&nas);
    std::fs::write(service.r.source.join("A.JPG"), &card).unwrap();
    let output = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(!output.status.success());
    let status = service.status();
    let conflict = status["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "original_collision")
        .unwrap();
    let group = status["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["group"]["id"] == conflict["group"])
        .unwrap();
    let target = "JPEG/2026/05/15/A_DIFFERENT.JPG";
    let request = serde_json::json!({"operation":photo_sorter::domain::OperationId::new(),"conflict":conflict["id"],"expected_revision":conflict["revision"],"expected_heads":[],"expected_memberships":[[conflict["group"],group["group"]["membership_revision"]]],"resolution":{"type":"choose_distinct_path","path":target}});
    let file = service.r._temp.path().join("resolution.json");
    std::fs::write(&file, serde_json::to_vec(&request).unwrap()).unwrap();
    let resolution = service.cli(&[
        "conflicts",
        "resolve",
        "--request",
        file.to_str().unwrap(),
        "--no-wait",
    ]);
    assert!(
        resolution.status.success(),
        "{}",
        String::from_utf8_lossy(&resolution.stdout)
    );
    wait_until(|| std::fs::read(service.r.archive.join(target)).is_ok_and(|value| value == card));
    assert_eq!(
        std::fs::read(service.r.archive.join(DESTINATION)).unwrap(),
        nas
    );
    let repeated = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        repeated.status.success(),
        "{}",
        String::from_utf8_lossy(&repeated.stdout)
    );
    assert_eq!(parsed(&repeated)["outcome"]["detail"]["transfer_bytes"], 0);
}
#[test]
fn existing_camera_original_copy_is_preserved_while_nas_metadata_is_preferred() {
    let nas = picture(RED, true, 1);
    let card = picture(RED, false, 1);
    let mut service = fixture(&nas);
    let reserved = "JPEG/2026/05/15/A_CAMERA_ORIGINAL.JPG";
    std::fs::write(service.r.archive.join(reserved), &card).unwrap();
    assert!(
        service
            .cli(&["bootstrap", "--wait-for", "archived"])
            .status
            .success()
    );
    seed_reserved_destination(&mut service, &card, &[DESTINATION]);
    std::fs::write(service.r.source.join("A.JPG"), &card).unwrap();
    let output = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(parsed(&output)["outcome"]["detail"]["image_duplicates"], 1);
    assert_eq!(parsed(&output)["outcome"]["detail"]["transfer_bytes"], 0);
    assert_eq!(
        std::fs::read(service.r.archive.join(DESTINATION)).unwrap(),
        nas
    );
    assert_eq!(
        std::fs::read(service.r.archive.join(reserved)).unwrap(),
        card
    );
}
