mod support;
use photo_sorter::{
    archive::*,
    catalog::*,
    domain::*,
    fs::{RootHandle, RootPolicy},
};
fn commit(archive: ArchiveId, records: Vec<ArchiveRecord>) -> CommitEnvelope {
    CommitEnvelope::new(archive, WriterId::new(), 1, vec![], records).unwrap()
}
#[test]
fn payload_digest_uses_exact_decoded_bytes() {
    let archive = ArchiveId::new();
    let mut c = commit(archive, vec![]);
    c.payload = "[  ]".into();
    c.payload_digest = Digest::of(b"[  ]");
    let bytes = c.encode().unwrap();
    assert_eq!(
        CommitEnvelope::decode(&bytes, archive).unwrap().payload,
        "[  ]"
    );
    c.payload_digest = Digest::of(b"[]");
    assert!(c.encode().is_err());
}
#[test]
fn out_of_order_dependencies() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let archive = ArchiveId::new();
    let group = GroupId::new();
    let first = commit(archive, vec![ArchiveRecord::GroupCreated { id: group }]);
    let mut second = commit(archive, vec![]);
    second.dependencies = vec![first.commit_id];
    assert_eq!(c.apply_commit(&second).unwrap(), ApplyOutcome::Deferred);
    assert!(matches!(
        c.publication_readiness().unwrap(),
        PublicationReadiness::Blocked(_)
    ));
    c.apply_commit(&first).unwrap();
    assert!(matches!(
        c.publication_readiness().unwrap(),
        PublicationReadiness::Ready
    ));
    assert_eq!(c.group(group).unwrap().members.len(), 0);
}
#[test]
fn replay_idempotent() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let group = GroupId::new();
    let commit = commit(
        ArchiveId::new(),
        vec![ArchiveRecord::GroupCreated { id: group }],
    );
    assert_eq!(c.apply_commit(&commit).unwrap(), ApplyOutcome::Applied);
    assert_eq!(
        c.apply_commit(&commit).unwrap(),
        ApplyOutcome::AlreadyApplied
    );
}
#[test]
fn duplicate_id_different_contents_rejected() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let mut first = commit(ArchiveId::new(), vec![]);
    c.apply_commit(&first).unwrap();
    first.writer_sequence = 2;
    assert!(c.apply_commit(&first).is_err());
    assert!(matches!(
        c.publication_readiness().unwrap(),
        PublicationReadiness::Blocked(_)
    ));
}
#[test]
fn unknown_or_missing_payload_blocks_publication() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let archive = ArchiveId::new();
    let mut record = commit(archive, vec![]);
    record.payload = r#"[{"type":"future_operation"}]"#.into();
    record.payload_digest = Digest::of(record.payload.as_bytes());
    let bytes = serde_json::to_vec(&record).unwrap();
    assert!(c.observe_commit(&bytes, archive).is_err());
    assert!(matches!(
        c.publication_readiness().unwrap(),
        PublicationReadiness::Blocked(_)
    ));
}
fn versions(reset: bool) {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let group = GroupId::new();
    let sidecar = SidecarId::new();
    let a = VersionId::new();
    let b = VersionId::new();
    let body = VersionBody::Content {
        digest: Digest::of(b"{}"),
        size: 2,
    };
    let record = commit(
        ArchiveId::new(),
        vec![
            ArchiveRecord::GroupCreated { id: group },
            ArchiveRecord::SidecarCreated {
                id: sidecar,
                group: Some(group),
                path: ArchivePath::parse("RAW/a.ORF.rrdata").unwrap(),
                owner: "rapidraw".into(),
            },
            ArchiveRecord::VersionAdded {
                version: Version {
                    id: a,
                    sidecar,
                    parents: vec![],
                    body: body.clone(),
                    origin: VersionOrigin::Observed,
                },
                operation: OperationId::new(),
            },
            ArchiveRecord::VersionAdded {
                version: Version {
                    id: b,
                    sidecar,
                    parents: vec![a],
                    body: if reset { VersionBody::Reset } else { body },
                    origin: VersionOrigin::Observed,
                },
                operation: OperationId::new(),
            },
        ],
    );
    c.apply_commit(&record).unwrap();
    assert_eq!(c.version(b).unwrap().parents, vec![a]);
    if reset {
        assert_eq!(c.version(b).unwrap().body, VersionBody::Reset);
    } else {
        assert_eq!(c.version(a).unwrap().body, c.version(b).unwrap().body);
        assert_ne!(a, b);
    }
}
#[test]
fn same_bytes_distinct_ancestry() {
    versions(false);
}
#[test]
fn reset_is_tombstone() {
    versions(true);
}
#[test]
fn oversized_commit_rejected() {
    let mut c = commit(ArchiveId::new(), vec![]);
    c.payload = " ".repeat(8 * 1024 * 1024);
    c.payload_digest = Digest::of(c.payload.as_bytes());
    assert!(c.encode().is_err());
}
#[test]
fn invalid_later_record_rolls_back_entire_commit() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let group = GroupId::new();
    let record = commit(
        ArchiveId::new(),
        vec![
            ArchiveRecord::GroupCreated { id: group },
            ArchiveRecord::VersionAdded {
                version: Version {
                    id: VersionId::new(),
                    sidecar: SidecarId::new(),
                    parents: vec![],
                    body: VersionBody::Reset,
                    origin: VersionOrigin::Observed,
                },
                operation: OperationId::new(),
            },
        ],
    );
    assert!(c.apply_commit(&record).is_err());
    assert!(c.group(group).is_err());
    assert!(matches!(
        c.publication_readiness().unwrap(),
        PublicationReadiness::Blocked(_)
    ));
}
