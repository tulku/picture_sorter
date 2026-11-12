mod support;
use photo_sorter::{archive::*, config::Config, domain::*, fs::*, qualification::*};
use std::os::unix::fs::MetadataExt;
fn production_root(r: &support::TestRig) -> RootHandle {
    RootHandle::open(
        &r.archive,
        RootPolicy::Archive {
            expected_device: std::fs::metadata(&r.archive).unwrap().dev(),
        },
    )
    .unwrap()
}
#[test]
fn unqualified_production_write_blocked_before_namespace_creation() {
    let r = support::TestRig::new();
    let root = production_root(&r);
    assert!(
        ArchiveStore::initialize(
            &root,
            InitArchive {
                archive_id: ArchiveId::new(),
                writer_id: WriterId::new()
            }
        )
        .is_err()
    );
    assert!(!r.archive.join(".photo-sorter").exists());
}
#[test]
fn single_client_evidence_never_enables_multi_writer() {
    let r = support::TestRig::new();
    let root = production_root(&r);
    let archive = ArchiveId::new();
    let writer = WriterId::new();
    let fingerprint =
        nas_fingerprint(&root, archive, writer, &PublicationMode::MultiWriter, 2).unwrap();
    let report = CapabilityReport {
        schema_version: 1,
        capability: "nas-publication".into(),
        backend_or_application: "linux-filesystem".into(),
        tested_version: NAS_IMPLEMENTATION.into(),
        mount_identity: Some(root.mount_identity().unwrap()),
        config_fingerprint: fingerprint.clone(),
        scope: ProbeScope::SingleClient,
        results: [
            "exclusive_init",
            "no_replace",
            "rename_exchange",
            "lock_exclusion",
            "flush_reopen",
        ]
        .into_iter()
        .map(|name| ProbeResult {
            name: name.into(),
            outcome: ProbeOutcome::Pass,
            detail: "isolated fixture".into(),
        })
        .collect(),
        limitations: vec!["second client unrun".into()],
    };
    assert!(
        !CapabilityRegistry::new(vec![report])
            .allows(OperationKind::ArchiveWrite {
                mount_identity: &root.mount_identity().unwrap(),
                fingerprint: &fingerprint,
                mode: &PublicationMode::MultiWriter
            })
            .allowed
    );
}
#[test]
fn local_test_evidence_cannot_enable_production_writes() {
    let r = support::TestRig::new();
    let root = production_root(&r);
    let archive = ArchiveId::new();
    let writer = WriterId::new();
    let fingerprint =
        nas_fingerprint(&root, archive, writer, &PublicationMode::SingleWriter, 1).unwrap();
    let report = CapabilityReport {
        schema_version: 1,
        capability: "nas-publication".into(),
        backend_or_application: "local-test".into(),
        tested_version: NAS_IMPLEMENTATION.into(),
        mount_identity: Some(root.mount_identity().unwrap()),
        config_fingerprint: fingerprint.clone(),
        scope: ProbeScope::SingleClient,
        results: [
            "exclusive_init",
            "no_replace",
            "rename_exchange",
            "lock_exclusion",
            "flush_reopen",
        ]
        .into_iter()
        .map(|name| ProbeResult {
            name: name.into(),
            outcome: ProbeOutcome::Pass,
            detail: "local test".into(),
        })
        .collect(),
        limitations: vec![],
    };
    assert!(
        !CapabilityRegistry::new(vec![report])
            .allows(OperationKind::ArchiveWrite {
                mount_identity: &root.mount_identity().unwrap(),
                fingerprint: &fingerprint,
                mode: &PublicationMode::SingleWriter
            })
            .allowed
    );
}
#[test]
fn readonly_doctor_works_without_service_and_never_initializes_roots() {
    let r = support::TestRig::new();
    let config = Config::new(
        r.local.clone(),
        r.state.clone(),
        r._temp.path().join("unavailable-archive"),
    );
    let profile = r._temp.path().join("profile.toml");
    std::fs::write(&profile, toml::to_string(&config).unwrap()).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_photo_sorter"))
        .arg("--config")
        .arg(&profile)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(std::fs::read_dir(&r.state).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&r.local).unwrap().count(), 0);
    assert!(!config.archive_root.exists());
}
#[test]
fn executable_scratch_probe_binds_mount_and_profile_without_touching_archive() {
    let r = support::TestRig::new();
    let scratch = r._temp.path().join("probe");
    std::fs::create_dir(&scratch).unwrap();
    std::fs::write(
        r.archive.join("sentinel"),
        b"personal bytes remain untouched",
    )
    .unwrap();
    let archive = ArchiveId::new();
    let writer = WriterId::new();
    let report = qualify_nas(NasProbeRequest {
        archive_root: r.archive.clone(),
        archive,
        writer,
        mode: PublicationMode::SingleWriter,
        mode_revision: 1,
        scratch_root: scratch,
        session: OperationId::new(),
        phase: NasProbePhase::Prepare,
        local_test_backend: false,
        prior_peer_stopped: false,
        all_writers_quiescent: false,
        disconnect_observed: false,
        interrupt_bytes: 1024 * 1024,
    })
    .unwrap();
    let root = production_root(&r);
    let fingerprint =
        nas_fingerprint(&root, archive, writer, &PublicationMode::SingleWriter, 1).unwrap();
    assert!(
        CapabilityRegistry::new(vec![report.clone()])
            .allows(OperationKind::ArchiveWrite {
                mount_identity: &root.mount_identity().unwrap(),
                fingerprint: &fingerprint,
                mode: &PublicationMode::SingleWriter
            })
            .allowed
    );
    assert!(matches!(report.scope, ProbeScope::SingleClient));
    assert!(
        report
            .results
            .iter()
            .any(|r| r.name == "peer_handoff" && matches!(r.outcome, ProbeOutcome::Unrun))
    );
    assert_eq!(
        std::fs::read(r.archive.join("sentinel")).unwrap(),
        b"personal bytes remain untouched"
    );
    assert!(!r.archive.join(".photo-sorter").exists());
}
#[test]
fn real_cli_probe_enables_only_matching_production_mount_and_writer() {
    use support::service::ServiceFixture;
    let request_path = std::cell::RefCell::new(None);
    let fixture = ServiceFixture::with_config(|config, r| {
        config.local_test_backend = false;
        let scratch = r._temp.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let request = QualificationRequest::Nas(NasProbeRequest {
            archive_root: config.archive_root.clone(),
            archive: config.archive_id.unwrap(),
            writer: config.writer_id,
            mode: PublicationMode::SingleWriter,
            mode_revision: 1,
            scratch_root: scratch,
            session: OperationId::new(),
            phase: NasProbePhase::Prepare,
            local_test_backend: false,
            prior_peer_stopped: false,
            all_writers_quiescent: false,
            disconnect_observed: false,
            interrupt_bytes: 1024 * 1024,
        });
        let path = r._temp.path().join("request.json");
        std::fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
        *request_path.borrow_mut() = Some(path);
    });
    let before = fixture.cli(&["archive", "init"]);
    assert_eq!(before.status.code(), Some(3));
    assert!(!fixture.r.archive.join(".photo-sorter").exists());
    let request_path = request_path.into_inner().unwrap();
    let out = fixture.cli(&["qualify", "--request", request_path.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let data: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(data["outcome"]["detail"]["qualified"], true);
    assert!(fixture.r.state.join("capability-registry.json").exists());
    let out = fixture.cli(&["archive", "init"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        fixture
            .r
            .archive
            .join(".photo-sorter/archive.json")
            .exists()
    );
}
#[test]
fn editor_probe_compares_native_saves_and_detects_lost_unknown_fields() {
    let r = support::TestRig::new();
    let scratch = r._temp.path().join("editor");
    std::fs::create_dir(&scratch).unwrap();
    let mut value = serde_json::json!({"version":1,"rating":0,"adjustments":{"exposure":1},"masks":[{"unknown":"retain"}]});
    for (name, rating) in [
        ("before.json", 0),
        ("three.json", 3),
        ("five.json", 5),
        ("zero.json", 0),
        ("reopen.json", 0),
    ] {
        value["rating"] = rating.into();
        std::fs::write(scratch.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
    }
    std::fs::write(
        scratch.join("reset.json"),
        include_bytes!("fixtures/rapidraw/default.json"),
    )
    .unwrap();
    let request = EditorProbeRequest {
        pick: None,
        isolated_root: scratch.clone(),
        version: "1.6.6".into(),
        before: ArchivePath::parse("before.json").unwrap(),
        three: ArchivePath::parse("three.json").unwrap(),
        five: ArchivePath::parse("five.json").unwrap(),
        zero: ArchivePath::parse("zero.json").unwrap(),
        reopened: ArchivePath::parse("reopen.json").unwrap(),
        regenerated_after_reset: ArchivePath::parse("reset.json").unwrap(),
        runtime_binary: None,
        runtime_saves_attested: false,
    };
    let report = qualify_rapidraw(request.clone()).unwrap();
    assert!(
        report
            .results
            .iter()
            .filter(|r| r.name != "runtime_application")
            .all(|r| matches!(r.outcome, ProbeOutcome::Pass))
    );
    assert!(matches!(report.scope, ProbeScope::Source));
    value.as_object_mut().unwrap().remove("masks");
    std::fs::write(
        scratch.join("five.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    let report = qualify_rapidraw(request).unwrap();
    assert!(
        report
            .results
            .iter()
            .any(|r| matches!(r.outcome, ProbeOutcome::Fail))
    );
}
#[test]
fn isolated_immich_probe_round_trips_and_restores_without_certifying_other_assets() {
    use std::os::unix::fs::PermissionsExt;
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let key = r.state.join("key");
    std::fs::write(&key, "fixture-key").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    *server.state.rating.lock().unwrap() = Some(2);
    let request = ImmichProbeRequest {
        favorites: false,
        origin: server.origin.clone(),
        key_file: key,
        asset: server.asset,
        isolated_asset_confirmed: true,
        fixture: true,
    };
    let report = qualify_immich(request).unwrap();
    assert!(
        report
            .results
            .iter()
            .all(|r| matches!(r.outcome, ProbeOutcome::Pass)),
        "{report:?}"
    );
    assert_eq!(*server.state.rating.lock().unwrap(), Some(2));
    let registry = CapabilityRegistry::new(vec![report]);
    assert!(
        registry
            .allows(OperationKind::RatingWrite {
                origin: &server.origin,
                asset: server.asset,
                version: "3.3.5"
            })
            .allowed
    );
    assert!(
        !registry
            .allows(OperationKind::RatingWrite {
                origin: &server.origin,
                asset: uuid::Uuid::new_v4(),
                version: "3.3.5"
            })
            .allowed
    );
}
#[test]
fn writer_mode_transition_requires_quiescence_and_preserves_journal() {
    let r = support::TestRig::new();
    let archive = ArchiveId::new();
    let writer = WriterId::new();
    ArchiveStore::initialize(
        &RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap(),
        InitArchive {
            archive_id: archive,
            writer_id: writer,
        },
    )
    .unwrap();
    let mut store = ArchiveStore::open(
        RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap(),
        archive,
        writer,
    )
    .unwrap();
    let request = ModeChangeRequest {
        operation: OperationId::new(),
        expected: store.marker().clone(),
        mode: PublicationMode::MultiWriter,
        new_owner: writer,
        all_writers_quiescent: false,
        prior_owner_stopped: true,
        no_unresolved_io: true,
        recover: false,
    };
    assert!(store.change_mode(request.clone()).is_err());
    assert_eq!(store.marker().mode_revision, 1);
    let request = ModeChangeRequest {
        all_writers_quiescent: true,
        ..request
    };
    let marker = store.change_mode(request.clone()).unwrap();
    assert_eq!(marker.mode, PublicationMode::MultiWriter);
    assert_eq!(marker.mode_revision, 2);
    assert!(
        r.archive
            .join(format!(".photo-sorter/modes/2-{}.json", request.operation))
            .exists()
    );
    assert!(store.require_writable().is_ok());
    assert_eq!(store.change_mode(request).unwrap(), marker);
}
#[test]
fn interrupted_mode_change_blocks_writes_until_matching_recovery() {
    for point in [
        FaultPoint::IntentSaved,
        FaultPoint::CommitPublished,
        FaultPoint::ProjectionPublished,
    ] {
        let r = support::TestRig::new();
        let archive = ArchiveId::new();
        let writer = WriterId::new();
        ArchiveStore::initialize(
            &RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap(),
            InitArchive {
                archive_id: archive,
                writer_id: writer,
            },
        )
        .unwrap();
        let mut store = ArchiveStore::open(
            RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap(),
            archive,
            writer,
        )
        .unwrap();
        let mut request = ModeChangeRequest {
            operation: OperationId::new(),
            expected: store.marker().clone(),
            mode: PublicationMode::MultiWriter,
            new_owner: writer,
            all_writers_quiescent: true,
            prior_owner_stopped: true,
            no_unresolved_io: true,
            recover: false,
        };
        *r.faults.0.lock().unwrap() = Some(point);
        assert!(
            store
                .change_mode_with_faults(request.clone(), &r.faults)
                .is_err()
        );
        drop(store);
        let mut store = ArchiveStore::open(
            RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap(),
            archive,
            writer,
        )
        .unwrap();
        assert!(store.require_writable().is_err());
        assert!(store.change_mode(request.clone()).is_err());
        request.recover = true;
        assert_eq!(store.change_mode(request).unwrap().mode_revision, 2);
        assert!(store.require_writable().is_ok());
        let guard = store.acquire_publication().unwrap();
        store.release(guard).unwrap();
    }
}
#[test]
fn changed_mount_version_and_writer_invalidate_evidence() {
    let r = support::TestRig::new();
    let scratch = r._temp.path().join("scope");
    std::fs::create_dir(&scratch).unwrap();
    let archive = ArchiveId::new();
    let writer = WriterId::new();
    let root = production_root(&r);
    let request = NasProbeRequest {
        archive_root: r.archive.clone(),
        archive,
        writer,
        mode: PublicationMode::SingleWriter,
        mode_revision: 1,
        scratch_root: scratch,
        session: OperationId::new(),
        phase: NasProbePhase::Prepare,
        local_test_backend: false,
        prior_peer_stopped: false,
        all_writers_quiescent: false,
        disconnect_observed: false,
        interrupt_bytes: 1024,
    };
    let report = qualify_nas(request).unwrap();
    let fingerprint = report.config_fingerprint.clone();
    let mount = root.mount_identity().unwrap();
    let mut stale = report.clone();
    stale.tested_version = "future-unverified".into();
    assert!(
        !CapabilityRegistry::new(vec![stale])
            .allows(OperationKind::ArchiveWrite {
                mount_identity: &mount,
                fingerprint: &fingerprint,
                mode: &PublicationMode::SingleWriter
            })
            .allowed
    );
    let registry = CapabilityRegistry::new(vec![report]);
    assert!(
        !registry
            .allows(OperationKind::ArchiveWrite {
                mount_identity: "different-mount",
                fingerprint: &fingerprint,
                mode: &PublicationMode::SingleWriter
            })
            .allowed
    );
    let other = nas_fingerprint(
        &root,
        archive,
        WriterId::new(),
        &PublicationMode::SingleWriter,
        1,
    )
    .unwrap();
    assert!(
        !registry
            .allows(OperationKind::ArchiveWrite {
                mount_identity: &mount,
                fingerprint: &other,
                mode: &PublicationMode::SingleWriter
            })
            .allowed
    );
}
#[test]
fn probe_requires_explicit_inputs_and_preserves_unknown_scratch() {
    assert!(
        serde_json::from_value::<QualificationRequest>(
            serde_json::json!({"kind":"nas","request":{}})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<QualificationRequest>(serde_json::json!({"kind":"immich","request":{"origin":"http://127.0.0.1/","key_file":"/tmp/no-default"}})).is_err());
    let r = support::TestRig::new();
    let scratch = r._temp.path().join("probe-owned");
    let session = OperationId::new();
    let session_dir = scratch.join(format!(".photo-sorter-qualification/{session}"));
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(session_dir.join("sentinel"), b"retain").unwrap();
    let result = qualify_nas(NasProbeRequest {
        archive_root: r.archive.clone(),
        archive: ArchiveId::new(),
        writer: WriterId::new(),
        mode: PublicationMode::SingleWriter,
        mode_revision: 1,
        scratch_root: scratch,
        session,
        phase: NasProbePhase::Prepare,
        local_test_backend: true,
        prior_peer_stopped: false,
        all_writers_quiescent: false,
        disconnect_observed: false,
        interrupt_bytes: 1024,
    });
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(session_dir.join("sentinel")).unwrap(),
        b"retain"
    );
    assert!(!r.archive.join(".photo-sorter").exists());
}
#[test]
fn uncertain_isolated_asset_probe_never_blindly_retries_or_restores() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::Ordering;
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let key = r.state.join("probe-key");
    std::fs::write(&key, "fixture").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    *server.state.rating.lock().unwrap() = Some(2);
    server.state.drop_put.store(true, Ordering::SeqCst);
    let report = qualify_immich(ImmichProbeRequest {
        favorites: false,
        origin: server.origin.clone(),
        key_file: key,
        asset: server.asset,
        isolated_asset_confirmed: true,
        fixture: true,
    })
    .unwrap();
    assert!(
        report
            .results
            .iter()
            .any(|r| matches!(r.outcome, ProbeOutcome::Fail))
    );
    assert_eq!(server.state.writes.lock().unwrap().len(), 1);
    assert!(
        !CapabilityRegistry::new(vec![report])
            .allows(OperationKind::RatingWrite {
                origin: &server.origin,
                asset: server.asset,
                version: "3.3.5"
            })
            .allowed
    );
}
#[test]
fn peer_handoff_does_not_certify_the_peers_untested_filesystem_operations() {
    let r = support::TestRig::new();
    let scratch = r._temp.path().join("peer-scratch");
    std::fs::create_dir(&scratch).unwrap();
    let mut request = NasProbeRequest {
        archive_root: r.archive.clone(),
        archive: ArchiveId::new(),
        writer: WriterId::new(),
        mode: PublicationMode::SingleWriter,
        mode_revision: 1,
        scratch_root: scratch,
        session: OperationId::new(),
        phase: NasProbePhase::Prepare,
        local_test_backend: false,
        prior_peer_stopped: false,
        all_writers_quiescent: false,
        disconnect_observed: false,
        interrupt_bytes: 1024,
    };
    qualify_nas(request.clone()).unwrap();
    request.writer = WriterId::new();
    request.phase = NasProbePhase::Peer;
    let report = qualify_nas(request).unwrap();
    let registry = CapabilityRegistry::new(vec![report.clone()]);
    assert!(
        !registry
            .allows(OperationKind::ArchiveWrite {
                mount_identity: report.mount_identity.as_ref().unwrap(),
                fingerprint: &report.config_fingerprint,
                mode: &PublicationMode::SingleWriter
            })
            .allowed
    );
}
#[test]
fn different_machine_ids_on_one_kernel_boot_are_not_two_nas_clients() {
    let r = support::TestRig::new();
    let scratch = r._temp.path().join("shared-boot");
    std::fs::create_dir(&scratch).unwrap();
    let mut request = NasProbeRequest {
        archive_root: r.archive.clone(),
        archive: ArchiveId::new(),
        writer: WriterId::new(),
        mode: PublicationMode::MultiWriter,
        mode_revision: 2,
        scratch_root: scratch.clone(),
        session: OperationId::new(),
        phase: NasProbePhase::Prepare,
        local_test_backend: false,
        prior_peer_stopped: false,
        all_writers_quiescent: false,
        disconnect_observed: false,
        interrupt_bytes: 1024,
    };
    qualify_nas(request.clone()).unwrap();
    let directory = scratch.join(format!(".photo-sorter-qualification/{}", request.session));
    let path = directory.join("session.json");
    let mut owner: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    owner["machine"] = serde_json::to_value(Digest::of(b"another container machine id")).unwrap();
    std::fs::write(&path, serde_json::to_vec(&owner).unwrap()).unwrap();
    request.writer = WriterId::new();
    request.phase = NasProbePhase::Peer;
    let report = qualify_nas(request).unwrap();
    assert!(
        report
            .results
            .iter()
            .any(|r| r.name == "peer_handoff" && matches!(r.outcome, ProbeOutcome::Inconclusive))
    );
    assert!(!directory.join("peer.json").exists());
}
#[test]
fn pick_transform_requires_native_flag_evidence_independent_of_stars() {
    let r = support::TestRig::new();
    let scratch = r._temp.path().join("pick-editor");
    std::fs::create_dir(&scratch).unwrap();
    let base = serde_json::json!({"version":1,"rating":0,"adjustments":{"masks":[1]},"future":9});
    for (name, flag) in [("pick", "pick"), ("reject", "reject")] {
        let mut value = base.clone();
        value["flag"] = flag.into();
        std::fs::write(scratch.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
    }
    for (name, n) in [("base", 0), ("three", 3), ("five", 5)] {
        let mut value = base.clone();
        value["rating"] = n.into();
        std::fs::write(scratch.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
    }
    std::fs::write(
        scratch.join("reset"),
        include_bytes!("fixtures/rapidraw/default.json"),
    )
    .unwrap();
    let binary = scratch.join("editor");
    std::fs::write(&binary, b"fixture executable identity").unwrap();
    let path = |s| ArchivePath::parse(s).unwrap();
    let mut req = EditorProbeRequest {
        isolated_root: scratch.clone(),
        version: "1.6.6".into(),
        before: path("base"),
        three: path("three"),
        five: path("five"),
        zero: path("base"),
        reopened: path("base"),
        regenerated_after_reset: path("reset"),
        runtime_binary: Some(binary),
        runtime_saves_attested: true,
        pick: None,
    };
    let report = qualify_rapidraw(req.clone()).unwrap();
    let decision = |report: CapabilityReport| {
        CapabilityRegistry::new(vec![report.clone()])
            .allows(OperationKind::EditorPickTransform {
                version: "1.6.6",
                fingerprint: &report.config_fingerprint,
            })
            .allowed
    };
    assert!(!decision(report));
    req.pick = Some(PickProbeDocuments {
        picked: path("pick"),
        cleared: path("base"),
        rejected: path("reject"),
    });
    assert!(decision(qualify_rapidraw(req.clone()).unwrap()));
    let mut corrupted = base;
    corrupted["rating"] = 5.into();
    std::fs::write(
        scratch.join("base"),
        serde_json::to_vec(&corrupted).unwrap(),
    )
    .unwrap();
    assert!(!decision(qualify_rapidraw(req).unwrap()));
}

fn report_without_exchange(
    root: &RootHandle,
    archive: ArchiveId,
    writer: WriterId,
    mode: &PublicationMode,
    revision: u64,
) -> CapabilityReport {
    CapabilityReport {
        schema_version: 1,
        capability: "nas-publication".into(),
        backend_or_application: "linux-filesystem".into(),
        tested_version: NAS_IMPLEMENTATION.into(),
        mount_identity: Some(root.mount_identity().unwrap()),
        config_fingerprint: nas_fingerprint(root, archive, writer, mode, revision).unwrap(),
        scope: ProbeScope::SingleClient,
        results: [
            "exclusive_init",
            "no_replace",
            "lock_exclusion",
            "flush_reopen",
        ]
        .into_iter()
        .map(|name| ProbeResult {
            name: name.into(),
            outcome: ProbeOutcome::Pass,
            detail: "fixture evidence".into(),
        })
        .chain([ProbeResult {
            name: "rename_exchange".into(),
            outcome: ProbeOutcome::Fail,
            detail: "exchange unsupported".into(),
        }])
        .collect(),
        limitations: vec!["metadata replacement unavailable".into()],
    }
}

#[test]
fn single_writer_immutable_publication_does_not_require_exchange() {
    let r = support::TestRig::new();
    let root = production_root(&r);
    let archive = ArchiveId::new();
    let writer = WriterId::new();
    let report = report_without_exchange(&root, archive, writer, &PublicationMode::SingleWriter, 1);
    let registry = CapabilityRegistry::new(vec![report.clone()]);
    assert!(
        registry
            .allows(OperationKind::ArchiveWrite {
                mount_identity: &root.mount_identity().unwrap(),
                fingerprint: &report.config_fingerprint,
                mode: &PublicationMode::SingleWriter,
            })
            .allowed
    );
    ArchiveStore::initialize_qualified(
        &root,
        InitArchive {
            archive_id: archive,
            writer_id: writer,
        },
        &registry,
    )
    .unwrap();
    let store = ArchiveStore::open(root, archive, writer)
        .unwrap()
        .with_registry(registry);
    assert!(store.require_writable().is_ok());
    assert_eq!(store.marker().mode, PublicationMode::SingleWriter);
}

#[test]
fn single_writer_still_requires_every_immutable_publication_probe() {
    let r = support::TestRig::new();
    let root = production_root(&r);
    let report = report_without_exchange(
        &root,
        ArchiveId::new(),
        WriterId::new(),
        &PublicationMode::SingleWriter,
        1,
    );
    for required in [
        "exclusive_init",
        "no_replace",
        "lock_exclusion",
        "flush_reopen",
    ] {
        for outcome in [
            ProbeOutcome::Fail,
            ProbeOutcome::Unrun,
            ProbeOutcome::Inconclusive,
        ] {
            let mut incomplete = report.clone();
            incomplete
                .results
                .iter_mut()
                .find(|p| p.name == required)
                .unwrap()
                .outcome = outcome;
            assert!(
                !CapabilityRegistry::new(vec![incomplete])
                    .allows(OperationKind::ArchiveWrite {
                        mount_identity: &root.mount_identity().unwrap(),
                        fingerprint: &report.config_fingerprint,
                        mode: &PublicationMode::SingleWriter,
                    })
                    .allowed,
                "{required} must pass"
            );
        }
        let mut missing = report.clone();
        missing.results.retain(|p| p.name != required);
        assert!(
            !CapabilityRegistry::new(vec![missing])
                .allows(OperationKind::ArchiveWrite {
                    mount_identity: &root.mount_identity().unwrap(),
                    fingerprint: &report.config_fingerprint,
                    mode: &PublicationMode::SingleWriter,
                })
                .allowed
        );
    }
}

#[test]
fn multi_writer_still_requires_exchange_with_two_client_evidence() {
    let r = support::TestRig::new();
    let root = production_root(&r);
    let mut report = report_without_exchange(
        &root,
        ArchiveId::new(),
        WriterId::new(),
        &PublicationMode::MultiWriter,
        2,
    );
    report.scope = ProbeScope::TwoClient;
    for name in ["peer_handoff", "disconnect_recovery"] {
        report.results.push(ProbeResult {
            name: name.into(),
            outcome: ProbeOutcome::Pass,
            detail: "fixture peer evidence".into(),
        });
    }
    assert!(
        !CapabilityRegistry::new(vec![report.clone()])
            .allows(OperationKind::ArchiveWrite {
                mount_identity: &root.mount_identity().unwrap(),
                fingerprint: &report.config_fingerprint,
                mode: &PublicationMode::MultiWriter,
            })
            .allowed
    );
}

#[test]
fn service_reports_imports_allowed_and_metadata_replacement_disabled() {
    let fixture = support::service::ServiceFixture::with_config(|config, r| {
        config.local_test_backend = false;
        let report = report_without_exchange(
            &production_root(r),
            config.archive_id.unwrap(),
            config.writer_id,
            &PublicationMode::SingleWriter,
            1,
        );
        persist_report(config, &report).unwrap();
        std::fs::write(r.source.join("A.ORF"), b"camera original").unwrap();
    });
    let initialized = fixture.cli(&["archive", "init"]);
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stdout)
    );
    let status = fixture.status();
    assert_eq!(status["capabilities"]["nas_publication"]["allowed"], true);
    assert_eq!(
        status["capabilities"]["nas_metadata_replacement"]["allowed"],
        false
    );
    let imported = fixture.cli(&[
        "import",
        fixture.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stdout)
    );
    let audit = fixture.cli(&["verify", "--archive"]);
    assert!(
        audit.status.success(),
        "{}",
        String::from_utf8_lossy(&audit.stdout)
    );
}

fn store_without_exchange(r: &support::TestRig) -> ArchiveStore {
    let root = production_root(r);
    let archive = ArchiveId::new();
    let writer = WriterId::new();
    let registry = CapabilityRegistry::new(vec![report_without_exchange(
        &root,
        archive,
        writer,
        &PublicationMode::SingleWriter,
        1,
    )]);
    ArchiveStore::initialize_qualified(
        &root,
        InitArchive {
            archive_id: archive,
            writer_id: writer,
        },
        &registry,
    )
    .unwrap();
    ArchiveStore::open(root, archive, writer)
        .unwrap()
        .with_registry(registry)
}

fn commit_projection(store: &ArchiveStore, plan: &ProjectionPlan) -> CommittedProjection {
    let version = Version {
        id: plan.version,
        sidecar: plan.sidecar,
        parents: vec![],
        body: plan.body.clone(),
        origin: VersionOrigin::Observed,
    };
    let commit = CommitEnvelope::new(
        store.marker().archive_id,
        store.marker().writer_id,
        1,
        vec![],
        vec![
            ArchiveRecord::SidecarCreated {
                id: plan.sidecar,
                group: None,
                path: plan.path.clone(),
                owner: plan.owner.clone(),
            },
            ArchiveRecord::VersionAdded {
                version,
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
        let batch = store.journal_batch(&mut guard, 128).unwrap();
        let end = batch.end;
        store.ack_journal(&mut guard, batch.token).unwrap();
        if end {
            break;
        }
    }
    store
        .publish_originals(
            &mut guard,
            PublicationPlan {
                commit: commit.clone(),
                staged: vec![],
                attempt: OperationId::new(),
            },
            &NoFaults,
        )
        .unwrap();
    store.release(guard).unwrap();
    store.committed_projection(&commit, plan).unwrap()
}

#[test]
fn unsupported_exchange_preserves_existing_metadata_and_allows_safe_creates() {
    for existing in [None, Some(b"old".as_slice()), Some(b"incoming".as_slice())] {
        let r = support::TestRig::new();
        let store = store_without_exchange(&r);
        let root = production_root(&r);
        let live = ArchivePath::parse("RAW/a.ORF.rrdata").unwrap();
        root.mkdir_all(&ArchivePath::parse("RAW").unwrap()).unwrap();
        if let Some(bytes) = existing {
            root.write_new(&live, bytes).unwrap();
        }
        let digest = Digest::of(b"incoming");
        let staged = ArchivePath::parse(".photo-sorter/staging/history").unwrap();
        root.write_new(&staged, b"incoming").unwrap();
        store.store_history(&staged, &digest, 8).unwrap();
        let plan = ProjectionPlan {
            sidecar: SidecarId::new(),
            path: live.clone(),
            version: VersionId::new(),
            expected: existing
                .map(|bytes| VersionBody::Content {
                    digest: Digest::of(bytes),
                    size: bytes.len() as u64,
                })
                .unwrap_or(VersionBody::Reset),
            body: VersionBody::Content {
                digest: digest.clone(),
                size: 8,
            },
            owner: "rapidraw-base".into(),
        };
        let proof = commit_projection(&store, &plan);
        let staging_before = std::fs::read_dir(r.archive.join(".photo-sorter/staging"))
            .unwrap()
            .count();
        let result = store.apply_projection(&proof);
        if existing == Some(b"old".as_slice()) {
            assert_eq!(
                result.unwrap_err().code,
                photo_sorter::error::ErrorCode::Unsupported
            );
            assert_eq!(root.read(&live, 32).unwrap(), b"old");
            assert_eq!(
                std::fs::read_dir(r.archive.join(".photo-sorter/staging"))
                    .unwrap()
                    .count(),
                staging_before
            );
        } else {
            assert!(matches!(
                result.unwrap(),
                ProjectionOutcome::Applied | ProjectionOutcome::AlreadyCurrent
            ));
            assert_eq!(root.read(&live, 32).unwrap(), b"incoming");
        }
        assert_eq!(
            root.read(&history_path(&digest).unwrap(), 32).unwrap(),
            b"incoming"
        );
        assert!(!store.exchange_decision().allowed);
    }
}

#[test]
fn unsupported_exchange_blocks_mode_change_before_administration_intent() {
    let r = support::TestRig::new();
    let root = production_root(&r);
    let mut store = store_without_exchange(&r);
    let before = store.marker().clone();
    store = store.with_registry(CapabilityRegistry::new(vec![
        report_without_exchange(
            &root,
            before.archive_id,
            before.writer_id,
            &PublicationMode::SingleWriter,
            1,
        ),
        report_without_exchange(
            &root,
            before.archive_id,
            before.writer_id,
            &PublicationMode::SingleWriter,
            2,
        ),
    ]));
    let request = ModeChangeRequest {
        operation: OperationId::new(),
        expected: before.clone(),
        mode: PublicationMode::SingleWriter,
        new_owner: before.writer_id,
        all_writers_quiescent: true,
        prior_owner_stopped: true,
        no_unresolved_io: true,
        recover: false,
    };
    assert_eq!(
        store.change_mode(request).unwrap_err().code,
        photo_sorter::error::ErrorCode::Unsupported
    );
    assert_eq!(store.marker(), &before);
    let marker: ArchiveMarker = serde_json::from_slice(
        &root
            .read(
                &ArchivePath::parse(".photo-sorter/archive.json").unwrap(),
                65536,
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(marker, before);
    for path in [
        ".photo-sorter/mode-pending.json",
        ".photo-sorter/modes",
        ".photo-sorter/locks/publication",
    ] {
        assert!(
            !r.archive.join(path).exists(),
            "unsupported exchange must not create {path}"
        );
    }
}

#[test]
fn unsupported_exchange_preserves_existing_metadata_on_reset() {
    let r = support::TestRig::new();
    let store = store_without_exchange(&r);
    let root = production_root(&r);
    let live = ArchivePath::parse("RAW/a.ORF.rrdata").unwrap();
    root.mkdir_all(&ArchivePath::parse("RAW").unwrap()).unwrap();
    root.write_new(&live, b"saved edits").unwrap();
    let plan = ProjectionPlan {
        sidecar: SidecarId::new(),
        path: live.clone(),
        version: VersionId::new(),
        expected: VersionBody::Content {
            digest: Digest::of(b"saved edits"),
            size: 11,
        },
        body: VersionBody::Reset,
        owner: "rapidraw-base".into(),
    };
    let proof = commit_projection(&store, &plan);
    let staging_before = std::fs::read_dir(r.archive.join(".photo-sorter/staging"))
        .unwrap()
        .count();
    assert_eq!(
        store.apply_projection(&proof).unwrap_err().code,
        photo_sorter::error::ErrorCode::Unsupported
    );
    assert_eq!(root.read(&live, 32).unwrap(), b"saved edits");
    assert_eq!(
        std::fs::read_dir(r.archive.join(".photo-sorter/staging"))
            .unwrap()
            .count(),
        staging_before
    );
}

#[test]
fn pending_unsupported_metadata_update_does_not_block_another_import() {
    use std::time::{Duration, Instant};
    let fixture = support::service::ServiceFixture::with_config(|config, r| {
        config.local_test_backend = false;
        config.debounce_seconds = 0;
        config.reconcile_seconds = 1;
        config.poll_seconds = 1;
        let report = report_without_exchange(
            &production_root(r),
            config.archive_id.unwrap(),
            config.writer_id,
            &PublicationMode::SingleWriter,
            1,
        );
        persist_report(config, &report).unwrap();
        std::fs::write(r.source.join("A.ORF"), b"original A").unwrap();
    });
    assert!(fixture.cli(&["archive", "init"]).status.success());
    let first_import = fixture.cli(&[
        "import",
        fixture.r.source.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        first_import.status.success(),
        "{}",
        String::from_utf8_lossy(&first_import.stdout)
    );
    let status = fixture.status();
    let path = status["groups"][0]["group"]["members"][0]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    let sidecar = format!("{path}.rrdata");
    let before = br#"{"version":1,"rating":3,"adjustments":{"exposure":1},"tags":null}"#;
    let incoming = br#"{"version":1,"rating":3,"adjustments":{"exposure":2},"tags":null}"#;
    std::fs::write(fixture.r.local.join(&sidecar), before).unwrap();
    let deadline = Instant::now();
    while std::fs::read(fixture.r.archive.join(&sidecar))
        .ok()
        .as_deref()
        != Some(before.as_slice())
    {
        assert!(
            deadline.elapsed() < Duration::from_secs(10),
            "initial metadata was not published: {}",
            fixture.status()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    std::fs::write(fixture.r.local.join(&sidecar), incoming).unwrap();
    let history = history_path(&Digest::of(incoming)).unwrap();
    let deadline = Instant::now();
    loop {
        let status = fixture.status();
        let deferred = status["worker"]["publication_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("exchange"));
        if deferred
            && fixture.r.archive.join(history.as_str()).exists()
            && !fixture
                .r
                .archive
                .join(".photo-sorter/locks/publication")
                .exists()
        {
            break;
        }
        assert!(
            deadline.elapsed() < Duration::from_secs(10),
            "unsupported replacement did not release its session: {status}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let second_card = fixture.r._temp.path().join("second-card");
    std::fs::create_dir(&second_card).unwrap();
    std::fs::write(second_card.join("B.ORF"), b"independent original").unwrap();
    let imported = fixture.cli(&[
        "import",
        second_card.to_str().unwrap(),
        "--wait-for",
        "archived",
    ]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stdout)
    );
    assert_eq!(
        std::fs::read(fixture.r.archive.join(&sidecar)).unwrap(),
        before
    );
    assert_eq!(
        std::fs::read(fixture.r.archive.join(history.as_str())).unwrap(),
        incoming
    );
    let status = fixture.status();
    let member = status["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| g["group"]["members"].as_array().unwrap())
        .find(|m| m["path"].as_str().unwrap().ends_with("B.ORF"))
        .unwrap();
    assert_eq!(
        std::fs::read(fixture.r.archive.join(member["path"].as_str().unwrap())).unwrap(),
        b"independent original"
    );
}
