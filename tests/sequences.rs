mod support;
use photo_sorter::{
    catalog::*,
    domain::*,
    fs::{RootHandle, RootPolicy},
    import::*,
};
fn records(fixture: &str) -> Vec<CaptureRecord> {
    let values: Vec<serde_json::Value> = serde_json::from_str(fixture).unwrap();
    values
        .into_iter()
        .map(|value| {
            let path = value["SourceFile"].as_str().unwrap().to_owned();
            let mut meta = MetadataResult::from_json(Digest::of(path.as_bytes()), value);
            meta.camera = "synthetic-OM1-II".into();
            CaptureRecord::new(
                OriginalMember {
                    digest: meta.digest.clone(),
                    size: 1,
                    path: ArchivePath::parse(&path).unwrap(),
                    role: MediaRole::Jpeg,
                },
                meta,
            )
        })
        .collect()
}
fn burst() -> Vec<CaptureRecord> {
    records(include_str!("fixtures/exif/burst.json"))
}
fn bracket() -> Vec<CaptureRecord> {
    records(include_str!("fixtures/exif/bracket.json"))
}
#[test]
fn real_four_frame_burst() {
    for role in [MediaRole::Jpeg, MediaRole::Raw] {
        let mut r = burst();
        for entry in &mut r {
            entry.member.role = role;
        }
        let s = classify_sequences(&r, &[], &SequencePolicy::default());
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].kind, SequenceKind::Burst);
        assert_eq!(s[0].members.len(), 4);
    }
}
#[test]
fn real_three_frame_bracket() {
    let s = classify_sequences(&bracket(), &[], &SequencePolicy::default());
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].kind, SequenceKind::Bracket);
    assert_eq!(s[0].members.len(), 3);
}
#[test]
fn single_not_sequence() {
    assert!(
        classify_sequences(
            &records(include_str!("fixtures/exif/single.json")),
            &[],
            &SequencePolicy::default()
        )
        .is_empty()
    );
}
#[test]
fn same_second_and_missing_frame() {
    let mut r = burst();
    r.remove(1);
    let s = classify_sequences(&r, &[], &SequencePolicy::default());
    assert_eq!(s.len(), 1);
    assert!(s[0].incomplete);
}
#[test]
fn counter_reset_separates() {
    let mut r = burst();
    r[2].counter = Some(1);
    r[3].counter = Some(2);
    let s = classify_sequences(&r, &[], &SequencePolicy::default());
    assert_eq!(s.len(), 2);
}
#[test]
fn multiple_dcim_or_cameras_do_not_pair() {
    let mut r = burst();
    r.truncate(1);
    let mut other = r[0].clone();
    other.member.role = MediaRole::Raw;
    other.context = "DCIM/101OMSYS".into();
    r.push(other.clone());
    assert!(pair_candidates(&r).iter().all(|d| d.members.len() == 1));
    other.context = r[0].context.clone();
    other.metadata.camera = "other-camera".into();
    r[1] = other;
    assert!(pair_candidates(&r).iter().all(|d| d.members.len() == 1));
}
#[test]
fn first_dot_is_not_identity() {
    let mut r = burst();
    r.truncate(1);
    r[0].stem = "a.one".into();
    let mut other = r[0].clone();
    other.stem = "a.two".into();
    other.member.role = MediaRole::Raw;
    r.push(other);
    assert_eq!(pair_candidates(&r).len(), 2);
}
#[test]
fn split_import_stable_paths() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let id = c.create_group(true).unwrap();
    let existing = OriginalMember {
        path: ArchivePath::parse("JPEG/old/layout/P3217330.JPG").unwrap(),
        ..burst()[0].member.clone()
    };
    c.attach_member(
        id,
        existing.clone(),
        PairEvidence::UserSelected {
            reason: "test".into(),
        },
    )
    .unwrap();
    let planned = PlannedGroup {
        id,
        records: vec![burst()[0].clone()],
        sequence: Some("new_BURST".into()),
    };
    assert_eq!(
        plan_placements(&[planned], &c, "UTC").unwrap()[0]
            .member
            .path,
        existing.path
    );
}
#[test]
fn different_shutter_still_bracket() {
    let mut r = bracket();
    for record in &mut r {
        record.metadata.raw["ShutterMode"] = serde_json::json!("Mechanical");
    }
    assert_eq!(
        classify_sequences(&r, &[], &SequencePolicy::default())[0].kind,
        SequenceKind::Bracket
    );
}
#[test]
fn paired_raw_jpeg_frames_are_one_sequence() {
    let mut r = burst();
    let raw = r
        .iter()
        .map(|record| {
            let mut raw = record.clone();
            raw.member.role = MediaRole::Raw;
            raw.member.path =
                ArchivePath::parse(&raw.member.path.as_str().replace(".JPG", ".ORF")).unwrap();
            raw.member.digest = Digest::of(raw.member.path.as_str().as_bytes());
            raw
        })
        .collect::<Vec<_>>();
    r.extend(raw);
    let s = classify_sequences(&r, &[], &SequencePolicy::default());
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].members.len(), 8);
}
#[test]
fn orf_is_primary_over_ori() {
    let mut r = burst();
    r.truncate(1);
    r[0].member.role = MediaRole::Raw;
    r[0].member.path = ArchivePath::parse("DCIM/100OMSYS/a.ORF").unwrap();
    let mut ori = r[0].clone();
    ori.member.path = ArchivePath::parse("DCIM/100OMSYS/a.ORI").unwrap();
    r.push(ori);
    let groups = pair_candidates(&r);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0].primary_raw.as_ref().unwrap().as_str(),
        "DCIM/100OMSYS/a.ORF"
    );
}
#[test]
fn real_import_sequence_context_survives_batches_restart_and_shared_replay() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut catalog = Catalog::open(&root).unwrap();
    let frames = burst();
    let first = catalog.classify_import_sequences(&frames[..2]).unwrap();
    assert_eq!(first.len(), 1);
    let anchor = first[0].anchor.clone();
    drop(catalog);
    let mut catalog = Catalog::open(&root).unwrap();
    let next = catalog.classify_import_sequences(&frames[2..]).unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].anchor, anchor);
    assert!(next[0].members.contains(&frames[3].member.digest));
    let records = catalog
        .sequence_records_for(&frames[2].member.digest)
        .unwrap();
    let archive = ArchiveId::new();
    let commit =
        photo_sorter::archive::CommitEnvelope::new(archive, WriterId::new(), 1, vec![], records)
            .unwrap();
    let other = r._temp.path().join("other-state");
    std::fs::create_dir(&other).unwrap();
    let root = RootHandle::open(&other, RootPolicy::Local).unwrap();
    let mut rebuilt = Catalog::open(&root).unwrap();
    rebuilt.apply_commit(&commit).unwrap();
    let repeated = rebuilt.classify_import_sequences(&frames[1..]).unwrap();
    assert_eq!(repeated[0].anchor, anchor);
}
