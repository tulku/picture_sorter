mod support;
use photo_sorter::{config::Config, domain::OperationId};
use std::{
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use support::TestRig;
struct Running {
    child: Child,
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn config(r: &TestRig) -> std::path::PathBuf {
    let p = r._temp.path().join("profile.toml");
    let mut c = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    c.local_test_backend = true;
    std::fs::write(&p, toml::to_string(&c).unwrap()).unwrap();
    p
}
fn cli(path: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_photo_sorter"))
        .arg("--config")
        .arg(path)
        .args(args)
        .output()
        .unwrap()
}
fn start(path: &std::path::Path, state: &std::path::Path) -> Running {
    let mut child = Command::new(env!("CARGO_BIN_EXE_photo_sorter"))
        .arg("--config")
        .arg(path)
        .args(["service", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let begin = Instant::now();
    loop {
        if state.join("service.sock").exists() {
            break;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "service exited during startup"
        );
        assert!(begin.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    Running { child }
}
#[test]
fn legacy_flags_fail_explicitly() {
    let result = Command::new(env!("CARGO_BIN_EXE_photo_sorter"))
        .arg("--source")
        .arg("/fixture")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
}
#[test]
fn one_service_owns_profile() {
    let r = TestRig::new();
    let p = config(&r);
    let _service = start(&p, &r.state);
    let other = cli(&p, &["service", "run"]);
    assert_eq!(other.status.code(), Some(3));
    let status = cli(&p, &["status", "--json"]);
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
}
#[test]
fn different_profiles_cannot_own_same_working_root() {
    let r = TestRig::new();
    let p = config(&r);
    let _service = start(&p, &r.state);
    let s = r._temp.path().join("otherstate");
    std::fs::create_dir(&s).unwrap();
    let other = Config::new(r.local.clone(), s, r.archive.clone());
    let q = r._temp.path().join("other.toml");
    std::fs::write(&q, toml::to_string(&other).unwrap()).unwrap();
    assert_eq!(cli(&q, &["service", "run"]).status.code(), Some(3));
}
#[test]
fn socket_permissions_and_peer_user() {
    let r = TestRig::new();
    let p = config(&r);
    let _service = start(&p, &r.state);
    assert_eq!(
        std::fs::metadata(r.state.join("service.sock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(cli(&p, &["status", "--json"]).status.success());
}
#[test]
fn json_has_required_fields() {
    let r = TestRig::new();
    let p = config(&r);
    let _service = start(&p, &r.state);
    let result = cli(&p, &["status", "--json"]);
    assert!(result.status.success());
    let response: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let status = &response["outcome"]["data"];
    for field in [
        "protocol_version",
        "schema_version",
        "profile_id",
        "writer_id",
        "archive_id",
        "archive",
        "capabilities",
        "space",
        "jobs",
        "groups",
        "conflicts",
        "watcher",
        "worker",
        "last_successful_operations",
    ] {
        assert!(status.get(field).is_some(), "missing {field}: {response}");
    }
}
#[test]
fn dry_run_no_markers_jobs_pins_or_reservations() {
    let r = TestRig::new();
    let p = config(&r);
    let _service = start(&p, &r.state);
    let before = cli(&p, &["status", "--json"]);
    let out = cli(
        &p,
        &["import", r.source.to_str().unwrap(), "--dry-run", "--json"],
    );
    assert!(out.status.success());
    let after = cli(&p, &["status", "--json"]);
    let a: serde_json::Value = serde_json::from_slice(&before.stdout).unwrap();
    let b: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(a["outcome"]["data"]["jobs"], b["outcome"]["data"]["jobs"]);
    assert!(!r.archive.join(".photo-sorter").exists());
}
#[test]
fn request_retries_same_job() {
    let r = TestRig::new();
    let p = config(&r);
    let _service = start(&p, &r.state);
    let op = OperationId::new().to_string();
    let args = [
        "import",
        r.source.to_str().unwrap(),
        "--request-id",
        &op,
        "--no-wait",
        "--json",
    ];
    let first = cli(&p, &args);
    let second = cli(&p, &args);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(second.status.success());
    let a: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let b: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(a["outcome"]["job"], b["outcome"]["job"]);
    assert!(a["outcome"]["job"].is_string());
}
#[test]
fn oversized_or_wrong_protocol_rejected() {
    use std::io::{Read, Write};
    let r = TestRig::new();
    let p = config(&r);
    let _service = start(&p, &r.state);
    for bytes in [format!("{{\"protocol_version\":99,\"request_id\":\"{}\",\"command\":{{\"type\":\"status\"}}}}\n",OperationId::new()).into_bytes(),vec![b'x';1024*1024+1]]{let mut socket=std::os::unix::net::UnixStream::connect(r.state.join("service.sock")).unwrap();socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();socket.write_all(&bytes).unwrap();let mut out=String::new();socket.read_to_string(&mut out).unwrap();assert!(out.contains("InvalidConfig"),"{out}");}
    assert!(cli(&p, &["status"]).status.success());
}
#[test]
fn disconnect_does_not_cancel() {
    use std::io::Write;
    let r = TestRig::new();
    let p = config(&r);
    let _service = start(&p, &r.state);
    let operation = OperationId::new();
    let mut socket = std::os::unix::net::UnixStream::connect(r.state.join("service.sock")).unwrap();
    let request = serde_json::json!({"protocol_version":1,"request_id":operation,"no_wait":true,"command":{"type":"import","source":r.source,"strict":false}});
    writeln!(socket, "{request}").unwrap();
    drop(socket);
    std::thread::sleep(Duration::from_millis(100));
    let status = cli(&p, &["status", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert!(v["outcome"]["data"]["jobs"].as_array().unwrap().iter().any(
        |job| job["request"]["operation_id"] == operation.to_string()
            && job["cancel_requested"] == false
    ));
}
#[test]
fn exit_codes_distinguish_accepted_completed_blocked() {
    let r = TestRig::new();
    let p = config(&r);
    assert_eq!(cli(&p, &["status"]).status.code(), Some(3));
    let _service = start(&p, &r.state);
    assert_eq!(cli(&p, &["status"]).status.code(), Some(0));
    assert_eq!(
        cli(&p, &["refresh", "--editor-closed", "--no-wait"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        cli(&p, &["pin", "not-a-group-or-url"]).status.code(),
        Some(2)
    );
}
#[test]
fn profile_identifiers_are_required_and_legacy_help_remains_available() {
    let r = TestRig::new();
    let p = config(&r);
    let text = std::fs::read_to_string(&p).unwrap();
    let text = text
        .lines()
        .filter(|line| !line.starts_with("profile_id ="))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&p, text).unwrap();
    assert!(Config::load(&p).is_err());
    assert!(
        Command::new(env!("CARGO_BIN_EXE_photo_sorter_legacy"))
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success()
    );
}
#[test]
fn cli_reports_integrity_and_operation_failures_without_changing_bytes() {
    use photo_sorter::{catalog::*, domain::*, fs::*};
    let r = TestRig::new();
    let p = config(&r);
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut cat = Catalog::open(&state).unwrap();
    let group = cat.create_group(false).unwrap();
    cat.attach_member(
        group,
        OriginalMember {
            path: ArchivePath::parse("A.ORF").unwrap(),
            digest: Digest::of(b"good"),
            size: 4,
            role: MediaRole::Raw,
        },
        PairEvidence::UserSelected {
            reason: "fixture".into(),
        },
    )
    .unwrap();
    drop(cat);
    std::fs::write(r.local.join("A.ORF"), b"evil").unwrap();
    let _service = start(&p, &r.state);
    let verify = cli(&p, &["verify", "--json"]);
    assert_eq!(
        verify.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&verify.stdout)
    );
    assert_eq!(std::fs::read(r.local.join("A.ORF")).unwrap(), b"evil");
    let dest = r._temp.path().join("occupied.sqlite");
    std::fs::write(&dest, b"preserved").unwrap();
    let backup = cli(&p, &["catalog", "backup", dest.to_str().unwrap(), "--json"]);
    assert_eq!(
        backup.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&backup.stdout)
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"preserved");
}
