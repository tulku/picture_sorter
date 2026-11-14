mod support;
use rusqlite::Connection;
use std::time::{Duration, Instant};
use support::service::ServiceFixture;

fn saved_versions(service: &ServiceFixture, path: &str) -> u64 {
    let database = Connection::open(service.r.state.join("catalog.sqlite")).unwrap();
    database
        .query_row(
            "SELECT count(*) FROM versions v JOIN sidecars s ON s.id=v.sidecar WHERE s.path=?",
            [path],
            |row| row.get(0),
        )
        .unwrap()
}

fn wait_for(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now();
    while !ready() {
        assert!(
            deadline.elapsed() < Duration::from_secs(12),
            "generated save was not captured"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn offline_generated_save_rewrite_and_local_deletion_preserve_history() {
    let service = ServiceFixture::with_config(|config, _| {
        config.debounce_seconds = 1;
        config.reconcile_seconds = 2;
    });
    std::fs::write(service.r.local.join("render.tif"), b"first complete HDR").unwrap();
    wait_for(|| saved_versions(&service, "render.tif") == 1);
    std::fs::write(service.r.local.join("render.tif"), b"second complete HDR").unwrap();
    wait_for(|| saved_versions(&service, "render.tif") == 2);
    std::fs::remove_file(service.r.local.join("render.tif")).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(saved_versions(&service, "render.tif"), 2);
    std::fs::write(service.r.local.join("render.tif"), b"third complete HDR").unwrap();
    wait_for(|| saved_versions(&service, "render.tif") == 3);
    assert_eq!(count(&service, "SELECT count(*) FROM version_parents"), 2);
    let database = Connection::open(service.r.state.join("catalog.sqlite")).unwrap();
    let mut query=database.prepare("SELECT snapshot.path FROM snapshots snapshot JOIN versions v ON json_extract(v.body,'$.Content.digest')=snapshot.digest JOIN sidecars s ON s.id=v.sidecar WHERE s.path='render.tif'").unwrap();
    let mut bytes = query
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| std::fs::read(service.r.state.join(row.unwrap())).unwrap())
        .collect::<Vec<_>>();
    bytes.sort();
    assert_eq!(
        bytes,
        vec![
            b"first complete HDR".to_vec(),
            b"second complete HDR".to_vec(),
            b"third complete HDR".to_vec()
        ]
    );
}

fn count(service: &ServiceFixture, sql: &str) -> u64 {
    Connection::open(service.r.state.join("catalog.sqlite"))
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

#[test]
fn unchanged_generated_file_is_not_recaptured_after_reconcile_and_restart() {
    let mut service = ServiceFixture::with_config(|config, rig| {
        config.debounce_seconds = 0;
        config.reconcile_seconds = 1;
        std::fs::write(rig.local.join("project.psd"), b"saved project bytes").unwrap();
    });
    wait_for(|| saved_versions(&service, "project.psd") == 1);
    let captures = count(&service, "SELECT count(*) FROM capture_requests");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        count(&service, "SELECT count(*) FROM capture_requests"),
        captures
    );
    service.stop();
    service.restart();
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        count(&service, "SELECT count(*) FROM capture_requests"),
        captures
    );
}

#[test]
fn adjacent_metadata_found_before_generated_base_has_the_same_group() {
    let service = ServiceFixture::with_config(|config, rig| {
        config.debounce_seconds = 0;
        std::fs::create_dir(rig.local.join("HDR")).unwrap();
        std::fs::write(
            rig.local.join("HDR/render.tif.rrdata"),
            br#"{"adjustments":{"exposure":1}}"#,
        )
        .unwrap();
        std::fs::write(rig.local.join("HDR/render.tif"), b"saved HDR bytes").unwrap();
    });
    wait_for(|| {
        saved_versions(&service, "HDR/render.tif") == 1
            && saved_versions(&service, "HDR/render.tif.rrdata") == 1
    });
    assert_eq!(
        count(
            &service,
            "SELECT count(DISTINCT group_id) FROM sidecars WHERE path IN('HDR/render.tif','HDR/render.tif.rrdata')"
        ),
        1
    );
    assert_eq!(
        count(
            &service,
            "SELECT count(*) FROM sidecars WHERE path='HDR/render.tif.rrdata' AND group_id IS NULL"
        ),
        0
    );
    assert_eq!(
        count(
            &service,
            "SELECT count(*) FROM conflicts WHERE kind='orphan_sidecar' AND resolved=0"
        ),
        0
    );
}

#[test]
fn known_camera_mutation_is_audit_failure_and_never_a_generated_save() {
    let service = ServiceFixture::with_config(|config, _| {
        config.debounce_seconds = 0;
        config.reconcile_seconds = 1;
    });
    std::fs::write(service.r.source.join("A.ORF"), b"camera original").unwrap();
    assert!(
        service
            .cli(&["import", service.r.source.to_str().unwrap()])
            .status
            .success()
    );
    let status = service.status();
    let path = status["groups"][0]["group"]["members"][0]["path"]
        .as_str()
        .unwrap();
    std::fs::write(service.r.local.join(path), b"mutated camera bytes").unwrap();
    wait_for(|| {
        count(
            &service,
            "SELECT count(*) FROM physical_findings WHERE status='Corrupt'",
        ) > 0
    });
    assert_eq!(
        count(
            &service,
            "SELECT count(*) FROM sidecars WHERE owner='generated-output'"
        ),
        0
    );
}

#[test]
fn editor_projects_are_discovered_only_in_working_inventory() {
    use photo_sorter::{
        fs::{RootHandle, RootPolicy},
        import::Inventory,
    };
    let rig = support::TestRig::new();
    for path in [
        "edit.psd",
        "edit.xcf",
        "edit.kra",
        "edit.ora",
        "edit.exr",
        "edit.hdr",
        "edit.afphoto",
        ".hidden.psd",
        "edit.partial.psd",
    ] {
        std::fs::write(rig.local.join(path), b"project").unwrap();
    }
    let root = || RootHandle::open(&rig.local, RootPolicy::Local).unwrap();
    let mut ordinary = Inventory::open_all(root(), None).unwrap();
    assert!(ordinary.next_batch(128).unwrap().is_empty());
    let mut working = Inventory::open_working(root()).unwrap();
    let mut names = working
        .next_batch(128)
        .unwrap()
        .into_iter()
        .map(|entry| entry.path.as_str().to_owned())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec![
            "edit.afphoto",
            "edit.exr",
            "edit.hdr",
            "edit.kra",
            "edit.ora",
            "edit.psd",
            "edit.xcf"
        ]
    );
}

#[test]
fn generated_output_and_later_metadata_share_group_after_atomic_save() {
    let service = ServiceFixture::with_config(|config, _| config.debounce_seconds = 0);
    std::fs::write(
        service.r.local.join("render.partial.tif"),
        b"complete staged output",
    )
    .unwrap();
    std::fs::rename(
        service.r.local.join("render.partial.tif"),
        service.r.local.join("render.tif"),
    )
    .unwrap();
    wait_for(|| saved_versions(&service, "render.tif") == 1);
    std::fs::write(
        service.r.local.join("render.tif.rrdata"),
        br#"{"adjustments":{}}"#,
    )
    .unwrap();
    wait_for(|| saved_versions(&service, "render.tif.rrdata") == 1);
    assert_eq!(
        count(
            &service,
            "SELECT count(DISTINCT group_id) FROM sidecars WHERE path IN('render.tif','render.tif.rrdata')"
        ),
        1
    );
    assert_eq!(saved_versions(&service, "render.partial.tif"), 0);
}

#[test]
fn continuous_generated_writes_are_debounced_until_stable() {
    let service = ServiceFixture::with_config(|config, _| {
        config.debounce_seconds = 2;
        config.reconcile_seconds = 1;
    });
    let path = service.r.local.join("render.tif");
    for index in 0..25 {
        std::fs::write(&path, format!("partial render {index}")).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(saved_versions(&service, "render.tif"), 0);
    std::fs::write(&path, b"complete stable render").unwrap();
    wait_for(|| saved_versions(&service, "render.tif") == 1);
}

#[test]
fn insufficient_offline_snapshot_capacity_retains_live_output() {
    let service = ServiceFixture::with_config(|config, _| {
        config.debounce_seconds = 0;
        config.budget = 1;
        config.reserve = 0;
    });
    let path = service.r.local.join("render.tif");
    std::fs::write(&path, b"render waiting for archive capacity").unwrap();
    wait_for(|| {
        count(
            &service,
            "SELECT count(*) FROM capture_requests WHERE direct=1 AND done=0",
        ) > 0
    });
    assert_eq!(
        std::fs::read(path).unwrap(),
        b"render waiting for archive capacity"
    );
    assert_eq!(saved_versions(&service, "render.tif"), 0);
    assert_eq!(
        count(&service, "SELECT count(*) FROM residency WHERE protected=1"),
        1
    );
}

fn initialize_generated_archive(service: &ServiceFixture) {
    let out = service.cli(&["archive", "init"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let request = service.r._temp.path().join("generated-qualification.json");
    std::fs::write(&request, br#"{"kind":"generated_outputs"}"#).unwrap();
    let out = service.cli(&["qualify", "--request", request.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["outcome"]["detail"]["qualified"], true, "{result}");
    let marker: photo_sorter::archive::ArchiveMarker = serde_json::from_slice(
        &std::fs::read(service.r.archive.join(".photo-sorter/archive.json")).unwrap(),
    )
    .unwrap();
    let change = photo_sorter::archive::ModeChangeRequest {
        operation: photo_sorter::domain::OperationId::new(),
        new_owner: marker.writer_id,
        expected: marker,
        mode: photo_sorter::archive::PublicationMode::LocalTest,
        all_writers_quiescent: true,
        prior_owner_stopped: true,
        no_unresolved_io: true,
        recover: false,
    };
    let request = service.r._temp.path().join("local-test-mode.json");
    std::fs::write(&request, serde_json::to_vec(&change).unwrap()).unwrap();
    let out = service.cli(&[
        "archive",
        "change-mode",
        "--request",
        request.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

fn wait_for_online(service: &ServiceFixture, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now();
    while !ready() {
        assert!(
            deadline.elapsed() < Duration::from_secs(12),
            "online generated save was not published: {}",
            service.status()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn archive_history(service: &ServiceFixture, bytes: &[u8]) -> Vec<u8> {
    let path =
        photo_sorter::archive::history_path(&photo_sorter::domain::Digest::of(bytes)).unwrap();
    std::fs::read(service.r.archive.join(path.as_str())).unwrap()
}

#[test]
fn online_large_generated_saves_publish_latest_filename_and_exact_history() {
    let service = ServiceFixture::with_config(|config, _| {
        config.debounce_seconds = 0;
        config.json_ceiling = 32;
    });
    initialize_generated_archive(&service);
    let first = vec![b'a'; 8192];
    let second = vec![b'b'; 8192];
    std::fs::create_dir(service.r.local.join("HDR")).unwrap();
    std::fs::write(service.r.local.join("HDR/render.tif"), &first).unwrap();
    std::fs::write(service.r.local.join("HDR/render.tif.rrdata"), b"{}").unwrap();
    wait_for_online(&service, || {
        std::fs::read(service.r.archive.join("HDR/render.tif")).is_ok_and(|bytes| bytes == first)
            && std::fs::read(service.r.archive.join("HDR/render.tif.rrdata"))
                .is_ok_and(|bytes| bytes == b"{}")
    });
    std::fs::write(service.r.local.join("HDR/render.tif"), &second).unwrap();
    wait_for_online(&service, || {
        std::fs::read(service.r.archive.join("HDR/render.tif")).is_ok_and(|bytes| bytes == second)
    });
    assert_eq!(archive_history(&service, &first), first);
    assert_eq!(archive_history(&service, &second), second);
    assert_eq!(saved_versions(&service, "HDR/render.tif"), 2);
}

#[test]
fn two_offline_generated_saves_reconnect_without_projecting_historical_save() {
    let service = ServiceFixture::with_config(|config, _| {
        config.debounce_seconds = 0;
        config.json_ceiling = 32;
    });
    let first = vec![b'c'; 8192];
    let second = vec![b'd'; 8192];
    std::fs::write(service.r.local.join("render.tif"), &first).unwrap();
    wait_for_online(&service, || saved_versions(&service, "render.tif") == 1);
    std::fs::write(service.r.local.join("render.tif"), &second).unwrap();
    wait_for_online(&service, || saved_versions(&service, "render.tif") == 2);
    initialize_generated_archive(&service);
    wait_for_online(&service, || {
        std::fs::read(service.r.archive.join("render.tif")).is_ok_and(|bytes| bytes == second)
    });
    wait_for_online(&service, || {
        count(
            &service,
            "SELECT count(*) FROM residency WHERE metadata_current=1 AND protected=0",
        ) == 1
    });
    assert_eq!(service.status()["activity"]["generated"]["pending"], 0);
    assert_eq!(archive_history(&service, &first), first);
    assert_eq!(archive_history(&service, &second), second);
    assert_eq!(
        count(
            &service,
            "SELECT count(*) FROM projection_commits p JOIN version_parents parents ON parents.parent=p.version"
        ),
        0
    );
}

#[test]
fn archived_orphan_metadata_becomes_adjacent_to_new_generated_output() {
    let service = ServiceFixture::with_config(|config, _| config.debounce_seconds = 0);
    initialize_generated_archive(&service);
    std::fs::write(service.r.local.join("render.tif.rrdata"), b"{}").unwrap();
    wait_for_online(&service, || {
        count(
            &service,
            "SELECT count(*) FROM archived_versions a JOIN versions v ON v.id=a.version JOIN sidecars s ON s.id=v.sidecar WHERE s.path='render.tif.rrdata'",
        ) == 1
    });
    assert_eq!(
        count(
            &service,
            "SELECT count(*) FROM conflicts WHERE kind='orphan_sidecar' AND resolved=0"
        ),
        1
    );
    std::fs::write(service.r.local.join("render.tif"), b"generated HDR").unwrap();
    wait_for_online(&service, || {
        count(
            &service,
            "SELECT count(*) FROM conflicts WHERE kind='orphan_sidecar' AND resolved=0",
        ) == 0
    });
    wait_for_online(&service, || {
        std::fs::read(service.r.archive.join("render.tif.rrdata")).is_ok_and(|bytes| bytes == b"{}")
    });
    assert_eq!(
        count(&service, "SELECT count(DISTINCT group_id) FROM sidecars"),
        1
    );
    assert_eq!(saved_versions(&service, "render.tif.rrdata"), 1);
    wait_for_online(&service, || {
        count(
            &service,
            "SELECT count(*) FROM residency WHERE metadata_current=1 AND protected=0",
        ) == 1
    });
}
