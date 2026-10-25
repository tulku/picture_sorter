mod support;
use photo_sorter::{domain::*, transfer::*};
fn spec(r: &support::TestRig, name: &str) -> TransferSpec {
    std::fs::write(r.source.join(name), b"original bytes").unwrap();
    let job = JobId::new();
    TransferSpec {
        job,
        generation: 1,
        source: SourceLocation {
            root: r.source.clone(),
            path: ArchivePath::parse(name).unwrap(),
        },
        destination_root: r.local.clone(),
        staged_path: staging_path(job, 1),
        expected_digest: Digest::of(b"original bytes"),
        expected_size: 14,
    }
}
#[test]
fn rsync_argv_has_no_destructive_flags() {
    let r = support::TestRig::new();
    let s = spec(&r, "a.ORF");
    let args = TransferEngine::arguments("/proc/123/fd/4", "/proc/123/fd/5/file");
    for bad in ["--delete", "--inplace", "--remove-source-files"] {
        assert!(!args.iter().any(|a| a == bad));
    }
    let receipt = TransferEngine::stage(&s).unwrap();
    assert_eq!(receipt.digest, s.expected_digest);
    assert_eq!(
        std::fs::read(r.local.join(s.staged_path.as_str())).unwrap(),
        b"original bytes"
    );
}
#[test]
fn digest_mismatch_never_verified() {
    let r = support::TestRig::new();
    let mut s = spec(&r, "a.ORF");
    s.expected_digest = Digest::of(b"different");
    assert!(TransferEngine::stage(&s).is_err());
    assert!(!r.local.join(s.staged_path.as_str()).exists());
}
#[test]
fn partial_resume_source_revalidated() {
    let r = support::TestRig::new();
    let s = spec(&r, "a.ORF");
    std::fs::create_dir_all(r.local.join(s.staged_path.as_str()).parent().unwrap()).unwrap();
    std::fs::write(r.local.join(s.staged_path.as_str()), b"partial").unwrap();
    std::fs::write(r.source.join("a.ORF"), b"changed bytes!").unwrap();
    assert!(TransferEngine::stage(&s).is_err());
    assert_eq!(
        std::fs::read(r.local.join(s.staged_path.as_str())).unwrap(),
        b"partial"
    );
}
#[test]
fn filenames_literal() {
    let r = support::TestRig::new();
    let s = spec(&r, "-quote '新\n.ORF");
    TransferEngine::stage(&s).unwrap();
    assert_eq!(
        std::fs::read(r.source.join("-quote '新\n.ORF")).unwrap(),
        b"original bytes"
    );
}
#[test]
fn stale_generation_result_ignored() {
    let r = support::TestRig::new();
    let s = spec(&r, "a.ORF");
    let receipt = TransferEngine::stage(&s).unwrap();
    assert!(!receipt.matches(s.job, 2));
    assert!(receipt.matches(s.job, 1));
}
#[test]
fn transfer_cannot_overwrite_visible_or_unknown_staging_bytes() {
    let r = support::TestRig::new();
    let mut s = spec(&r, "a.ORF");
    s.staged_path = ArchivePath::parse("visible.ORF").unwrap();
    std::fs::write(r.local.join("visible.ORF"), b"user bytes").unwrap();
    assert!(TransferEngine::stage(&s).is_err());
    assert_eq!(
        std::fs::read(r.local.join("visible.ORF")).unwrap(),
        b"user bytes"
    );
}
