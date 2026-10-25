mod support;
use photo_sorter::{config::Config, domain::*};
use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use support::TestRig;
struct ServiceFixture {
    r: TestRig,
    profile: std::path::PathBuf,
    child: Child,
}
impl ServiceFixture {
    fn stop(&mut self) {
        if self.child.try_wait().unwrap().is_some() {
            return;
        }
        let status = self.status();
        for name in ["pid", "local_pid", "observation_pid", "maintenance_pid"] {
            if let Some(pid) = status["worker"][name].as_u64() {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
        }
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
    fn restart(&mut self) {
        self.child = Command::new(env!("CARGO_BIN_EXE_photo_sorter"))
            .arg("--config")
            .arg(&self.profile)
            .args(["service", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now();
        loop {
            let out = self.cli(&["service", "status"]);
            if out.status.success() {
                break;
            }
            assert!(self.child.try_wait().unwrap().is_none());
            assert!(deadline.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn new() -> Self {
        Self::with_config(|_, _| {})
    }
    fn with_config(update: impl FnOnce(&mut Config, &TestRig)) -> Self {
        let r = TestRig::new();
        let profile = r._temp.path().join("profile.toml");
        let mut c = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
        c.archive_id = Some(ArchiveId::new());
        update(&mut c, &r);
        std::fs::write(&profile, toml::to_string(&c).unwrap()).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_photo_sorter"))
            .arg("--config")
            .arg(&profile)
            .args(["service", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let start = Instant::now();
        loop {
            if r.state.join("service.sock").exists() {
                break;
            }
            assert!(child.try_wait().unwrap().is_none());
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
        Self { r, profile, child }
    }
    fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_photo_sorter"))
            .arg("--config")
            .arg(&self.profile)
            .args(args)
            .arg("--json")
            .output()
            .unwrap()
    }
    fn status(&self) -> serde_json::Value {
        let out = self.cli(&["status"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["outcome"]["data"].clone()
    }
}
impl Drop for ServiceFixture {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let out = self.cli(&["status"]);
            let status = serde_json::from_slice::<serde_json::Value>(&out.stdout)
                .unwrap_or_default()["outcome"]["data"]
                .clone();
            for name in ["pid", "local_pid", "observation_pid", "maintenance_pid"] {
                if let Some(pid) = status["worker"][name].as_u64() {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
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
    let pid = service.status()["worker"]["pid"].as_u64().unwrap() as i32;
    unsafe {
        libc::kill(pid, libc::SIGSTOP);
    }
    let out = service.cli(&["archive", "init", "--no-wait"]);
    let response: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let job = response["outcome"]["job"].as_str().unwrap();
    let dispatch = Instant::now();
    while service.status()["worker"]["available"] != false {
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
