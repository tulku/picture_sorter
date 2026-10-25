mod support;
use photo_sorter::{
    domain::ArchivePath,
    fs::{PublishOutcome, RootHandle, RootPolicy, publish_noreplace},
};
use std::{
    ffi::OsString,
    fs,
    os::unix::{ffi::OsStringExt, fs::symlink},
};
fn p(s: &str) -> ArchivePath {
    ArchivePath::parse(s).unwrap()
}
#[test]
fn reject_escape_and_symlink_swap() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    fs::write(r.state.join("sentinel"), b"safe").unwrap();
    symlink(&r.state, r.local.join("link")).unwrap();
    assert!(root.create_new(&p("link/sentinel")).is_err());
    for bad in ["../state/sentinel", "/absolute", "a//b", "a/./b", "a\\b"] {
        assert!(ArchivePath::parse(bad).is_err());
    }
    assert_eq!(fs::read(r.state.join("sentinel")).unwrap(), b"safe");
}
#[test]
fn no_replace_existing() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    root.write_new(&p("stage"), b"new").unwrap();
    root.write_new(&p("dest"), b"old").unwrap();
    assert_eq!(
        publish_noreplace(&root, &p("stage"), &p("dest")).unwrap(),
        PublishOutcome::Exists
    );
    assert_eq!(root.read(&p("dest"), 100).unwrap(), b"old");
    assert_eq!(
        publish_noreplace(&root, &p("stage"), &p("other")).unwrap(),
        PublishOutcome::Published
    );
}
#[test]
fn mountpoint_replacement_does_not_fall_through() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    fs::rename(&r.local, r.state.join("old")).unwrap();
    fs::create_dir(&r.local).unwrap();
    assert!(root.validate_identity().is_err());
    assert!(root.write_new(&p("no-fallback"), b"x").is_err());
    assert!(!r.local.join("no-fallback").exists());
}
#[test]
fn unicode_and_case_collision() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    root.write_new(&p("fóto.ORF"), b"one").unwrap();
    assert_eq!(root.read(&p("fóto.ORF"), 10).unwrap(), b"one");
    assert!(root.case_collision(&p("FÓTO.orf")).unwrap());
}
#[test]
fn non_utf8_is_reported() {
    let name = OsString::from_vec(vec![0xff]);
    assert!(photo_sorter::fs::path_from_os(&name).is_err());
}
