mod support;
use photo_sorter::{
    catalog::Catalog,
    domain::*,
    fs::{RootHandle, RootPolicy},
    import::*,
};
use std::{fs, sync::Arc};
fn entry(r: &support::TestRig) -> SourceEntry {
    let source = r.local.join("card");
    fs::create_dir_all(&source).unwrap();
    if !source.join("a.ORF").exists() {
        fs::write(source.join("a.ORF"), b"original").unwrap();
    }
    let mut inventory =
        Inventory::open(RootHandle::open(&source, RootPolicy::Local).unwrap()).unwrap();
    inventory.next_batch(128).unwrap().remove(0)
}
fn identity(reliable: bool) -> SourceIdentity {
    SourceIdentity {
        incarnation: "test-attachment".into(),
        reliable,
    }
}
#[test]
fn repeat_known_source_zero_exif() {
    let r = support::TestRig::new();
    let entry = entry(&r);
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let first = recognize(&entry, &identity(true), &c, false).unwrap();
    c.remember_source(&entry, &identity(true), &first.digest)
        .unwrap();
    let second = recognize(&entry, &identity(true), &c, false).unwrap();
    assert!(second.shortcut);
    assert_eq!(second.hashed_bytes, 0);
}
#[test]
fn unseen_same_label_requires_hash() {
    let r = support::TestRig::new();
    let entry = entry(&r);
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let first = recognize(&entry, &identity(true), &c, false).unwrap();
    c.remember_source(&entry, &identity(true), &first.digest)
        .unwrap();
    assert!(
        !recognize(&entry, &identity(false), &c, false)
            .unwrap()
            .shortcut
    );
}
#[test]
fn strict_detects_hidden_change() {
    let r = support::TestRig::new();
    let e = entry(&r);
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let first = recognize(&e, &identity(true), &c, false).unwrap();
    c.remember_source(&e, &identity(true), &first.digest)
        .unwrap();
    let file = e.root.open_relative(&e.path, libc::O_RDWR, 0).unwrap();
    use std::io::Write;
    (&file).write_all(b"modified").unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(e.modified))
        .unwrap();
    let new = e.refresh().unwrap();
    let strict = recognize(&new, &identity(true), &c, true).unwrap();
    assert_ne!(first.digest, strict.digest);
}
#[test]
fn known_digest_reuses_exif() {
    let r = support::TestRig::new();
    let e = entry(&r);
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let first = recognize(&e, &identity(false), &c, false).unwrap();
    c.cache_metadata(&MetadataResult::from_json(
        first.digest.clone(),
        serde_json::json!({"DateTimeOriginal":"2026:05:17 23:59:59+05:00"}),
    ))
    .unwrap();
    assert!(
        recognize(&e, &identity(false), &c, false)
            .unwrap()
            .metadata
            .is_some()
    );
}
#[test]
fn source_changes_during_read() {
    let r = support::TestRig::new();
    let e = entry(&r);
    fs::write(e.root.path().join(e.path.as_str()), b"changed length").unwrap();
    assert!(hash_source(&e).is_err());
}
#[test]
fn bad_exif_cached() {
    let r = support::TestRig::new();
    let e = entry(&r);
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let digest = hash_source(&e).unwrap();
    let mut m = MetadataResult::from_json(digest.clone(), serde_json::json!({}));
    m.error = Some("invalid EXIF".into());
    c.cache_metadata(&m).unwrap();
    assert_eq!(
        recognize(&e, &identity(false), &c, false)
            .unwrap()
            .metadata
            .unwrap()
            .error,
        Some("invalid EXIF".into())
    );
}
#[test]
fn datetime_preserves_wall_date() {
    let m = MetadataResult::from_json(
        Digest::of(b"x"),
        serde_json::json!({"DateTimeOriginal":"2026:05:17 23:59:59-07:00"}),
    );
    assert_eq!(m.capture_day.as_deref(), Some("2026/05/17"));
}
#[test]
fn filename_arguments_are_literal() {
    let r = support::TestRig::new();
    let source = r.local.join("source");
    fs::create_dir(&source).unwrap();
    for name in ["-x 'a'.ORF", "新\nfile.JPG", "a.ORF.rrdata", "a.ORF.orig"] {
        fs::write(source.join(name), b"x").unwrap();
    }
    let mut inventory =
        Inventory::open(RootHandle::open(&source, RootPolicy::Local).unwrap()).unwrap();
    let entries = inventory.next_batch(128).unwrap();
    assert_eq!(entries.len(), 4);
    assert!(inventory.next_batch(129).is_err());
    let args = ExifToolExtractor::arguments(&entries).unwrap();
    assert!(args.iter().any(|a| a == "--"));
    assert!(!args.iter().any(|a| a == "-fast2"));
    assert!(
        args.iter()
            .any(|a| a.to_string_lossy().contains("-x 'a'.ORF"))
    );
    let _ = Arc::<u8>::new(0);
}
#[test]
fn metadata_cache_does_not_invent_zero_byte_original() {
    let r = support::TestRig::new();
    let e = entry(&r);
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let digest = hash_source(&e).unwrap();
    c.cache_metadata(&MetadataResult::from_json(
        digest.clone(),
        serde_json::json!({}),
    ))
    .unwrap();
    c.remember_source(&e, &identity(true), &digest).unwrap();
    let group = c.create_group(false).unwrap();
    c.attach_member(
        group,
        OriginalMember {
            path: ArchivePath::parse("RAW/a.ORF").unwrap(),
            size: 8,
            digest,
            role: MediaRole::Raw,
        },
        photo_sorter::catalog::PairEvidence::UserSelected {
            reason: "isolated".into(),
        },
    )
    .unwrap();
    assert_eq!(c.group(group).unwrap().members[0].size, 8);
}
#[test]
fn actual_exiftool_literal_path_error_is_captured() {
    let r = support::TestRig::new();
    let source = r.source.clone();
    fs::write(source.join("-quote '新\n.ORF"), b"invalid raw fixture").unwrap();
    let mut inventory =
        Inventory::open(RootHandle::open(&source, RootPolicy::Local).unwrap()).unwrap();
    let entries = inventory.next_batch(128).unwrap();
    let results = ExifToolExtractor::default().extract(&entries).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].error.is_some());
    assert_eq!(results[0].digest, Digest::of(b"invalid raw fixture"));
}
#[test]
fn nonportable_source_name_does_not_stop_independent_import() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("bad:name.JPG"), b"preserve").unwrap();
    std::fs::write(r.source.join("good.JPG"), b"good").unwrap();
    let mut inventory = photo_sorter::import::Inventory::open(
        photo_sorter::fs::RootHandle::open(&r.source, photo_sorter::fs::RootPolicy::Local).unwrap(),
    )
    .unwrap();
    let entries = inventory.next_batch(128).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(inventory.skipped, 1);
    assert_eq!(
        std::fs::read(r.source.join("bad:name.JPG")).unwrap(),
        b"preserve"
    );
}
