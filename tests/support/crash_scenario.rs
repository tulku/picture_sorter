use photo_sorter::{archive::*, catalog::*, domain::*, edit::*, fs::*, storage::*, transfer::*};
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Serialize, Deserialize)]
pub struct Manifest {
    pub archive: ArchiveId,
    pub writer: WriterId,
    pub group: GroupId,
    pub sidecar: SidecarId,
    pub snapshot: OperationId,
    pub reset: OperationId,
    pub transfers: [JobId; 2],
    pub originals: PublicationPlan,
}
pub const SAVED: &[u8] = br#"{"version":1,"rating":3,"adjustments":{"exposure":1},"tags":null}"#;
pub fn path(s: &str) -> ArchivePath {
    ArchivePath::parse(s).unwrap()
}
pub fn members() -> Vec<OriginalMember> {
    vec![
        OriginalMember {
            path: path("RAW/a.ORF"),
            digest: Digest::of(b"raw original"),
            size: 12,
            role: MediaRole::Raw,
        },
        OriginalMember {
            path: path("JPEG/a.JPG"),
            digest: Digest::of(b"jpeg original"),
            size: 13,
            role: MediaRole::Jpeg,
        },
    ]
}
fn evidence(member: &OriginalMember) -> PairEvidence {
    PairEvidence::ObservedPlacement {
        path: member.path.clone(),
        digest: member.digest.clone(),
    }
}
pub fn setup(base: &Path) -> Manifest {
    for name in ["source", "local", "state", "archive"] {
        std::fs::create_dir_all(base.join(name)).unwrap();
    }
    let archive = ArchiveId::new();
    let writer = WriterId::new();
    let group = GroupId::new();
    let transfers = [JobId::new(), JobId::new()];
    let root = RootHandle::open(&base.join("archive"), RootPolicy::TestArchive).unwrap();
    ArchiveStore::initialize(
        &root,
        InitArchive {
            archive_id: archive,
            writer_id: writer,
        },
    )
    .unwrap();
    for (member, bytes) in members()
        .iter()
        .zip([b"raw original".as_slice(), b"jpeg original".as_slice()])
    {
        let file = base.join("source").join(member.path.as_str());
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }
    std::fs::write(base.join("source/saved.rrdata"), SAVED).unwrap();
    std::fs::create_dir_all(base.join("local/RAW")).unwrap();
    std::fs::write(base.join("local/RAW/a.ORF.rrdata"), SAVED).unwrap();
    let mut records = vec![ArchiveRecord::GroupCreated { id: group }];
    for member in members() {
        records.push(ArchiveRecord::MemberAttached {
            group,
            evidence: evidence(&member),
            member,
        });
    }
    let originals = PublicationPlan {
        commit: CommitEnvelope::new(archive, writer, 1, vec![], records).unwrap(),
        staged: members()
            .into_iter()
            .zip(transfers)
            .map(|(member, job)| StagedMember {
                member,
                staged: staging_path(job, 1),
            })
            .collect(),
        attempt: OperationId::new(),
    };
    let manifest = Manifest {
        archive,
        writer,
        group,
        sidecar: SidecarId::new(),
        snapshot: OperationId::new(),
        reset: OperationId::new(),
        transfers,
        originals,
    };
    std::fs::write(
        base.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    manifest
}
fn confined(base: &Path, name: &str) -> RootHandle {
    RootHandle::open(
        &base.join(name),
        if name == "archive" {
            RootPolicy::TestArchive
        } else {
            RootPolicy::Local
        },
    )
    .unwrap()
}
fn replay(store: &ArchiveStore, catalog: &mut Catalog) {
    let mut journal = store.read_only_journal().unwrap();
    loop {
        let batch = journal.next_batch(128).unwrap();
        for entry in batch.entries {
            catalog
                .apply_commit(&journal.read(&entry.path).unwrap())
                .unwrap();
        }
        if batch.end {
            break;
        }
    }
}
fn replay_guard(store: &ArchiveStore, catalog: &mut Catalog, guard: &mut PublicationGuard) {
    loop {
        let batch = store.journal_batch(guard, 128).unwrap();
        for entry in &batch.entries {
            catalog
                .apply_commit(&store.read_commit(guard, &entry.path).unwrap())
                .unwrap();
        }
        store.ack_journal(guard, batch.token).unwrap();
        if batch.end {
            break;
        }
    }
}
fn persisted_plan(
    root: &RootHandle,
    name: &str,
    build: impl FnOnce() -> VersionPublication,
) -> VersionPublication {
    let path = path(name);
    match root.read(&path, 8 * 1024 * 1024) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap(),
        Err(e) if e.errno == Some(libc::ENOENT) => {
            let plan = build();
            root.write_new(&path, &serde_json::to_vec(&plan).unwrap())
                .unwrap();
            plan
        }
        Err(e) => panic!("{e}"),
    }
}
pub fn run(base: &Path, faults: &dyn FaultInjector) {
    let m: Manifest =
        serde_json::from_slice(&std::fs::read(base.join("manifest.json")).unwrap()).unwrap();
    let state = confined(base, "state");
    let local = confined(base, "local");
    let archive = confined(base, "archive");
    let source = confined(base, "source");
    let store = ArchiveStore::open(archive.try_clone().unwrap(), m.archive, m.writer).unwrap();
    let mut catalog = Catalog::open(&state).unwrap();
    if !catalog
        .group_batch(None, 128)
        .unwrap()
        .iter()
        .any(|g| g.id == m.group)
    {
        catalog.insert_group(m.group, false).unwrap();
    }
    catalog
        .register_sidecar(
            m.sidecar,
            Some(m.group),
            path("RAW/a.ORF.rrdata"),
            "rapidraw-base",
        )
        .unwrap();
    replay(&store, &mut catalog);
    for (member, job) in members().iter().zip(m.transfers) {
        let spec = TransferSpec {
            job,
            generation: 1,
            source: SourceLocation {
                root: base.join("source"),
                path: member.path.clone(),
            },
            destination_root: base.join("local"),
            staged_path: staging_path(job, 1),
            expected_digest: member.digest.clone(),
            expected_size: member.size,
        };
        let receipt =
            TransferEngine::stage_confined_with_faults(&spec, &source, &local, faults).unwrap();
        publish_local(&local, &receipt, &member.path).unwrap();
        TransferEngine::stage_confined_with_faults(
            &TransferSpec {
                source: SourceLocation {
                    root: base.join("local"),
                    path: member.path.clone(),
                },
                destination_root: base.join("archive"),
                ..spec
            },
            &local,
            &archive,
            faults,
        )
        .unwrap();
    }
    let mut guard = store.acquire_publication().unwrap();
    replay_guard(&store, &mut catalog, &mut guard);
    let receipt = store
        .publish_originals(&mut guard, m.originals.clone(), faults)
        .unwrap();
    catalog
        .apply_commit_with_faults(&receipt.commit, faults)
        .unwrap();
    store.release(guard).unwrap();
    for member in members() {
        catalog
            .record_materialization(m.group, &member, true)
            .unwrap();
    }
    let version = VersionStore::new(&mut catalog, &state)
        .record_with_faults(
            SnapshotIntent {
                sidecar: m.sidecar,
                parents: vec![],
                operation: m.snapshot,
                payload: SnapshotPayload::Content(SAVED.to_vec()),
                origin: VersionOrigin::Observed,
            },
            faults,
        )
        .unwrap();
    let body = VersionBody::Content {
        digest: Digest::of(SAVED),
        size: SAVED.len() as u64,
    };
    let projection = ProjectionPlan {
        sidecar: m.sidecar,
        path: path("RAW/a.ORF.rrdata"),
        version,
        expected: VersionBody::Reset,
        body: body.clone(),
        owner: "rapidraw-base".into(),
    };
    let metadata = persisted_plan(&state, "metadata-plan.json", || VersionPublication {
        commit: CommitEnvelope::new(
            m.archive,
            m.writer,
            2,
            vec![m.originals.commit.commit_id],
            vec![
                ArchiveRecord::SidecarCreated {
                    id: m.sidecar,
                    group: Some(m.group),
                    path: projection.path.clone(),
                    owner: projection.owner.clone(),
                },
                ArchiveRecord::VersionAdded {
                    version: Version {
                        id: version,
                        sidecar: m.sidecar,
                        parents: vec![],
                        body,
                        origin: VersionOrigin::Observed,
                    },
                    operation: m.snapshot,
                },
                ArchiveRecord::HeadsChanged {
                    sidecar: m.sidecar,
                    expected: vec![],
                    heads: vec![version],
                },
                ArchiveRecord::ProjectionPlanned {
                    plan: projection.clone(),
                },
            ],
        )
        .unwrap(),
        history: vec![StagedHistory {
            staged: path(".photo-sorter/staging/saved"),
            digest: Digest::of(SAVED),
            size: SAVED.len() as u64,
        }],
        projections: vec![projection],
        attempt: OperationId::new(),
    });
    if !catalog.commit_applied(metadata.commit.commit_id).unwrap() {
        match archive.write_new(&path(".photo-sorter/staging/saved"), SAVED) {
            Ok(()) => {}
            Err(e) if e.errno == Some(libc::EEXIST) => assert_eq!(
                archive
                    .read(&path(".photo-sorter/staging/saved"), 1024)
                    .unwrap(),
                SAVED
            ),
            Err(e) => panic!("{e}"),
        }
        let mut guard = store.acquire_publication().unwrap();
        replay_guard(&store, &mut catalog, &mut guard);
        let receipt = store
            .publish_versions(&mut guard, metadata.clone(), faults)
            .unwrap();
        catalog
            .apply_commit_with_faults(&receipt.commit, faults)
            .unwrap();
        store.release(guard).unwrap();
    }
    catalog.acknowledge_snapshot(m.sidecar, version).unwrap();
    let reset = VersionStore::new(&mut catalog, &state)
        .record(SnapshotIntent {
            sidecar: m.sidecar,
            parents: vec![version],
            operation: m.reset,
            payload: SnapshotPayload::Reset,
            origin: VersionOrigin::Derived,
        })
        .unwrap();
    let reset_plan = persisted_plan(&state, "reset-plan.json", || {
        let plan = ProjectionPlan {
            sidecar: m.sidecar,
            path: path("RAW/a.ORF.rrdata"),
            version: reset,
            expected: VersionBody::Content {
                digest: Digest::of(SAVED),
                size: SAVED.len() as u64,
            },
            body: VersionBody::Reset,
            owner: "rapidraw-base".into(),
        };
        VersionPublication {
            commit: CommitEnvelope::new(
                m.archive,
                m.writer,
                3,
                vec![metadata.commit.commit_id],
                vec![
                    ArchiveRecord::VersionAdded {
                        version: Version {
                            id: reset,
                            sidecar: m.sidecar,
                            parents: vec![version],
                            body: VersionBody::Reset,
                            origin: VersionOrigin::Derived,
                        },
                        operation: m.reset,
                    },
                    ArchiveRecord::HeadsChanged {
                        sidecar: m.sidecar,
                        expected: vec![version],
                        heads: vec![reset],
                    },
                    ArchiveRecord::ProjectionPlanned { plan: plan.clone() },
                ],
            )
            .unwrap(),
            history: vec![],
            projections: vec![plan],
            attempt: OperationId::new(),
        }
    });
    if !catalog.commit_applied(reset_plan.commit.commit_id).unwrap() {
        let mut guard = store.acquire_publication().unwrap();
        replay_guard(&store, &mut catalog, &mut guard);
        let receipt = store
            .publish_versions(&mut guard, reset_plan, faults)
            .unwrap();
        catalog
            .apply_commit_with_faults(&receipt.commit, faults)
            .unwrap();
        store.release(guard).unwrap();
    }
    let sessions = ApplicationSessions::default();
    let session = sessions.authorize(true, false).unwrap();
    RefreshCoordinator {
        catalog: &mut catalog,
        local: &local,
        state: &state,
        sessions: &sessions,
    }
    .apply(
        RefreshRequest {
            groups: vec![m.group],
            application_session: session.id(),
            operation_id: OperationId::new(),
        },
        faults,
    )
    .unwrap();
    catalog.refresh_protection(m.group).unwrap();
    let intents = EvictionPlanner::candidates_with_hold(&catalog, u64::MAX, 0).unwrap();
    assert!(
        !intents.is_empty(),
        "archived, refreshed originals remained permanently protected"
    );
    for intent in intents {
        Evictor {
            catalog: &mut catalog,
            local: &local,
        }
        .execute(intent, faults)
        .unwrap();
    }
}
pub fn assert_preserved(base: &Path) {
    for (member, bytes) in members()
        .iter()
        .zip([b"raw original".as_slice(), b"jpeg original".as_slice()])
    {
        assert_eq!(
            std::fs::read(base.join("source").join(member.path.as_str())).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read(base.join("archive").join(member.path.as_str())).unwrap(),
            bytes
        );
    }
    assert_eq!(
        std::fs::read(
            base.join("archive")
                .join(history_path(&Digest::of(SAVED)).unwrap().as_str())
        )
        .unwrap(),
        SAVED
    );
    assert!(!base.join("archive/RAW/a.ORF.rrdata").exists());
}
