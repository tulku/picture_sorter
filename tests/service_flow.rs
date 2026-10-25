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
            .stderr(Stdio::null())
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
            let status = self.status();
            for name in ["pid", "local_pid"] {
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
fn offline_import_edit_restart_reconnect_archive() {
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
fn actual_immich_url_mapping_fetches_through_service_and_keeps_writes_disabled() {
    let server = support::http::RatingServer::new();
    let origin = server.origin.to_string();
    let service = ServiceFixture::with_config(|config, r| {
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
}
