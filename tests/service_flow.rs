mod support;
use photo_sorter::{config::Config, domain::*};
use std::time::{Duration, Instant};
use support::service::ServiceFixture;
#[test]
fn hung_nas_status_and_local_work_respond() {
    let service = ServiceFixture::new();
    let status = service.status();
    let pid = status["worker"]["pid"].as_u64().unwrap() as i32;
    unsafe {
        libc::kill(pid, libc::SIGSTOP);
    }
    let archive = service.cli(&["archive", "init", "--no-wait"]);
    assert!(archive.status.success());
    std::fs::write(service.r.source.join("A.ORF"), b"raw").unwrap();
    let start = Instant::now();
    let out = service.cli(&["import", service.r.source.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(start.elapsed() < Duration::from_secs(5));
    let start = Instant::now();
    let status = service.status();
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(
        status["groups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["readiness"]["originals_ready"] == true)
    );
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}
#[test]
fn online_saved_edit_publishes_exact_projection() {
    let service = ServiceFixture::new();
    assert!(service.cli(&["archive", "init"]).status.success());
    std::fs::write(service.r.source.join("A.ORF"), b"raw").unwrap();
    let out = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let status = service.status();
    let path = status["groups"][0]["group"]["members"][0]["path"]
        .as_str()
        .unwrap();
    assert_eq!(std::fs::read(service.r.archive.join(path)).unwrap(), b"raw");
    let sidecar = service.r.local.join(format!("{path}.rrdata"));
    std::fs::write(
        &sidecar,
        br#"{"version":1,"rating":3,"adjustments":{"exposure":1},"tags":null}"#,
    )
    .unwrap();
    let deadline = Instant::now();
    loop {
        let archive_sidecar = service.r.archive.join(format!("{path}.rrdata"));
        if std::fs::read(&archive_sidecar).is_ok_and(|bytes| {
            bytes == br#"{"version":1,"rating":3,"adjustments":{"exposure":1},"tags":null}"#
        }) {
            break;
        }
        assert!(
            deadline.elapsed() < Duration::from_secs(7),
            "saved edit was not published"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
#[test]
fn fresh_url_unavailable_does_not_block_import() {
    let service = ServiceFixture::with_config(|config, r| {
        use std::os::unix::fs::PermissionsExt;
        let key = r._temp.path().join("key");
        std::fs::write(&key, "isolated-fixture-key").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        config.immich = Some(photo_sorter::config::ImmichConfig {
            origin: "http://127.0.0.1:1".into(),
            key_file: key,
            library_id: None,
            write_mode: "disabled".into(),
        });
    });
    let out = service.cli(&[
        "pin",
        "http://127.0.0.1:1/photos/11111111-1111-1111-1111-111111111111",
        "--no-wait",
    ]);
    assert_eq!(out.status.code(), Some(0));
    std::fs::write(service.r.source.join("A.JPG"), b"jpeg").unwrap();
    assert!(
        service
            .cli(&["import", service.r.source.to_str().unwrap()])
            .status
            .success()
    );
}
#[test]
fn offline_import_edit_restart_reconnect_archive_with_actual_immich_pin() {
    let server = support::http::RatingServer::new();
    let origin = server.origin.to_string();
    let mut service = ServiceFixture::with_config(|config, r| {
        use std::os::unix::fs::PermissionsExt;
        let key = r._temp.path().join("key");
        std::fs::write(&key, "isolated-fixture-key").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        config.immich = Some(photo_sorter::config::ImmichConfig {
            origin: origin.clone(),
            key_file: key,
            library_id: None,
            write_mode: "disabled".into(),
        });
        config.immich_path_mappings = vec![photo_sorter::immich::PrefixMapping {
            source: "/external".into(),
            target: None,
            library: None,
        }];
        std::fs::write(r.archive.join("a.JPG"), b"fixture jpeg").unwrap();
    });
    assert!(service.cli(&["archive", "init"]).status.success());
    let bootstrap = service.cli(&["bootstrap"]);
    assert!(
        bootstrap.status.success(),
        "{}",
        String::from_utf8_lossy(&bootstrap.stdout)
    );
    let url = format!("{}/photos/{}", origin.trim_end_matches('/'), server.asset);
    let out = service.cli(&["pin", &url]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        std::fs::read(service.r.local.join("a.JPG")).unwrap(),
        b"fixture jpeg"
    );
    assert!(server.state.writes.lock().unwrap().is_empty());
    let offline = service.r._temp.path().join("offline-archive");
    std::fs::rename(&service.r.archive, &offline).unwrap();
    std::fs::write(service.r.source.join("A.ORF"), b"offline raw").unwrap();
    let out = service.cli(&["import", service.r.source.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let status = service.status();
    let raw = status["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["group"]["members"].as_array().unwrap())
        .find(|m| m["path"].as_str().unwrap().ends_with("A.ORF"))
        .unwrap()["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        std::fs::read(service.r.local.join(&raw)).unwrap(),
        b"offline raw"
    );
    std::fs::remove_file(service.r.source.join("A.ORF")).unwrap();
    let sidecar = service.r.local.join(format!("{raw}.rrdata"));
    let first = br#"{"version":1,"rating":3,"adjustments":{"exposure":1},"tags":null}"#;
    let later = br#"{"version":1,"rating":5,"adjustments":{"exposure":2},"tags":null}"#;
    std::fs::write(&sidecar, first).unwrap();
    let deadline = Instant::now();
    while service.status()["watcher"]["captured"]
        .as_u64()
        .unwrap_or(0)
        == 0
    {
        assert!(deadline.elapsed() < Duration::from_secs(7));
        std::thread::sleep(Duration::from_millis(20));
    }
    service.stop();
    std::fs::write(&sidecar, later).unwrap();
    service.restart();
    std::fs::rename(&offline, &service.r.archive).unwrap();
    if service
        .r
        .archive
        .join(".photo-sorter/locks/publication")
        .exists()
    {
        let out = service.cli(&[
            "archive",
            "recover-lock",
            "--prior-owner-stopped",
            "--all-writers-quiescent",
        ]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    let deadline = Instant::now();
    while std::fs::read(service.r.archive.join(format!("{raw}.rrdata")))
        .ok()
        .as_deref()
        != Some(later.as_slice())
    {
        assert!(
            deadline.elapsed() < Duration::from_secs(20),
            "offline/restart work did not converge: {}",
            service.status()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        std::fs::read(service.r.archive.join(&raw)).unwrap(),
        b"offline raw"
    );
    for bytes in [first.as_slice(), later.as_slice()] {
        let history = photo_sorter::archive::history_path(&Digest::of(bytes)).unwrap();
        assert_eq!(
            std::fs::read(service.r.archive.join(history.as_str())).unwrap(),
            bytes
        );
    }
    assert!(server.state.writes.lock().unwrap().is_empty());
}
#[test]
fn nas_collision_does_not_starve_independent_items() {
    let service = ServiceFixture::new();
    assert!(service.cli(&["archive", "init"]).status.success());
    let pid = service.status()["worker"]["pid"].as_u64().unwrap() as i32;
    unsafe {
        libc::kill(pid, libc::SIGSTOP);
    };
    std::fs::write(service.r.source.join("A.ORF"), b"raw").unwrap();
    std::fs::write(service.r.source.join("B.JPG"), b"jpeg").unwrap();
    assert!(
        service
            .cli(&["import", service.r.source.to_str().unwrap()])
            .status
            .success()
    );
    let status = service.status();
    let paths = status["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["group"]["members"].as_array().unwrap())
        .map(|m| m["path"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let raw = paths.iter().find(|p| p.ends_with("A.ORF")).unwrap();
    let jpeg = paths.iter().find(|p| p.ends_with("B.JPG")).unwrap();
    let target = service.r.archive.join(raw);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, b"foreign original").unwrap();
    unsafe {
        libc::kill(pid, libc::SIGCONT);
    };
    let begin = Instant::now();
    loop {
        if std::fs::read(service.r.archive.join(jpeg)).is_ok_and(|bytes| bytes == b"jpeg") {
            break;
        }
        assert!(
            begin.elapsed() < Duration::from_secs(7),
            "collision blocked unrelated publication"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(std::fs::read(target).unwrap(), b"foreign original");
    assert_eq!(
        std::fs::read(service.r.source.join("A.ORF")).unwrap(),
        b"raw"
    );
}
#[test]
fn archive_verify_reads_actual_objects_without_repair() {
    let service = ServiceFixture::new();
    assert!(service.cli(&["archive", "init"]).status.success());
    std::fs::write(service.r.source.join("A.ORF"), b"good").unwrap();
    assert!(
        service
            .cli(&[
                "import",
                service.r.source.to_str().unwrap(),
                "--wait-for",
                "archived"
            ])
            .status
            .success()
    );
    let status = service.status();
    let path = status["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["group"]["members"].as_array().unwrap())
        .next()
        .unwrap()["path"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::write(service.r.archive.join(&path), b"evil").unwrap();
    let out = service.cli(&["verify", "--archive"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        std::fs::read(service.r.archive.join(path)).unwrap(),
        b"evil"
    );
}
#[test]
fn online_rebuild_creates_separate_state_and_reports_lost_private_intents() {
    let mut service = ServiceFixture::new();
    assert!(service.cli(&["archive", "init"]).status.success());
    std::fs::write(service.r.source.join("A.JPG"), b"jpeg").unwrap();
    assert!(
        service
            .cli(&[
                "import",
                service.r.source.to_str().unwrap(),
                "--wait-for",
                "archived"
            ])
            .status
            .success()
    );
    std::fs::write(service.r.local.join("private.bin"), b"private bytes").unwrap();
    let replacement = service.r._temp.path().join("replacement");
    std::fs::create_dir(&replacement).unwrap();
    let out = service.cli(&["catalog", "rebuild", replacement.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}; service: {:?}",
        String::from_utf8_lossy(&out.stdout),
        service.child.try_wait()
    );
    assert!(replacement.join("catalog.sqlite").exists());
    assert!(replacement.join("preserved/live-catalog.sqlite").exists());
    assert_eq!(
        std::fs::read(service.r.local.join("private.bin")).unwrap(),
        b"private bytes"
    );
    assert!(service.r.state.join("catalog.sqlite").exists());
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(replacement.join("recovery.json")).unwrap()).unwrap();
    assert_eq!(report["private_state_lost"], true);
    assert!(report["local_observations"].as_u64().unwrap() >= 2);
}
#[test]
fn cancellation_waits_for_confirmed_worker_completion_and_keeps_service_alive() {
    let service = ServiceFixture::new();
    let deadline = Instant::now();
    loop {
        let status = service.status();
        if status["worker"]["available"] == true && status["archive"]["reason"].is_string() {
            break;
        }
        assert!(
            deadline.elapsed() < Duration::from_secs(5),
            "initial read-only probe did not finish: {status}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = service.status()["worker"]["pid"].as_u64().unwrap() as i32;
    unsafe {
        libc::kill(pid, libc::SIGSTOP);
    }
    let out = service.cli(&["archive", "init", "--no-wait"]);
    let response: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let job = response["outcome"]["job"].as_str().unwrap();
    let dispatch = Instant::now();
    while service.status()["worker"]["control_job"] != job {
        assert!(dispatch.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    service.cli(&["jobs", "cancel", job]);
    assert!(
        service.status()["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == job && r["state"] == "CancelRequested")
    );
    unsafe {
        libc::kill(pid, libc::SIGCONT);
    }
    let deadline = Instant::now();
    loop {
        if service.status()["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == job && r["state"] == "Cancelled")
        {
            break;
        }
        assert!(
            deadline.elapsed() < Duration::from_secs(5),
            "confirmed operation did not finish cancellation"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::write(service.r.source.join("B.JPG"), b"other").unwrap();
    assert!(
        service
            .cli(&["import", service.r.source.to_str().unwrap()])
            .status
            .success()
    );
}
#[test]
fn pin_reserves_before_nas_dispatch_and_status_does_not_hash_resident_raw() {
    let service = ServiceFixture::new();
    assert!(service.cli(&["archive", "init"]).status.success());
    std::fs::write(service.r.source.join("A.ORF"), b"raw bytes").unwrap();
    assert!(
        service
            .cli(&[
                "import",
                service.r.source.to_str().unwrap(),
                "--wait-for",
                "archived"
            ])
            .status
            .success()
    );
    let status = service.status();
    let group = status["groups"][0]["group"]["id"].as_str().unwrap();
    let path = status["groups"][0]["group"]["members"][0]["path"]
        .as_str()
        .unwrap();
    std::fs::remove_file(service.r.local.join(path)).unwrap();
    let pid = status["worker"]["pid"].as_u64().unwrap() as i32;
    unsafe {
        libc::kill(pid, libc::SIGSTOP);
    }
    service.cli(&["pin", group, "--no-wait"]);
    let start = Instant::now();
    loop {
        if service.status()["space"]["remaining_reservations"]
            .as_u64()
            .unwrap()
            >= 9
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "pin dispatch omitted reservation"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        libc::kill(pid, libc::SIGCONT);
    }
    let deadline = Instant::now();
    while std::fs::read(service.r.local.join(path)).ok().as_deref() != Some(b"raw bytes") {
        assert!(deadline.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[test]
fn source_companion_has_verified_owner_and_orphan_has_resolvable_conflict() {
    use photo_sorter::archive::{ArchiveRecord, CommitEnvelope};
    let service = ServiceFixture::new();
    assert!(service.cli(&["archive", "init"]).status.success());
    std::fs::write(service.r.source.join("A.ORF"), b"raw").unwrap();
    std::fs::write(
        service.r.source.join("A.ORF.rrdata"),
        br#"{"version":1,"rating":3,"adjustments":null,"tags":null}"#,
    )
    .unwrap();
    std::fs::write(service.r.source.join("unknown.orig"), b"opaque evidence").unwrap();
    let out = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let status = service.status();
    let raw_path = status["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["group"]["members"].as_array().unwrap())
        .find(|m| m["path"].as_str().unwrap().ends_with("A.ORF"))
        .unwrap()["path"]
        .as_str()
        .unwrap();
    let config = Config::load(&service.profile).unwrap();
    let mut owned = false;
    for entry in std::fs::read_dir(
        service
            .r
            .archive
            .join(format!(".photo-sorter/commits/{}", config.writer_id)),
    )
    .unwrap()
    {
        let entry = entry.unwrap();
        if !entry.file_type().unwrap().is_file() {
            continue;
        }
        let commit = CommitEnvelope::decode(
            &std::fs::read(entry.path()).unwrap(),
            config.archive_id.unwrap(),
        )
        .unwrap();
        owned|=commit.records().unwrap().iter().any(|r| matches!(r,ArchiveRecord::SidecarCreated{group:Some(_),path,..} if path.as_str()==format!("{raw_path}.rrdata")));
    }
    assert!(owned, "verified companion was left unassociated");
    assert!(
        status["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["kind"] == "orphan_sidecar")
    );
}
#[test]
fn cached_pin_requires_fresh_shared_namespace_before_latest_ready() {
    let service = ServiceFixture::new();
    assert!(service.cli(&["archive", "init"]).status.success());
    std::fs::write(service.r.source.join("A.JPG"), b"jpeg").unwrap();
    assert!(
        service
            .cli(&[
                "import",
                service.r.source.to_str().unwrap(),
                "--wait-for",
                "archived"
            ])
            .status
            .success()
    );
    let status = service.status();
    let group = status["groups"][0]["group"]["id"].as_str().unwrap();
    std::fs::rename(&service.r.archive, service.r._temp.path().join("offline")).unwrap();
    let out = service.cli(&["pin", group]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        service.status()["groups"][0]["readiness"]["originals_ready"]
            .as_bool()
            .unwrap()
    );
}
#[test]
fn fresh_client_reads_records_once_then_catches_up_without_refetch() {
    let original = ServiceFixture::new();
    assert!(original.cli(&["archive", "init"]).status.success());
    std::fs::write(original.r.source.join("A.JPG"), b"jpeg").unwrap();
    assert!(
        original
            .cli(&[
                "import",
                original.r.source.to_str().unwrap(),
                "--wait-for",
                "archived"
            ])
            .status
            .success()
    );
    let archive: ArchiveId =
        serde_json::from_value(original.status()["archive_id"].clone()).unwrap();
    let peer = ServiceFixture::with_config(|config, _| {
        config.archive_root = original.r.archive.clone();
        config.archive_id = Some(archive);
    });
    let deadline = Instant::now();
    let status = loop {
        let status = peer.status();
        if !status["groups"].as_array().unwrap().is_empty() {
            break status;
        }
        assert!(
            deadline.elapsed() < Duration::from_secs(7),
            "startup did not replay shared namespace"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let group = status["groups"][0]["group"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let reads = status["session_metrics"]["journal_reads"].as_u64().unwrap();
    assert!(reads > 0);
    let out = peer.cli(&["pin", &group]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        peer.status()["session_metrics"]["journal_reads"]
            .as_u64()
            .unwrap(),
        reads
    );
    assert!(peer.cli(&["pin", &group]).status.success());
    assert_eq!(
        peer.status()["session_metrics"]["journal_reads"]
            .as_u64()
            .unwrap(),
        reads
    );
}
#[test]
fn status_and_conflicts_have_bounded_resumable_pages() {
    let fixture = ServiceFixture::with_config(|_, r| {
        let root =
            photo_sorter::fs::RootHandle::open(&r.state, photo_sorter::fs::RootPolicy::Local)
                .unwrap();
        let mut catalog = photo_sorter::catalog::Catalog::open(&root).unwrap();
        for n in 0..260 {
            catalog
                .record_ambiguity("fixture", None, serde_json::json!({"index":n}))
                .unwrap();
        }
    });
    let status = fixture.status();
    let first = status["conflicts"].as_array().unwrap();
    assert_eq!(first.len(), 128);
    assert_eq!(status["pagination"]["conflicts"]["has_more"], true);
    let after = first.last().unwrap()["id"].as_str().unwrap();
    let out = fixture.cli(&["status", "--conflicts-after", after]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let next: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        next["outcome"]["data"]["conflicts"]
            .as_array()
            .unwrap()
            .len(),
        128
    );
    assert_ne!(
        next["outcome"]["data"]["conflicts"][0]["id"],
        first[0]["id"]
    );
    let out = fixture.cli(&["conflicts", "list", "--after", after, "--limit", "128"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let next: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        next["outcome"]["data"]["conflicts"]
            .as_array()
            .unwrap()
            .len(),
        128
    );
}
#[test]
fn capacity_pressure_reclaims_old_archived_lru_before_direct_import() {
    let fixture = ServiceFixture::with_config(|config, _| {
        config.budget = 3 * 1024 * 1024;
        config.activity_hold_seconds = 0;
    });
    let init = fixture.cli(&["archive", "init"]);
    assert!(init.status.success());
    std::fs::write(fixture.r.source.join("OLD.ORF"), vec![1; 1200 * 1024]).unwrap();
    let out = fixture.cli(&[
        "import",
        fixture.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        out.status.success(),
        "{} status {}",
        String::from_utf8_lossy(&out.stdout),
        fixture.status()
    );
    let old = fixture.status()["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["group"]["members"].as_array().unwrap())
        .find(|m| m["path"].as_str().unwrap().ends_with("OLD.ORF"))
        .unwrap()["path"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::remove_file(fixture.r.source.join("OLD.ORF")).unwrap();
    std::fs::write(fixture.r.source.join("NEW.ORF"), vec![2; 1500 * 1024]).unwrap();
    let out = fixture.cli(&["import", fixture.r.source.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{} status {}",
        String::from_utf8_lossy(&out.stdout),
        fixture.status()
    );
    let new = fixture.status()["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["group"]["members"].as_array().unwrap())
        .find(|m| m["path"].as_str().unwrap().ends_with("NEW.ORF"))
        .unwrap()["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        fixture.r.local.join(&new).exists(),
        "new original should be admitted locally after reclaiming eligible archived LRU"
    );
    assert!(!fixture.r.local.join(&old).exists());
    assert!(fixture.r.archive.join(old).exists());
}
#[test]
fn unchanged_owned_source_companion_reuses_archived_history_without_new_commit() {
    let service = ServiceFixture::new();
    assert!(service.cli(&["archive", "init"]).status.success());
    std::fs::write(service.r.source.join("A.ORF"), b"raw").unwrap();
    std::fs::write(
        service.r.source.join("A.ORF.rrdata"),
        br#"{"version":1,"rating":3,"adjustments":null}"#,
    )
    .unwrap();
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
    let count = || {
        std::fs::read_dir(service.r.archive.join(".photo-sorter/commits"))
            .unwrap()
            .map(|directory| {
                std::fs::read_dir(directory.unwrap().path())
                    .unwrap()
                    .count()
            })
            .sum::<usize>()
    };
    let before = count();
    let second = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stdout)
    );
    assert_eq!(
        count(),
        before,
        "an unchanged owned companion must reuse exact committed version evidence"
    );
}
#[test]
fn oversized_saved_opaque_content_archives_coherently_without_local_snapshot_duplication() {
    let service = ServiceFixture::with_config(|config, _| {
        config.budget = 4 * 1024 * 1024;
    });
    assert!(service.cli(&["archive", "init"]).status.success());
    std::fs::write(service.r.source.join("A.ORF"), b"raw").unwrap();
    let out = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(out.status.success());
    let path = service.status()["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["group"]["members"].as_array().unwrap())
        .find(|m| m["path"].as_str().unwrap().ends_with("A.ORF"))
        .unwrap()["path"]
        .as_str()
        .unwrap()
        .to_owned();
    let bytes = vec![42; 3 * 1024 * 1024];
    let digest = Digest::of(&bytes);
    let live = service.r.local.join(format!("{path}.orig"));
    std::fs::write(&live, &bytes).unwrap();
    let history = service.r.archive.join(
        photo_sorter::archive::history_path(&digest)
            .unwrap()
            .as_str(),
    );
    let deadline = Instant::now();
    while !history.exists() {
        assert!(
            deadline.elapsed() < Duration::from_secs(15),
            "opaque history did not archive: {}",
            service.status()
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    let deadline = Instant::now();
    while !service.r.archive.join(format!("{path}.orig")).exists() {
        assert!(
            deadline.elapsed() < Duration::from_secs(10),
            "history was not journaled/projected: {}",
            service.status()
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(std::fs::read(history).unwrap(), bytes);
    assert_eq!(std::fs::read(&live).unwrap(), bytes);
    assert!(
        !service
            .r
            .state
            .join(photo_sorter::edit::snapshot_path(&digest).unwrap().as_str())
            .exists(),
        "budget admission must use direct history rather than duplicate a large saved file locally"
    );
    let status = service.status();
    let group = status["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| {
            g["group"]["members"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["path"].as_str() == Some(&path))
        })
        .unwrap()["group"]["id"]
        .as_str()
        .unwrap();
    let pin = service.cli(&["pin", group]);
    assert!(
        pin.status.success(),
        "already-applied direct history should satisfy pin after strong live-byte verification: {}",
        String::from_utf8_lossy(&pin.stdout)
    );
    let observer = service.status()["worker"]["observation_pid"]
        .as_i64()
        .unwrap() as i32;
    assert_eq!(unsafe { libc::kill(observer, libc::SIGSTOP) }, 0);
    let changed = vec![43; bytes.len()];
    std::fs::write(&live, &changed).unwrap();
    let pin = service.cli(&["pin", group]);
    assert_eq!(
        pin.status.code(),
        Some(3),
        "changed live bytes cannot satisfy ready without explicit refresh: {}",
        String::from_utf8_lossy(&pin.stdout)
    );
    assert_eq!(std::fs::read(&live).unwrap(), changed);
    assert_eq!(unsafe { libc::kill(observer, libc::SIGCONT) }, 0);
}
#[test]
fn periodic_local_proofs_detect_missing_and_changed_originals_without_refetch() {
    let service = ServiceFixture::with_config(|config, _| {
        config.reconcile_seconds = 1;
    });
    assert!(service.cli(&["archive", "init"]).status.success());
    std::fs::write(service.r.source.join("A.ORF"), b"raw").unwrap();
    assert!(
        service
            .cli(&[
                "import",
                service.r.source.to_str().unwrap(),
                "--wait-for",
                "archived"
            ])
            .status
            .success()
    );
    let status = service.status();
    let group = status["groups"][0]["group"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = status["groups"][0]["group"]["members"][0]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::remove_file(service.r.local.join(&path)).unwrap();
    let deadline = Instant::now();
    loop {
        let status = service.status();
        let group = status["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["group"]["id"].as_str() == Some(&group))
            .unwrap();
        if group["readiness"]["originals_ready"] == false {
            break;
        }
        assert!(
            deadline.elapsed() < Duration::from_secs(5),
            "periodic verification left a missing file ready"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(!service.r.local.join(&path).exists());
    std::fs::write(service.r.local.join(&path), b"bad").unwrap();
    let deadline = Instant::now();
    loop {
        let status = service.status();
        let group = status["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["group"]["id"].as_str() == Some(&group))
            .unwrap();
        if group["physical_findings"]
            .as_array()
            .is_some_and(|findings| {
                findings
                    .iter()
                    .any(|finding| finding["status"] == "Corrupt")
            })
        {
            assert_eq!(group["protection"]["pending"], true);
            break;
        }
        assert!(
            deadline.elapsed() < Duration::from_secs(5),
            "changed local bytes were not reported/protected"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(std::fs::read(service.r.local.join(&path)).unwrap(), b"bad");
    assert_eq!(
        std::fs::read(service.r.archive.join(&path)).unwrap(),
        b"raw"
    );
}
#[test]
fn rebuilt_profile_finishes_real_readonly_catchup_and_physical_reconciliation() {
    let mut service = ServiceFixture::new();
    assert!(service.cli(&["archive", "init"]).status.success());
    std::fs::write(service.r.source.join("A.ORF"), b"retained raw").unwrap();
    assert!(
        service
            .cli(&[
                "import",
                service.r.source.to_str().unwrap(),
                "--wait-for",
                "archived"
            ])
            .status
            .success()
    );
    let replacement = service.r._temp.path().join("replacement-state");
    std::fs::create_dir(&replacement).unwrap();
    let out = service.cli(&["catalog", "rebuild", replacement.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let response: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let writer: WriterId =
        serde_json::from_value(response["outcome"]["detail"]["writer"].clone()).unwrap();
    let mut config = photo_sorter::config::Config::load(&service.profile).unwrap();
    let old_writer = config.writer_id;
    service.stop();
    config.state_root = replacement.clone();
    config.writer_id = writer;
    std::fs::write(&service.profile, toml::to_string(&config).unwrap()).unwrap();
    service.restart();
    let deadline = Instant::now();
    loop {
        let db = rusqlite::Connection::open_with_flags(
            replacement.join("catalog.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let complete:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM recovery_finalizations WHERE id='rebuild-incomplete') AND NOT EXISTS(SELECT 1 FROM blocked_records WHERE id='rebuild-incomplete')",[],|r|r.get(0)).unwrap();
        if complete {
            break;
        }
        assert!(
            deadline.elapsed() < Duration::from_secs(8),
            "rebuild never finished actual reconciliation: {}",
            service.status()
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    let status = service.status();
    assert_eq!(status["groups"][0]["readiness"]["originals_ready"], true);
    assert_eq!(status["writer_id"], serde_json::to_value(writer).unwrap());
    assert_ne!(writer, old_writer);
    let marker: photo_sorter::archive::ArchiveMarker = serde_json::from_slice(
        &std::fs::read(service.r.archive.join(".photo-sorter/archive.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        marker.writer_id, old_writer,
        "read-only reconciliation cannot silently take publication ownership"
    );
}
#[test]
fn status_reports_space_audit_failure_without_walking_or_following_unknown_symlinks() {
    let service = ServiceFixture::with_config(|config, _| {
        config.reconcile_seconds = 1;
    });
    let sentinel = service.r._temp.path().join("unmanaged-sentinel");
    std::fs::write(&sentinel, b"keep").unwrap();
    std::os::unix::fs::symlink(&sentinel, service.r.local.join("unexpected.link")).unwrap();
    let deadline = Instant::now();
    loop {
        let out = service.cli(&["status"]);
        assert!(
            out.status.success(),
            "status must remain readable while space measurement is unavailable: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let response: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        if response["outcome"]["data"]["space"]["occupancy_reason"]
            .as_str()
            .is_some()
        {
            break;
        }
        assert!(deadline.elapsed() < Duration::from_secs(6));
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(
        std::fs::symlink_metadata(service.r.local.join("unexpected.link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(sentinel).unwrap(), b"keep");
}

#[test]
fn clean_waits_for_local_verification_and_keeps_status_responsive() {
    clean_with_paused_verification(false);
}
#[test]
fn pin_during_cleanup_verification_invalidates_the_unlink_proof() {
    clean_with_paused_verification(true);
}
fn clean_with_paused_verification(pin_during: bool) {
    let fixture = ServiceFixture::with_config(|config, _| {
        config.reserve = 0;
        config.activity_hold_seconds = 0;
    });
    std::fs::write(fixture.r.source.join("clean.ORF"), b"generated original").unwrap();
    assert!(fixture.cli(&["archive", "init"]).status.success());
    assert!(
        fixture
            .cli(&[
                "import",
                fixture.r.source.to_str().unwrap(),
                "--wait-for",
                "archived"
            ])
            .status
            .success()
    );
    let deadline = std::time::Instant::now();
    let status = loop {
        let status = fixture.status();
        if status["groups"][0]["physical_findings"]
            .as_array()
            .is_some_and(|f| f.is_empty())
            && status["space"]["evictable"].as_u64().unwrap_or(0) > 0
        {
            break status;
        }
        assert!(
            deadline.elapsed() < std::time::Duration::from_secs(10),
            "{status}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let original = fixture.r.local.join(
        status["groups"][0]["group"]["members"][0]["path"]
            .as_str()
            .unwrap(),
    );
    let pid = status["worker"]["maintenance_pid"].as_i64().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    let output = fixture.cli(&["clean", "--no-wait"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let status = fixture.status();
    assert!(
        status["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["request"]["kind"]["type"] == "clean" && j["state"] != "Complete"),
        "{status}"
    );
    if pin_during {
        let group = status["groups"][0]["group"]["id"].as_str().unwrap();
        assert!(fixture.cli(&["pin", group, "--no-wait"]).status.success());
        assert_eq!(fixture.status()["groups"][0]["protection"]["pinned"], true);
    }
    assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);
    let deadline = std::time::Instant::now();
    loop {
        let status = fixture.status();
        if status["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["request"]["kind"]["type"] == "clean" && j["state"] == "Complete")
        {
            break;
        }
        assert!(
            deadline.elapsed() < std::time::Duration::from_secs(15),
            "{status}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(original.exists(), pin_during);
    if pin_during {
        assert_eq!(std::fs::read(original).unwrap(), b"generated original");
    }
}

#[test]
fn clean_reclaims_hidden_companion_only_after_exact_immutable_archival() {
    let service = ServiceFixture::new();
    let body = b"opaque generated companion";
    std::fs::write(service.r.source.join("a.ORF"), b"generated raw").unwrap();
    std::fs::write(service.r.source.join("a.ORF.orig"), body).unwrap();
    assert!(service.cli(&["archive", "init"]).status.success());
    let imported = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stdout)
    );
    let digest = Digest::of(body);
    let hidden = service.r.local.join(format!(
        ".photo-sorter/companions/{}/a.ORF.orig",
        &digest.as_str()[7..]
    ));
    assert_eq!(std::fs::read(&hidden).unwrap(), body);
    let cleaned = service.cli(&["clean"]);
    assert!(
        cleaned.status.success(),
        "{}",
        String::from_utf8_lossy(&cleaned.stdout)
    );
    assert!(!hidden.exists());
    let history = photo_sorter::archive::history_path(&digest).unwrap();
    assert_eq!(
        std::fs::read(service.r.archive.join(history.as_str())).unwrap(),
        body
    );
    assert_eq!(
        std::fs::read(service.r.source.join("a.ORF.orig")).unwrap(),
        body
    );
}

#[test]
fn distinct_path_resolution_replans_real_upload_and_preserves_collision_bytes() {
    use photo_sorter::catalog::*;
    let service = ServiceFixture::new();
    let path = "RAW/2026/01/02/a.ORF";
    let original = b"generated import";
    std::fs::write(service.r.source.join("a.ORF"), original).unwrap();
    std::fs::File::open(service.r.source.join("a.ORF"))
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1767355200)),
        )
        .unwrap();
    assert!(service.cli(&["archive", "init"]).status.success());
    std::fs::create_dir_all(service.r.archive.join("RAW/2026/01/02")).unwrap();
    std::fs::write(service.r.archive.join(path), b"foreign original preserved").unwrap();
    assert!(
        service
            .cli(&["import", service.r.source.to_str().unwrap(), "--no-wait"])
            .status
            .success()
    );
    let deadline = Instant::now();
    let (conflict, group) = loop {
        let status = service.status();
        if let Some(conflict) = status["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["kind"] == "original_collision")
        {
            break (conflict.clone(), status["groups"][0]["group"].clone());
        }
        assert!(deadline.elapsed() < Duration::from_secs(10), "{status}");
        std::thread::sleep(Duration::from_millis(20));
    };
    let destination = ArchivePath::parse("RAW/2026/01/02/selected.ORF").unwrap();
    let request = ResolutionRequest {
        operation: OperationId::new(),
        conflict: serde_json::from_value(conflict["id"].clone()).unwrap(),
        expected_revision: conflict["revision"].as_u64().unwrap(),
        expected_heads: vec![],
        expected_memberships: vec![(
            serde_json::from_value(group["id"].clone()).unwrap(),
            serde_json::from_value(group["membership_revision"].clone()).unwrap(),
        )],
        resolution: ConflictResolution::ChooseDistinctPath {
            path: destination.clone(),
        },
    };
    let file = service.r._temp.path().join("resolution.json");
    std::fs::write(&file, serde_json::to_vec(&request).unwrap()).unwrap();
    let output = service.cli(&[
        "conflicts",
        "resolve",
        "--request",
        file.to_str().unwrap(),
        "--no-wait",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let deadline = Instant::now();
    loop {
        if std::fs::read(service.r.archive.join(destination.as_str()))
            .is_ok_and(|bytes| bytes == original)
        {
            break;
        }
        assert!(
            deadline.elapsed() < Duration::from_secs(15),
            "{}",
            service.status()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read(service.r.archive.join(path)).unwrap(),
        b"foreign original preserved"
    );
    assert_eq!(std::fs::read(service.r.local.join(path)).unwrap(), original);
    assert_eq!(
        std::fs::read(service.r.local.join(destination.as_str())).unwrap(),
        original
    );
    assert!(
        service
            .cli(&["conflicts", "resolve", "--request", file.to_str().unwrap()])
            .status
            .success()
    );
}

#[test]
fn clean_reclaims_only_quiescent_verified_duplicate_local_transfer_payload() {
    let service = ServiceFixture::new();
    let body = b"generated duplicate original";
    let destination = service.r.local.join("RAW/2026/01/02/a.ORF");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, body).unwrap();
    std::fs::write(service.r.source.join("a.ORF"), body).unwrap();
    std::fs::File::open(service.r.source.join("a.ORF"))
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1767355200)),
        )
        .unwrap();
    assert!(service.cli(&["archive", "init"]).status.success());
    let imported = service.cli(&[
        "import",
        service.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stdout)
    );
    fn payloads(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut result = Vec::new();
        for entry in std::fs::read_dir(root).unwrap().map(Result::unwrap) {
            if entry.file_type().unwrap().is_dir() {
                result.extend(payloads(&entry.path()));
            } else if entry.file_name() == "payload" {
                result.push(entry.path());
            }
        }
        result
    }
    let staging = service.r.local.join(".photo-sorter/staging");
    let duplicates = payloads(&staging);
    assert_eq!(duplicates.len(), 1);
    let cleaned = service.cli(&["clean"]);
    assert!(
        cleaned.status.success(),
        "{}",
        String::from_utf8_lossy(&cleaned.stdout)
    );
    assert!(payloads(&staging).is_empty());
    assert_eq!(std::fs::read(destination).unwrap(), body);
    assert_eq!(
        std::fs::read(service.r.archive.join("RAW/2026/01/02/a.ORF")).unwrap(),
        body
    );
}

#[test]
fn explicit_nas_projection_choice_preserves_unknown_bytes_and_publishes_selected_head() {
    use photo_sorter::catalog::*;
    let service = ServiceFixture::new();
    std::fs::write(service.r.source.join("a.ORF"), b"generated raw").unwrap();
    assert!(service.cli(&["archive", "init"]).status.success());
    assert!(
        service
            .cli(&[
                "import",
                service.r.source.to_str().unwrap(),
                "--wait-for",
                "archived"
            ])
            .status
            .success()
    );
    let status = service.status();
    let path = status["groups"][0]["group"]["members"][0]["path"]
        .as_str()
        .unwrap()
        .to_owned()
        + ".rrdata";
    let baseline = br#"{"version":1,"rating":3,"adjustments":{"exposure":1}}"#;
    std::fs::write(service.r.local.join(&path), baseline).unwrap();
    let deadline = Instant::now();
    while !std::fs::read(service.r.archive.join(&path)).is_ok_and(|b| b == baseline) {
        assert!(deadline.elapsed() < Duration::from_secs(8));
        std::thread::sleep(Duration::from_millis(20));
    }
    let unknown = b"unknown NAS bytes preserved";
    std::fs::write(service.r.archive.join(&path), unknown).unwrap();
    let desired = br#"{"version":1,"rating":5,"adjustments":{"exposure":2}}"#;
    std::fs::write(service.r.local.join(&path), desired).unwrap();
    let deadline = Instant::now();
    let (conflict, group) = loop {
        let status = service.status();
        if let Some(c) = status["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["kind"] == "nas_projection")
        {
            break (c.clone(), status["groups"][0]["group"].clone());
        }
        assert!(deadline.elapsed() < Duration::from_secs(12), "{status}");
        std::thread::sleep(Duration::from_millis(20));
    };
    let version = serde_json::from_value(conflict["details"]["version"].clone()).unwrap();
    let request = ResolutionRequest {
        operation: OperationId::new(),
        conflict: serde_json::from_value(conflict["id"].clone()).unwrap(),
        expected_revision: conflict["revision"].as_u64().unwrap(),
        expected_heads: vec![version],
        expected_memberships: vec![(
            serde_json::from_value(group["id"].clone()).unwrap(),
            serde_json::from_value(group["membership_revision"].clone()).unwrap(),
        )],
        resolution: ConflictResolution::ChooseHead { version },
    };
    let file = service.r._temp.path().join("choose-projection.json");
    std::fs::write(&file, serde_json::to_vec(&request).unwrap()).unwrap();
    let output = service.cli(&[
        "conflicts",
        "resolve",
        "--request",
        file.to_str().unwrap(),
        "--no-wait",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let deadline = Instant::now();
    while !std::fs::read(service.r.archive.join(&path)).is_ok_and(|b| b == desired) {
        assert!(
            deadline.elapsed() < Duration::from_secs(15),
            "{}",
            service.status()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(std::fs::read(service.r.local.join(path)).unwrap(), desired);
    fn contains(root: &std::path::Path, expected: &[u8]) -> bool {
        std::fs::read_dir(root)
            .unwrap()
            .map(Result::unwrap)
            .any(|entry| {
                if entry.file_type().unwrap().is_dir() {
                    contains(&entry.path(), expected)
                } else {
                    std::fs::read(entry.path()).is_ok_and(|body| body == expected)
                }
            })
    }
    assert!(contains(
        &service.r.archive.join(".photo-sorter/staging"),
        unknown
    ));
}

#[test]
fn all_commands_have_real_handlers_for_pins_maintenance_and_inspection() {
    let service = ServiceFixture::new();
    std::fs::write(
        service.r.source.join("commands.ORF"),
        b"generated command fixture",
    )
    .unwrap();
    assert!(service.cli(&["archive", "init"]).status.success());
    assert!(
        service
            .cli(&[
                "import",
                service.r.source.to_str().unwrap(),
                "--wait-for",
                "archived"
            ])
            .status
            .success()
    );
    let status = service.status();
    let group = status["groups"][0]["group"]["id"].as_str().unwrap();
    assert!(service.cli(&["pin", group, "--no-wait"]).status.success());
    assert_eq!(service.status()["groups"][0]["protection"]["pinned"], true);
    assert!(service.cli(&["unpin", group]).status.success());
    assert_eq!(service.status()["groups"][0]["protection"]["pinned"], false);
    let staged = service.cli(&["refresh", group, "--stage-only"]);
    assert_eq!(staged.status.code(), Some(3));
    let refreshed = service.cli(&["refresh", group, "--editor-closed"]);
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stdout)
    );
    for args in [
        vec!["verify"],
        vec!["conflicts", "list"],
        vec!["doctor"],
        vec!["service", "status"],
    ] {
        let out = service.cli(&args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    let backup = service.r._temp.path().join("commands-backup.sqlite");
    let out = service.cli(&["catalog", "backup", backup.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let database =
        rusqlite::Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(
        database
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    for job in service.status()["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|j| j["state"] == "Complete")
    {
        assert!(
            service
                .cli(&["jobs", "show", job["id"].as_str().unwrap()])
                .status
                .success()
        );
    }
}
