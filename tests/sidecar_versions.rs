mod support;
use photo_sorter::{
    archive::*,
    catalog::*,
    domain::*,
    edit::*,
    fs::{RootHandle, RootPolicy},
};
fn setup(r: &support::TestRig) -> (Catalog, SidecarId) {
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let group = c.create_group(false).unwrap();
    let sidecar = SidecarId::new();
    c.register_sidecar(
        sidecar,
        Some(group),
        ArchivePath::parse("RAW/a.ORF.rrdata").unwrap(),
        "rapidraw-base",
    )
    .unwrap();
    (c, sidecar)
}
fn save(
    c: &mut Catalog,
    r: &support::TestRig,
    sidecar: SidecarId,
    parents: Vec<VersionId>,
    bytes: Option<&[u8]>,
) -> VersionId {
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    VersionStore::new(c, &root)
        .record(SnapshotIntent {
            sidecar,
            parents,
            operation: OperationId::new(),
            payload: bytes
                .map(|b| SnapshotPayload::Content(b.to_vec()))
                .unwrap_or(SnapshotPayload::Reset),
            origin: VersionOrigin::Observed,
        })
        .unwrap()
}
#[test]
fn a_b_a_has_three_versions() {
    let r = support::TestRig::new();
    let (mut c, s) = setup(&r);
    let a = save(&mut c, &r, s, vec![], Some(b"A"));
    let b = save(&mut c, &r, s, vec![a], Some(b"B"));
    let again = save(&mut c, &r, s, vec![b], Some(b"A"));
    assert_ne!(a, again);
    assert_eq!(c.version(a).unwrap().body, c.version(again).unwrap().body);
    assert_eq!(c.version(again).unwrap().parents, vec![b]);
}
#[test]
fn same_bytes_different_parents_preserve_ancestry() {
    let r = support::TestRig::new();
    let (mut c, s) = setup(&r);
    let a = save(&mut c, &r, s, vec![], Some(b"A"));
    let b = save(&mut c, &r, s, vec![a], Some(b"B"));
    let x = save(&mut c, &r, s, vec![a], Some(b"A"));
    let y = save(&mut c, &r, s, vec![b], Some(b"A"));
    assert_ne!(x, y);
    assert_ne!(c.version(x).unwrap().parents, c.version(y).unwrap().parents);
}
#[test]
fn concurrent_edits_keep_both_heads() {
    let base = VersionId::new();
    let a = VersionId::new();
    let b = VersionId::new();
    match reconcile_heads(&[base], &[a], &[b]) {
        VersionDecision::Conflict(mut heads) => {
            heads.sort();
            let mut expected = vec![a, b];
            expected.sort();
            assert_eq!(heads, expected);
        }
        _ => panic!("divergence must preserve both heads"),
    }
}
#[test]
fn reset_vs_edit_conflicts() {
    let r = support::TestRig::new();
    let (mut c, s) = setup(&r);
    let base = save(&mut c, &r, s, vec![], Some(b"base"));
    let reset = save(&mut c, &r, s, vec![base], None);
    let edit = save(&mut c, &r, s, vec![base], Some(b"edit"));
    assert!(matches!(
        reconcile_heads(&[base], &[reset], &[edit]),
        VersionDecision::Conflict(_)
    ));
    assert_eq!(c.version(reset).unwrap().body, VersionBody::Reset);
}
#[test]
fn old_upload_does_not_ack_new_save() {
    let r = support::TestRig::new();
    let (mut c, s) = setup(&r);
    let a = save(&mut c, &r, s, vec![], Some(b"A"));
    let b = save(&mut c, &r, s, vec![a], Some(b"B"));
    assert!(!c.acknowledge_snapshot(s, a).unwrap());
    assert!(c.snapshot_dirty(s).unwrap());
    assert!(c.acknowledge_snapshot(s, b).unwrap());
}
fn archive(r: &support::TestRig) -> ArchiveStore {
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    let writer = WriterId::new();
    let id = ArchiveId::new();
    ArchiveStore::initialize(
        &root,
        InitArchive {
            archive_id: id,
            writer_id: writer,
        },
    )
    .unwrap();
    ArchiveStore::open(root, id, writer).unwrap()
}
#[test]
fn unknown_live_nas_bytes_preserved() {
    let r = support::TestRig::new();
    let s = archive(&r);
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    root.mkdir_all(&ArchivePath::parse("RAW").unwrap()).unwrap();
    root.write_new(
        &ArchivePath::parse("RAW/a.ORF.rrdata").unwrap(),
        b"external",
    )
    .unwrap();
    let plan = ProjectionPlan {
        sidecar: SidecarId::new(),
        path: ArchivePath::parse("RAW/a.ORF.rrdata").unwrap(),
        version: VersionId::new(),
        expected: VersionBody::Reset,
        body: VersionBody::Content {
            digest: Digest::of(b"incoming"),
            size: 8,
        },
        owner: "rapidraw-base".into(),
    };
    assert!(matches!(
        s.apply_projection(
            &s.committed_projection(&committed(&s, &plan), &plan)
                .unwrap()
        )
        .unwrap(),
        ProjectionOutcome::Conflict { .. }
    ));
    assert_eq!(
        std::fs::read(r.archive.join("RAW/a.ORF.rrdata")).unwrap(),
        b"external"
    );
}
#[test]
fn commit_before_projection_recovery() {
    let r = support::TestRig::new();
    let s = archive(&r);
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    root.write_new(
        &ArchivePath::parse(".photo-sorter/staging/payload").unwrap(),
        b"incoming",
    )
    .unwrap();
    s.store_history(
        &ArchivePath::parse(".photo-sorter/staging/payload").unwrap(),
        &Digest::of(b"incoming"),
        8,
    )
    .unwrap();
    let plan = ProjectionPlan {
        sidecar: SidecarId::new(),
        path: ArchivePath::parse("RAW/a.ORF.rrdata").unwrap(),
        version: VersionId::new(),
        expected: VersionBody::Reset,
        body: VersionBody::Content {
            digest: Digest::of(b"incoming"),
            size: 8,
        },
        owner: "rapidraw-base".into(),
    };
    assert!(matches!(
        s.apply_projection(
            &s.committed_projection(&committed(&s, &plan), &plan)
                .unwrap()
        )
        .unwrap(),
        ProjectionOutcome::Applied
    ));
    assert!(matches!(
        s.apply_projection(
            &s.committed_projection(&committed(&s, &plan), &plan)
                .unwrap()
        )
        .unwrap(),
        ProjectionOutcome::AlreadyCurrent
    ));
    assert_eq!(
        std::fs::read(r.archive.join("RAW/a.ORF.rrdata")).unwrap(),
        b"incoming"
    );
}

fn committed(s: &ArchiveStore, plan: &ProjectionPlan) -> CommitEnvelope {
    let root = RootHandle::open(s.root_path(), RootPolicy::TestArchive).unwrap();
    if let VersionBody::Content { digest, size } = &plan.body {
        let bytes = b"incoming";
        assert_eq!(*digest, Digest::of(bytes));
        assert_eq!(*size, 8);
        let stage = ArchivePath::parse(&format!(
            ".photo-sorter/staging/history-{}",
            OperationId::new()
        ))
        .unwrap();
        root.write_new(&stage, bytes).unwrap();
        s.store_history(&stage, digest, *size).unwrap();
    }
    let version = Version {
        id: plan.version,
        sidecar: plan.sidecar,
        parents: vec![],
        body: plan.body.clone(),
        origin: VersionOrigin::Derived,
    };
    let records = vec![
        ArchiveRecord::SidecarCreated {
            id: plan.sidecar,
            group: None,
            path: plan.path.clone(),
            owner: plan.owner.clone(),
        },
        ArchiveRecord::VersionAdded {
            version,
            operation: OperationId(plan.version.0),
        },
        ArchiveRecord::HeadsChanged {
            sidecar: plan.sidecar,
            expected: vec![],
            heads: vec![plan.version],
        },
        ArchiveRecord::ProjectionPlanned { plan: plan.clone() },
    ];
    let mut commit = CommitEnvelope::new(
        s.marker().archive_id,
        s.marker().writer_id,
        1,
        vec![],
        records,
    )
    .unwrap();
    commit.commit_id = CommitId(plan.version.0);
    let mut guard = s.acquire_publication().unwrap();
    loop {
        let batch = s.journal_batch(&mut guard, 128).unwrap();
        let end = batch.end;
        s.ack_journal(&mut guard, batch.token).unwrap();
        if end {
            break;
        }
    }
    s.publish_originals(
        &mut guard,
        PublicationPlan {
            commit: commit.clone(),
            staged: vec![],
            attempt: OperationId::new(),
        },
        &NoFaults,
    )
    .unwrap();
    s.release(guard).unwrap();
    commit
}

#[test]
fn resolution_names_all_heads() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let sidecar = SidecarId::new();
    let base = VersionId::new();
    let a = VersionId::new();
    let b = VersionId::new();
    let conflict = ConflictId::new();
    let mut records = vec![ArchiveRecord::SidecarCreated {
        id: sidecar,
        group: None,
        path: ArchivePath::parse("RAW/a.ORF.rrdata").unwrap(),
        owner: "rapidraw-base".into(),
    }];
    for (id, parents) in [(base, vec![]), (a, vec![base]), (b, vec![base])] {
        records.push(ArchiveRecord::VersionAdded {
            version: Version {
                id,
                sidecar,
                parents,
                body: VersionBody::Reset,
                origin: VersionOrigin::Observed,
            },
            operation: OperationId::new(),
        });
    }
    records.push(ArchiveRecord::HeadsChanged {
        sidecar,
        expected: vec![],
        heads: vec![a, b],
    });
    records.push(ArchiveRecord::ConflictRecorded {
        id: conflict,
        group: None,
        kind: "sidecar".into(),
        heads: vec![a, b],
    });
    c.apply_commit(
        &CommitEnvelope::new(ArchiveId::new(), WriterId::new(), 1, vec![], records).unwrap(),
    )
    .unwrap();
    assert!(c.resolve_conflict(conflict, &[a], a).is_err());
    let resolved = c.resolve_conflict(conflict, &[a, b], a).unwrap();
    let mut parents = c.version(resolved).unwrap().parents;
    parents.sort();
    let mut expected = vec![a, b];
    expected.sort();
    assert_eq!(parents, expected);
    assert_eq!(c.heads(sidecar).unwrap(), expected);
}
#[test]
fn retrying_old_snapshot_does_not_replace_new_local_head() {
    let r = support::TestRig::new();
    let (mut c, s) = setup(&r);
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let intent = SnapshotIntent {
        sidecar: s,
        parents: vec![],
        operation: OperationId::new(),
        payload: SnapshotPayload::Content(b"A".to_vec()),
        origin: VersionOrigin::Observed,
    };
    let a = VersionStore::new(&mut c, &root)
        .record(intent.clone())
        .unwrap();
    let b = save(&mut c, &r, s, vec![a], Some(b"B"));
    assert_eq!(VersionStore::new(&mut c, &root).record(intent).unwrap(), a);
    assert_eq!(c.local_head(s).unwrap(), Some(b));
}
#[test]
fn locally_saved_version_can_be_acknowledged_by_shared_commit() {
    let r = support::TestRig::new();
    let (mut c, s) = setup(&r);
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let intent = SnapshotIntent {
        sidecar: s,
        parents: vec![],
        operation: OperationId::new(),
        payload: SnapshotPayload::Content(b"A".to_vec()),
        origin: VersionOrigin::Observed,
    };
    let version = VersionStore::new(&mut c, &root)
        .record(intent.clone())
        .unwrap();
    let saved = c.version(version).unwrap();
    let commit = CommitEnvelope::new(
        ArchiveId::new(),
        WriterId::new(),
        1,
        vec![],
        vec![
            ArchiveRecord::VersionAdded {
                version: saved,
                operation: intent.operation,
            },
            ArchiveRecord::HeadsChanged {
                sidecar: s,
                expected: vec![],
                heads: vec![version],
            },
        ],
    )
    .unwrap();
    c.apply_commit(&commit).unwrap();
    assert_eq!(c.heads(s).unwrap(), vec![version]);
}
