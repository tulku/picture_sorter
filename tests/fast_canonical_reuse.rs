mod support;
use photo_sorter::{
    catalog::*,
    config::Config,
    domain::*,
    fs::{RootHandle, RootPolicy},
    import::*,
};

fn metadata_variant(card: &[u8], text: &[u8]) -> Vec<u8> {
    let mut bytes = card[..2].to_vec();
    bytes.extend_from_slice(b"\xff\xfe");
    bytes.extend_from_slice(&((text.len() + 2) as u16).to_be_bytes());
    bytes.extend_from_slice(text);
    bytes.extend_from_slice(&card[2..]);
    bytes
}

fn engine(r: &support::TestRig) -> ImportCoordinator {
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut config = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    config.archive_id = Some(ArchiveId::new());
    ImportCoordinator::new(config, Catalog::open(&root).unwrap())
}
fn seed(
    e: &mut ImportCoordinator,
    r: &support::TestRig,
    name: &str,
    bytes: &[u8],
    role: MediaRole,
) -> GroupId {
    let group = e.catalog.create_group(false).unwrap();
    e.catalog
        .attach_member(
            group,
            OriginalMember {
                path: ArchivePath::parse(name).unwrap(),
                digest: Digest::of(bytes),
                size: bytes.len() as u64,
                role,
            },
            PairEvidence::UserSelected {
                reason: "archive fixture".into(),
            },
        )
        .unwrap();
    let db = rusqlite::Connection::open(r.state.join("catalog.sqlite")).unwrap();
    db.execute(
        "UPDATE members SET archive=? WHERE group_id=?",
        rusqlite::params![e.config.archive_id.unwrap().to_string(), group.to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO placements(id,group_id,path,digest,archived) VALUES(?,?,?,?,1)",
        rusqlite::params![
            JobId::new().to_string(),
            group.to_string(),
            name,
            Digest::of(bytes).as_str()
        ],
    )
    .unwrap();
    group
}
fn prepare(e: &mut ImportCoordinator, r: &support::TestRig) -> ImportReport {
    e.prepare(ImportRequest {
        source_path: r.source.clone(),
        strict: false,
        dry_run: false,
        operation_id: OperationId::new(),
    })
    .unwrap()
}
#[test]
fn exact_archived_originals_reuse_flat_paths_without_creating_groups() {
    let r = support::TestRig::new();
    let mut e = engine(&r);
    for (name, bytes, role) in [
        ("A.ORF", b"raw".as_slice(), MediaRole::Raw),
        (
            "A.JPG",
            include_bytes!("fixtures/jpeg/red.jpg").as_slice(),
            MediaRole::Jpeg,
        ),
    ] {
        seed(&mut e, &r, &format!("legacy/flat/{name}"), bytes, role);
        std::fs::write(r.source.join(name), bytes).unwrap();
    }
    let db = rusqlite::Connection::open(r.state.join("catalog.sqlite")).unwrap();
    for _ in 0..2 {
        let report = prepare(&mut e, &r);
        assert_eq!(report.archive_committed, 2);
        assert_eq!(report.recognized, 2);
        assert_eq!(report.waiting, 0);
        assert_eq!(report.transfer_bytes, 0);
        assert_eq!(
            db.query_row("SELECT count(*) FROM groups", [], |row| row
                .get::<_, u64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM placements", [], |row| row
                .get::<_, u64>(0))
                .unwrap(),
            2
        );
    }
}
#[test]
fn cached_metadata_variant_reuses_canonical_receipt_with_archive_offline() {
    let r = support::TestRig::new();
    let mut e = engine(&r);
    let card = include_bytes!("fixtures/jpeg/red.jpg");
    let nas = metadata_variant(card, b"catalog");
    seed(&mut e, &r, "legacy/flat/A.JPG", &nas, MediaRole::Jpeg);
    e.catalog
        .cache_jpeg_inspection(&inspect_jpeg(nas.as_slice()).unwrap())
        .unwrap();
    std::fs::write(r.source.join("A.JPG"), card).unwrap();
    std::fs::remove_dir(&r.archive).unwrap();
    for _ in 0..2 {
        let report = prepare(&mut e, &r);
        assert_eq!(report.image_duplicates, 1);
        assert_eq!(report.archive_committed, 0);
        assert_eq!(report.pending_archive, 0);
        assert_eq!(report.waiting, 0);
        assert_eq!(report.transfer_bytes, 0);
    }
}
#[test]
fn multiple_archived_exact_paths_require_review_without_guessing() {
    let r = support::TestRig::new();
    let mut e = engine(&r);
    for name in ["one/A.ORF", "two/A.ORF"] {
        seed(&mut e, &r, name, b"raw", MediaRole::Raw);
    }
    std::fs::write(r.source.join("A.ORF"), b"raw").unwrap();
    let report = prepare(&mut e, &r);
    assert_eq!(report.conflicting, 1);
    assert_eq!(report.archive_committed, 0);
    assert_eq!(report.transfer_bytes, 0);
}

#[test]
fn dated_flat_exact_path_disambiguates_preserved_sequence_copies() {
    let r = support::TestRig::new();
    let mut e = engine(&r);
    let paths = [
        "000/duplicate/P5157589.ORF",
        "001/duplicate/P5157589.ORF",
        "RAW/2026/05/15/P5157589.ORF",
        "RAW/2026/05/15/P5157589_HDR/P5157589.ORF",
    ];
    let mut groups = Vec::new();
    for path in paths {
        groups.push(seed(&mut e, &r, path, b"raw", MediaRole::Raw));
        let destination = r.archive.join(path);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(destination, b"raw").unwrap();
    }
    e.catalog
        .cache_metadata(&MetadataResult::from_json(
            Digest::of(b"raw"),
            serde_json::json!({"DateTimeOriginal":"2026:05:15 12:00:00"}),
        ))
        .unwrap();
    std::fs::create_dir_all(r.source.join("DCIM/111OMSYS")).unwrap();
    std::fs::write(r.source.join("DCIM/111OMSYS/P5157589.ORF"), b"raw").unwrap();
    let report = prepare(&mut e, &r);
    assert_eq!(report.conflicting, 0);
    assert_eq!(report.archive_committed, 1);
    assert_eq!(report.transfer_bytes, 0);
    let db = rusqlite::Connection::open(r.state.join("catalog.sqlite")).unwrap();
    let destination: String = db
        .query_row(
            "SELECT destination FROM import_items WHERE parent=?",
            [report.job.unwrap().to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(destination, "RAW/2026/05/15/P5157589.ORF");
    assert_eq!(
        db.query_row("SELECT count(*) FROM groups", [], |row| row
            .get::<_, u64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM placements", [], |row| row
            .get::<_, u64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM aliases", [], |row| row
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    for (group, path) in groups.into_iter().zip(paths) {
        assert_eq!(
            e.catalog.group(group).unwrap().members[0].path.as_str(),
            path
        );
        assert_eq!(std::fs::read(r.archive.join(path)).unwrap(), b"raw");
    }
}

#[test]
fn ordinary_flat_path_requires_verified_matching_bytes_to_resolve_duplicates() {
    for archived_match in [false, true] {
        let r = support::TestRig::new();
        let mut e = engine(&r);
        for path in ["one/A.ORF", "two/A.ORF"] {
            seed(&mut e, &r, path, b"raw", MediaRole::Raw);
        }
        let flat = "RAW/2026/05/15/A.ORF";
        let flat_bytes = if archived_match {
            b"other".as_slice()
        } else {
            b"raw".as_slice()
        };
        seed(&mut e, &r, flat, flat_bytes, MediaRole::Raw);
        let db = rusqlite::Connection::open(r.state.join("catalog.sqlite")).unwrap();
        if !archived_match {
            db.execute("UPDATE placements SET archived=0 WHERE path=?", [flat])
                .unwrap();
        }
        e.catalog
            .cache_metadata(&MetadataResult::from_json(
                Digest::of(b"raw"),
                serde_json::json!({"DateTimeOriginal":"2026:05:15 12:00:00"}),
            ))
            .unwrap();
        std::fs::write(r.source.join("A.ORF"), b"raw").unwrap();
        let report = prepare(&mut e, &r);
        assert_eq!(report.conflicting, 1);
        assert_eq!(report.archive_committed, 0);
        assert_eq!(report.transfer_bytes, 0);
    }
}

#[test]
fn archived_burst_frames_keep_flat_paths_without_recording_new_sequence_plans() {
    let r = support::TestRig::new();
    let mut e = engine(&r);
    let metadata: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("fixtures/exif/burst.json")).unwrap();
    for role in [MediaRole::Raw, MediaRole::Jpeg] {
        for value in &metadata {
            let filename = value["SourceFile"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap();
            let name = if role == MediaRole::Raw {
                filename.replace(".JPG", ".ORF")
            } else {
                filename.to_owned()
            };
            let bytes = if role == MediaRole::Raw {
                name.as_bytes().to_vec()
            } else {
                metadata_variant(include_bytes!("fixtures/jpeg/red.jpg"), name.as_bytes())
            };
            seed(
                &mut e,
                &r,
                &format!("legacy/2026/03/21/{name}"),
                &bytes,
                role,
            );
            e.catalog
                .cache_metadata(&MetadataResult::from_json(
                    Digest::of(&bytes),
                    value.clone(),
                ))
                .unwrap();
            std::fs::write(r.source.join(&name), &bytes).unwrap();
        }
    }
    let report = prepare(&mut e, &r);
    assert_eq!(report.archive_committed, 8);
    assert_eq!(report.waiting, 0);
    let db = rusqlite::Connection::open(r.state.join("catalog.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM sequence_members", [], |row| row
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM groups", [], |row| row
            .get::<_, u64>(0))
            .unwrap(),
        8
    );
}

#[test]
fn cached_image_matches_at_multiple_paths_require_review() {
    let r = support::TestRig::new();
    let mut e = engine(&r);
    let card = include_bytes!("fixtures/jpeg/red.jpg");
    let nas = metadata_variant(card, b"catalog");
    for name in ["one/A.JPG", "two/A.JPG"] {
        seed(&mut e, &r, name, &nas, MediaRole::Jpeg);
    }
    e.catalog
        .cache_jpeg_inspection(&inspect_jpeg(nas.as_slice()).unwrap())
        .unwrap();
    std::fs::write(r.source.join("A.JPG"), card).unwrap();
    let report = prepare(&mut e, &r);
    assert_eq!(report.conflicting, 1);
    assert_eq!(report.image_duplicates, 0);
    assert_eq!(report.transfer_bytes, 0);
}

#[test]
fn changed_pixels_do_not_reuse_cached_image_receipts() {
    let r = support::TestRig::new();
    let mut e = engine(&r);
    let nas = include_bytes!("fixtures/jpeg/red.jpg");
    seed(&mut e, &r, "legacy/flat/A.JPG", nas, MediaRole::Jpeg);
    e.catalog
        .cache_jpeg_inspection(&inspect_jpeg(nas.as_slice()).unwrap())
        .unwrap();
    std::fs::write(
        r.source.join("A.JPG"),
        include_bytes!("fixtures/jpeg/blue.jpg"),
    )
    .unwrap();
    let report = prepare(&mut e, &r);
    assert_eq!(report.image_duplicates, 0);
    assert_eq!(report.archive_committed, 0);
    assert_eq!(report.waiting, 1);
}

#[test]
fn missing_archive_image_fingerprint_uses_guarded_comparison_across_layouts() {
    let r = support::TestRig::new();
    let mut e = engine(&r);
    let card = include_bytes!("fixtures/jpeg/red.jpg");
    let nas = metadata_variant(card, b"catalog");
    seed(&mut e, &r, "legacy/flat/A.JPG", &nas, MediaRole::Jpeg);
    let facts =
        serde_json::json!({"Make":"OM","Model":"T","DateTimeOriginal":"2026:05:15 12:00:00"});
    for bytes in [card.as_slice(), nas.as_slice()] {
        e.catalog
            .cache_metadata(&MetadataResult::from_json(Digest::of(bytes), facts.clone()))
            .unwrap();
    }
    std::fs::write(r.source.join("A.JPG"), card).unwrap();
    let report = prepare(&mut e, &r);
    assert_eq!(report.pending_jpeg_checks, 1);
    assert_eq!(report.image_duplicates, 0);
    assert_eq!(report.transfer_bytes, 0);
}

#[test]
fn cached_retired_plan_prefers_canonical_metadata_with_archive_offline() {
    let r = support::TestRig::new();
    let mut e = engine(&r);
    let card = include_bytes!("fixtures/jpeg/red.jpg");
    let nas = metadata_variant(card, b"catalog");
    let canonical = "legacy/flat/A.JPG";
    let reserved = "legacy/flat/A_CAMERA_ORIGINAL.JPG";
    seed(&mut e, &r, canonical, &nas, MediaRole::Jpeg);
    let reserved_group = seed(&mut e, &r, reserved, card, MediaRole::Jpeg);
    e.catalog
        .cache_jpeg_inspection(&inspect_jpeg(nas.as_slice()).unwrap())
        .unwrap();
    std::fs::write(r.source.join("A.JPG"), card).unwrap();
    let old_job = e
        .begin_scan(&ImportRequest {
            source_path: r.source.clone(),
            strict: false,
            dry_run: false,
            operation_id: OperationId::new(),
        })
        .unwrap();
    let item = JobId::new();
    let db = rusqlite::Connection::open(r.state.join("catalog.sqlite")).unwrap();
    db.execute("INSERT INTO import_items(id,parent,source_root,source_path,digest,size,role,group_id,destination,state) VALUES(?,?,?,?,?,?,?,?,?,'pending')",rusqlite::params![item.to_string(),old_job.to_string(),r.source.to_str().unwrap(),"A.JPG",Digest::of(card).as_str(),card.len() as u64,serde_json::to_string(&SourceRole::Original(MediaRole::Jpeg)).unwrap(),reserved_group.to_string(),reserved]).unwrap();
    db.execute("INSERT INTO retired_import_plans(item,replacement,operation,previous) VALUES(?,?,?,?)",rusqlite::params![JobId::new().to_string(),item.to_string(),OperationId::new().to_string(),serde_json::json!({"destination":canonical,"digest":Digest::of(card),"size":card.len(),"local_ready":false}).to_string()]).unwrap();
    std::fs::create_dir_all(r.archive.join("legacy/flat")).unwrap();
    std::fs::write(r.archive.join(canonical), &nas).unwrap();
    std::fs::write(r.archive.join(reserved), card).unwrap();
    let offline = r._temp.path().join("offline-nas");
    std::fs::rename(&r.archive, &offline).unwrap();
    for _ in 0..2 {
        let report = prepare(&mut e, &r);
        assert_eq!(report.image_duplicates, 1);
        assert_eq!(report.archive_committed, 0);
        assert_eq!(report.pending_jpeg_checks, 0);
        assert_eq!(report.waiting, 0);
        assert_eq!(report.transfer_bytes, 0);
        let path:String=db.query_row("SELECT canonical_path FROM jpeg_duplicates WHERE item IN(SELECT id FROM import_items WHERE parent=?)",[report.job.unwrap().to_string()],|row|row.get(0)).unwrap();
        assert_eq!(path, canonical);
    }
    assert_eq!(std::fs::read(offline.join(canonical)).unwrap(), nas);
    assert_eq!(std::fs::read(offline.join(reserved)).unwrap(), card);
    assert_eq!(
        db.query_row("SELECT count(*) FROM groups", [], |row| row
            .get::<_, u64>(0))
            .unwrap(),
        2
    );
}
