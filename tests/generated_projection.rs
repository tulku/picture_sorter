mod support;
use photo_sorter::{archive::*, domain::*, fs::*};
fn p(s: &str) -> ArchivePath {
    ArchivePath::parse(s).unwrap()
}
fn setup() -> (support::TestRig, ArchiveStore) {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    let id = ArchiveId::new();
    let writer = WriterId::new();
    ArchiveStore::initialize(
        &root,
        InitArchive {
            archive_id: id,
            writer_id: writer,
        },
    )
    .unwrap();
    let mut store = ArchiveStore::open(root, id, writer).unwrap();
    store
        .change_mode(ModeChangeRequest {
            operation: OperationId::new(),
            expected: store.marker().clone(),
            mode: PublicationMode::LocalTest,
            new_owner: writer,
            all_writers_quiescent: true,
            prior_owner_stopped: true,
            no_unresolved_io: true,
            recover: false,
        })
        .unwrap();
    (r, store)
}
fn body(bytes: &[u8]) -> VersionBody {
    VersionBody::Content {
        digest: Digest::of(bytes),
        size: bytes.len() as u64,
    }
}
fn commit(store: &ArchiveStore, plan: &ProjectionPlan, seq: u64) -> CommittedProjection {
    let group = GroupId::new();
    let c = CommitEnvelope::new(
        store.marker().archive_id,
        store.marker().writer_id,
        seq,
        vec![],
        vec![
            ArchiveRecord::GroupCreated { id: group },
            ArchiveRecord::SidecarCreated {
                id: plan.sidecar,
                group: Some(group),
                path: plan.path.clone(),
                owner: plan.owner.clone(),
            },
            ArchiveRecord::VersionAdded {
                version: Version {
                    id: plan.version,
                    sidecar: plan.sidecar,
                    parents: vec![],
                    body: plan.body.clone(),
                    origin: VersionOrigin::Observed,
                },
                operation: OperationId::new(),
            },
            ArchiveRecord::HeadsChanged {
                sidecar: plan.sidecar,
                expected: vec![],
                heads: vec![plan.version],
            },
            ArchiveRecord::ProjectionPlanned { plan: plan.clone() },
        ],
    )
    .unwrap();
    let mut guard = store.acquire_publication().unwrap();
    loop {
        let b = store.journal_batch(&mut guard, 128).unwrap();
        let end = b.end;
        store.ack_journal(&mut guard, b.token).unwrap();
        if end {
            break;
        }
    }
    store
        .publish_originals(
            &mut guard,
            PublicationPlan {
                commit: c.clone(),
                staged: vec![],
                attempt: OperationId::new(),
            },
            &NoFaults,
        )
        .unwrap();
    store.release(guard).unwrap();
    store.committed_projection(&c, plan).unwrap()
}
fn history(r: &support::TestRig, store: &ArchiveStore, bytes: &[u8]) {
    let staged = p(&format!(".photo-sorter/staging/{}", OperationId::new()));
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    root.write_new(&staged, bytes).unwrap();
    store
        .store_history(&staged, &Digest::of(bytes), bytes.len() as u64)
        .unwrap();
}
#[test]
fn generated_first_claim_rejects_identical_foreign_bytes() {
    let (r, store) = setup();
    let plan = ProjectionPlan {
        sidecar: SidecarId::new(),
        path: p("HDR/output.tif"),
        version: VersionId::new(),
        expected: VersionBody::Reset,
        body: body(b"new"),
        owner: "generated-output".into(),
    };
    history(&r, &store, b"new");
    let proof = commit(&store, &plan, 1);
    std::fs::create_dir_all(r.archive.join("HDR")).unwrap();
    std::fs::write(r.archive.join("HDR/output.tif"), b"new").unwrap();
    assert!(matches!(
        store.apply_projection(&proof).unwrap(),
        ProjectionOutcome::Conflict { .. }
    ));
}
#[test]
fn generated_replacement_preserves_previous_bytes_and_retries() {
    let (r, store) = setup();
    let mut plan = ProjectionPlan {
        sidecar: SidecarId::new(),
        path: p("HDR/output.tif"),
        version: VersionId::new(),
        expected: VersionBody::Reset,
        body: body(b"old"),
        owner: "generated-output".into(),
    };
    history(&r, &store, b"old");
    let proof = commit(&store, &plan, 1);
    assert!(matches!(
        store.apply_projection(&proof).unwrap(),
        ProjectionOutcome::Applied
    ));
    plan.version = VersionId::new();
    plan.expected = body(b"old");
    plan.body = body(b"new");
    history(&r, &store, b"new");
    let proof = commit(&store, &plan, 2);
    assert!(matches!(
        store.apply_projection(&proof).unwrap(),
        ProjectionOutcome::Applied
    ));
    assert_eq!(
        std::fs::read(r.archive.join("HDR/output.tif")).unwrap(),
        b"new"
    );
    assert!(matches!(
        store.apply_projection(&proof).unwrap(),
        ProjectionOutcome::AlreadyCurrent
    ));
}
struct Stop(FaultPoint);
impl FaultInjector for Stop {
    fn hit(&self, p: FaultPoint) -> photo_sorter::error::AppResult<()> {
        if p == self.0 {
            Err(photo_sorter::error::AppError::integrity(
                "injected interruption",
            ))
        } else {
            Ok(())
        }
    }
}
#[test]
fn generated_recovery_handles_every_persisted_transition() {
    for point in [
        FaultPoint::IntentSaved,
        FaultPoint::Verified,
        FaultPoint::OriginalPublished,
        FaultPoint::ProjectionPublished,
    ] {
        let (r, store) = setup();
        let mut plan = ProjectionPlan {
            sidecar: SidecarId::new(),
            path: p("HDR/output.tif"),
            version: VersionId::new(),
            expected: VersionBody::Reset,
            body: body(b"old"),
            owner: "generated-output".into(),
        };
        history(&r, &store, b"old");
        let proof = commit(&store, &plan, 1);
        store.apply_projection(&proof).unwrap();
        plan.version = VersionId::new();
        plan.expected = body(b"old");
        plan.body = body(b"new");
        history(&r, &store, b"new");
        let proof = commit(&store, &plan, 2);
        assert!(
            store
                .apply_projection_with_faults(&proof, &Stop(point))
                .is_err()
        );
        assert!(matches!(
            store.apply_projection(&proof).unwrap(),
            ProjectionOutcome::Applied | ProjectionOutcome::AlreadyCurrent
        ));
        assert_eq!(
            std::fs::read(r.archive.join("HDR/output.tif")).unwrap(),
            b"new"
        );
        assert_eq!(
            std::fs::read(r.archive.join(format!(
                ".photo-sorter/generated/{}/retained-{}",
                plan.sidecar, plan.version
            )))
            .unwrap(),
            b"old"
        );
    }
}
#[test]
fn generated_retained_collision_preserves_unknown_bytes() {
    let (r, store) = setup();
    let mut plan = ProjectionPlan {
        sidecar: SidecarId::new(),
        path: p("output.tif"),
        version: VersionId::new(),
        expected: VersionBody::Reset,
        body: body(b"old"),
        owner: "generated-output".into(),
    };
    history(&r, &store, b"old");
    let proof = commit(&store, &plan, 1);
    store.apply_projection(&proof).unwrap();
    plan.version = VersionId::new();
    plan.expected = body(b"old");
    plan.body = body(b"new");
    history(&r, &store, b"new");
    let proof = commit(&store, &plan, 2);
    let retained = r.archive.join(format!(
        ".photo-sorter/generated/{}/retained-{}",
        plan.sidecar, plan.version
    ));
    std::fs::write(&retained, b"unknown").unwrap();
    assert!(matches!(
        store.apply_projection(&proof).unwrap(),
        ProjectionOutcome::Conflict { .. }
    ));
    assert_eq!(std::fs::read(retained).unwrap(), b"unknown");
    assert_eq!(std::fs::read(r.archive.join("output.tif")).unwrap(), b"old");
}
#[test]
fn generated_first_claim_race_and_lost_reply_do_not_adopt_foreign_bytes() {
    for foreign in [true, false] {
        let (r, store) = setup();
        let plan = ProjectionPlan {
            sidecar: SidecarId::new(),
            path: p("output.tif"),
            version: VersionId::new(),
            expected: VersionBody::Reset,
            body: body(b"new"),
            owner: "generated-output".into(),
        };
        history(&r, &store, b"new");
        let proof = commit(&store, &plan, 1);
        let stop = if foreign {
            FaultPoint::IntentSaved
        } else {
            FaultPoint::ProjectionPublished
        };
        assert!(
            store
                .apply_projection_with_faults(&proof, &Stop(stop))
                .is_err()
        );
        if foreign {
            std::fs::write(r.archive.join("output.tif"), b"new").unwrap();
            assert!(matches!(
                store.apply_projection(&proof).unwrap(),
                ProjectionOutcome::Conflict { .. }
            ));
        } else {
            assert!(matches!(
                store.apply_projection(&proof).unwrap(),
                ProjectionOutcome::AlreadyCurrent
            ));
        }
        assert_eq!(std::fs::read(r.archive.join("output.tif")).unwrap(), b"new");
    }
}
#[test]
fn generated_scratch_probe_runs_the_replacement_protocol_and_fault_recovery() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    let archive = ArchiveId::new();
    let writer = WriterId::new();
    ArchiveStore::initialize(
        &root,
        InitArchive {
            archive_id: archive,
            writer_id: writer,
        },
    )
    .unwrap();
    let config = photo_sorter::config::Config {
        archive_root: r.archive.clone(),
        archive_id: Some(archive),
        writer_id: writer,
        local_test_backend: true,
        ..Default::default()
    };
    let report = photo_sorter::qualification::qualify_generated_outputs(&config).unwrap();
    assert!(
        photo_sorter::qualification::report_qualified(
            &photo_sorter::qualification::QualificationRequest::GeneratedOutputs,
            &report
        ),
        "{report:#?}"
    );
    assert!(!r.archive.join("first.tif").exists());
}
#[test]
fn generated_owner_and_retained_evidence_precede_already_current() {
    let (r, store) = setup();
    let mut plan = ProjectionPlan {
        sidecar: SidecarId::new(),
        path: p("output.tif"),
        version: VersionId::new(),
        expected: VersionBody::Reset,
        body: body(b"old"),
        owner: "generated-output".into(),
    };
    history(&r, &store, b"old");
    let proof = commit(&store, &plan, 1);
    store.apply_projection(&proof).unwrap();
    plan.expected = body(b"old");
    plan.body = body(b"new");
    plan.version = VersionId::new();
    history(&r, &store, b"new");
    let proof = commit(&store, &plan, 2);
    store.apply_projection(&proof).unwrap();
    let retained = r.archive.join(format!(
        ".photo-sorter/generated/{}/retained-{}",
        plan.sidecar, plan.version
    ));
    std::fs::write(&retained, b"unknown").unwrap();
    assert!(matches!(
        store.apply_projection(&proof).unwrap(),
        ProjectionOutcome::Conflict { .. }
    ));
    std::fs::write(
        r.archive.join(format!(
            ".photo-sorter/generated/{}/owner.json",
            plan.sidecar
        )),
        b"{}",
    )
    .unwrap();
    assert!(store.apply_projection(&proof).is_err());
    assert_eq!(std::fs::read(retained).unwrap(), b"unknown");
}
struct ReplaceOnStage(std::path::PathBuf);
impl FaultInjector for ReplaceOnStage {
    fn hit(&self, point: FaultPoint) -> photo_sorter::error::AppResult<()> {
        if point == FaultPoint::Verified {
            std::fs::write(&self.0, b"foreign")?;
        }
        Ok(())
    }
}
#[test]
fn generated_unexpected_displaced_bytes_are_retained_before_publication() {
    let (r, store) = setup();
    let mut plan = ProjectionPlan {
        sidecar: SidecarId::new(),
        path: p("output.tif"),
        version: VersionId::new(),
        expected: VersionBody::Reset,
        body: body(b"old"),
        owner: "generated-output".into(),
    };
    history(&r, &store, b"old");
    let proof = commit(&store, &plan, 1);
    store.apply_projection(&proof).unwrap();
    plan.expected = body(b"old");
    plan.body = body(b"new");
    plan.version = VersionId::new();
    history(&r, &store, b"new");
    let proof = commit(&store, &plan, 2);
    assert!(matches!(
        store
            .apply_projection_with_faults(&proof, &ReplaceOnStage(r.archive.join("output.tif")))
            .unwrap(),
        ProjectionOutcome::Conflict {
            preserved: Some(_),
            ..
        }
    ));
    assert!(!r.archive.join("output.tif").exists());
    assert_eq!(
        std::fs::read(r.archive.join(format!(
            ".photo-sorter/generated/{}/retained-{}",
            plan.sidecar, plan.version
        )))
        .unwrap(),
        b"foreign"
    );
    assert!(matches!(
        store.apply_projection(&proof).unwrap(),
        ProjectionOutcome::Conflict { .. }
    ));
}
#[test]
fn generated_qualification_is_separate_and_single_writer_only() {
    use photo_sorter::qualification::*;
    let fingerprint = Digest::of(b"generated");
    let report = CapabilityReport {
        schema_version: 1,
        capability: "nas-generated-replacement".into(),
        backend_or_application: "linux-filesystem".into(),
        tested_version: GENERATED_IMPLEMENTATION.into(),
        mount_identity: Some("mount".into()),
        config_fingerprint: fingerprint.clone(),
        scope: ProbeScope::SingleClient,
        results: GENERATED_CHECKS
            .iter()
            .map(|name| ProbeResult {
                name: (*name).into(),
                outcome: ProbeOutcome::Pass,
                detail: "fixture".into(),
            })
            .collect(),
        limitations: vec![],
    };
    let registry = CapabilityRegistry::new(vec![report.clone()]);
    assert!(
        registry
            .allows(OperationKind::GeneratedReplacement {
                mount_identity: "mount",
                fingerprint: &fingerprint,
                mode: &PublicationMode::SingleWriter
            })
            .allowed
    );
    for mode in [PublicationMode::MultiWriter, PublicationMode::LocalTest] {
        assert!(
            !registry
                .allows(OperationKind::GeneratedReplacement {
                    mount_identity: "mount",
                    fingerprint: &fingerprint,
                    mode: &mode
                })
                .allowed
        );
    }
    assert!(
        !registry
            .allows(OperationKind::ArchiveExchange {
                mount_identity: "mount",
                fingerprint: &fingerprint,
                mode: &PublicationMode::SingleWriter
            })
            .allowed
    );
    let mut missing = report;
    missing.results.pop();
    assert!(
        !CapabilityRegistry::new(vec![missing])
            .allows(OperationKind::GeneratedReplacement {
                mount_identity: "mount",
                fingerprint: &fingerprint,
                mode: &PublicationMode::SingleWriter
            })
            .allowed
    );
}

#[test]
fn interrupted_generated_marker_staging_never_exposes_partial_intent() {
    for point in [
        FaultPoint::GeneratedOwnerStaged,
        FaultPoint::GeneratedPreparedStaged,
    ] {
        let (r, store) = setup();
        let plan = ProjectionPlan {
            sidecar: SidecarId::new(),
            path: p("render.tif"),
            version: VersionId::new(),
            expected: VersionBody::Reset,
            body: body(b"new"),
            owner: GENERATED_OUTPUT_OWNER.into(),
        };
        history(&r, &store, b"new");
        let proof = commit(&store, &plan, 1);
        assert!(
            store
                .apply_projection_with_faults(&proof, &Stop(point))
                .is_err()
        );
        let directory = r
            .archive
            .join(format!(".photo-sorter/generated/{}", plan.sidecar));
        let marker = if point == FaultPoint::GeneratedOwnerStaged {
            "owner.json".into()
        } else {
            format!("prepared-{}.json", plan.version)
        };
        assert!(!directory.join(marker).exists());
        let staged = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("marker-"))
            .unwrap()
            .path();
        std::fs::write(&staged, b"{").unwrap();
        assert!(matches!(
            store.apply_projection(&proof).unwrap(),
            ProjectionOutcome::Applied
        ));
        assert_eq!(std::fs::read(r.archive.join("render.tif")).unwrap(), b"new");
        assert_eq!(std::fs::read(staged).unwrap(), b"{");
    }
}
