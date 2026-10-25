mod support;
use photo_sorter::{
    config::Config,
    domain::{ByteCount, Digest},
};
use support::TestRig;
#[test]
fn defaults_and_root_overlap() {
    let rig = TestRig::new();
    let mut c = Config::new(rig.local.clone(), rig.state.clone(), rig.archive.clone());
    assert_eq!(c.budget, 75_161_927_680);
    assert_eq!(c.reserve, 5_368_709_120);
    assert!(c.validate().is_ok());
    c.state_root = c.working_root.join("nested");
    assert!(c.validate().is_err());
    c.state_root = c.working_root.clone();
    assert!(c.validate().is_err());
}
#[test]
fn byte_overflow() {
    assert!(ByteCount(u64::MAX).checked_add(ByteCount(1)).is_err());
}
#[test]
fn digest_validation() {
    assert!(Digest::parse(&format!("blake3:{}", "a".repeat(64))).is_ok());
    for bad in ["abc", "blake3:ABC", &format!("blake3:{}", "A".repeat(64))] {
        assert!(Digest::parse(bad).is_err());
        assert!(serde_json::from_str::<Digest>(&format!("\"{bad}\"")).is_err());
    }
}
#[test]
fn legacy_help_still_available() {
    for executable in [
        env!("CARGO_BIN_EXE_photo_sorter"),
        env!("CARGO_BIN_EXE_photo_sorter_legacy"),
    ] {
        let output = std::process::Command::new(executable)
            .arg("--help")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
#[test]
fn profile_validation_defers_archive_filesystem_access_to_worker() {
    let r = TestRig::new();
    let a = r.source.join("archive-a");
    let b = r.source.join("archive-b");
    std::os::unix::fs::symlink(&b, &a).unwrap();
    std::os::unix::fs::symlink(&a, &b).unwrap();
    let config = Config::new(r.local.clone(), r.state.clone(), a.clone());
    assert!(config.validate().is_ok());
    assert!(
        photo_sorter::fs::RootHandle::open(
            &a,
            photo_sorter::fs::RootPolicy::Archive { expected_device: 0 }
        )
        .is_err()
    );
}
