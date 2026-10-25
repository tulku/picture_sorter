mod support;
use std::{os::unix::fs::PermissionsExt, process::Command};
#[test]
fn runner_quotes_space_in_camera_path() {
    let r = support::TestRig::new();
    let fixture = r._temp.path().join("fixture executable");
    let output = r.state.join("args");
    let profile = r.state.join("explicit profile.toml");
    std::fs::write(&profile, b"fixture").unwrap();
    let card = r._temp.path().join("Camera card with spaces");
    std::fs::create_dir(&card).unwrap();
    std::fs::write(
        &fixture,
        "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" > \"$CAPTURE_ARGS\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();
    let status = Command::new("bash")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/run.sh"))
        .arg(&profile)
        .arg(&card)
        .arg("--no-wait")
        .env("PHOTO_SORTER_BIN", &fixture)
        .env("CAPTURE_ARGS", &output)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(output)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec![
            "--config",
            profile.to_str().unwrap(),
            "import",
            card.to_str().unwrap(),
            "--no-wait"
        ]
    );
}
#[test]
fn runner_missing_config_does_not_initialize() {
    let r = support::TestRig::new();
    let missing = r.state.join("missing.toml");
    let output = Command::new("bash")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/run.sh"))
        .arg(&missing)
        .arg(&r.source)
        .env("PHOTO_SORTER_BIN", env!("CARGO_BIN_EXE_photo_sorter"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!missing.exists());
    assert_eq!(std::fs::read_dir(&r.state).unwrap().count(), 0);
}
#[test]
fn example_config_parses_with_temp_roots() {
    let r = support::TestRig::new();
    let example = include_str!("../config.example.toml");
    let mut value: toml::Value = toml::from_str(example).unwrap();
    for (key, path) in [
        ("working_root", &r.local),
        ("state_root", &r.state),
        ("archive_root", &r.archive),
    ] {
        value[key] = toml::Value::String(path.to_str().unwrap().into());
    }
    let profile = r._temp.path().join("profile.toml");
    std::fs::write(&profile, toml::to_string(&value).unwrap()).unwrap();
    let config = photo_sorter::config::Config::load(&profile).unwrap();
    assert!(!config.local_test_backend);
    assert_eq!(config.budget, 70 * 1024 * 1024 * 1024);
    assert!(config.immich.is_none());
    assert!(config.capability_evidence.is_empty());
}
#[test]
fn unit_points_to_documented_entrypoint() {
    let unit = include_str!("../packaging/photo-sorter.service");
    assert!(unit.contains("ExecStart=@BINDIR@/photo_sorter --config @PROFILE@ service run"));
    assert!(unit.contains("KillMode=control-group"));
    assert!(!unit.contains("/home/tulku"));
    let docs = include_str!("../docs/usage.md");
    assert!(docs.contains("@BINDIR@"));
    assert!(docs.contains("@PROFILE@"));
}
