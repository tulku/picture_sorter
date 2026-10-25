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
#[test]
fn state_occupancy_does_not_release_sqlite_style_process_locks() {
    use std::os::fd::AsRawFd;
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let file = root.create_new(&p("catalog.sqlite")).unwrap();
    let mut lock = libc::flock {
        l_type: libc::F_WRLCK as i16,
        l_whence: libc::SEEK_SET as i16,
        l_start: 0,
        l_len: 1,
        l_pid: 0,
    };
    // The isolated fixture file remains held while another process checks the POSIX lock.
    assert_eq!(
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) },
        0
    );
    photo_sorter::fs::state_occupied_bytes(&root).unwrap();
    // The child performs only async-signal-safe system calls before _exit.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &mut lock) };
        unsafe {
            libc::_exit(if result == -1 { 0 } else { 1 });
        }
    }
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert_eq!(
        status, 0,
        "occupancy released a process-scoped database lock"
    );
}
