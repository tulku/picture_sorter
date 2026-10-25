mod support;
use photo_sorter::{catalog::*, domain::*, edit::*, fs::*, transfer::*};
#[test]
fn failed_snapshot_ack_retries_same_version_and_exact_saved_bytes() {
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut catalog = Catalog::open(&root).unwrap();
    let sidecar = SidecarId::new();
    catalog
        .register_sidecar(
            sidecar,
            None,
            ArchivePath::parse("A.ORF.rrdata").unwrap(),
            "opaque",
        )
        .unwrap();
    let intent = SnapshotIntent {
        sidecar,
        parents: vec![],
        operation: OperationId::new(),
        payload: SnapshotPayload::Content(b"saved bytes".to_vec()),
        origin: VersionOrigin::Observed,
    };
    *r.faults.0.lock().unwrap() = Some(FaultPoint::IntentSaved);
    assert!(
        VersionStore::new(&mut catalog, &root)
            .record_with_faults(intent.clone(), &r.faults)
            .is_err()
    );
    drop(catalog);
    let mut catalog = Catalog::open(&root).unwrap();
    let version = VersionStore::new(&mut catalog, &root)
        .record(intent.clone())
        .unwrap();
    assert_eq!(
        VersionStore::new(&mut catalog, &root)
            .record(intent)
            .unwrap(),
        version
    );
    assert_eq!(
        root.read(&snapshot_path(&Digest::of(b"saved bytes")).unwrap(), 100)
            .unwrap(),
        b"saved bytes"
    );
}
#[test]
fn verified_transfer_fault_retains_source_and_owned_stage() {
    let r = support::TestRig::new();
    std::fs::write(r.source.join("A.ORF"), b"raw").unwrap();
    let job = JobId::new();
    let spec = TransferSpec {
        job,
        generation: 1,
        source: SourceLocation {
            root: r.source.clone(),
            path: ArchivePath::parse("A.ORF").unwrap(),
        },
        destination_root: r.local.clone(),
        staged_path: staging_path(job, 1),
        expected_digest: Digest::of(b"raw"),
        expected_size: 3,
    };
    let source = RootHandle::open(&r.source, RootPolicy::Local).unwrap();
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    *r.faults.0.lock().unwrap() = Some(FaultPoint::Verified);
    assert!(TransferEngine::stage_confined_with_faults(&spec, &source, &local, &r.faults).is_err());
    assert_eq!(std::fs::read(r.source.join("A.ORF")).unwrap(), b"raw");
    assert_eq!(local.read(&spec.staged_path, 100).unwrap(), b"raw");
    assert_eq!(
        TransferEngine::stage_confined(&spec, &source, &local)
            .unwrap()
            .digest,
        Digest::of(b"raw")
    );
}
#[path = "support/crash_scenario.rs"]
mod scenario;
struct ProcessCrash(String);
impl FaultInjector for ProcessCrash {
    fn hit(&self, point: FaultPoint) -> photo_sorter::error::AppResult<()> {
        if self.0 == format!("{point:?}") {
            unsafe {
                libc::raise(libc::SIGKILL);
            }
        }
        Ok(())
    }
}
#[test]
fn subprocess_boundary() {
    let Some(base) = std::env::var_os("PHOTO_TEST_CRASH_ROOT").map(std::path::PathBuf::from) else {
        return;
    };
    let fault = ProcessCrash(std::env::var("PHOTO_TEST_CRASH_POINT").unwrap_or_default());
    scenario::run(&base, &fault);
}
#[test]
fn every_filesystem_durable_boundary_restarts_without_loss() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    for point in [
        FaultPoint::IntentSaved,
        FaultPoint::StagePartial,
        FaultPoint::Verified,
        FaultPoint::OriginalPublished,
        FaultPoint::HistoryPublished,
        FaultPoint::CommitPublished,
        FaultPoint::ProjectionPublished,
        FaultPoint::LocalAck,
        FaultPoint::BeforeProjection,
        FaultPoint::BeforeUnlink,
        FaultPoint::UnlinkMember,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let manifest = scenario::setup(dir.path());
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "subprocess_boundary", "--nocapture"])
            .env("PHOTO_TEST_CRASH_ROOT", dir.path())
            .env("PHOTO_TEST_CRASH_POINT", format!("{point:?}"))
            .process_group(0)
            .output()
            .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGKILL),
            "{point:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        recover(dir.path(), &manifest);
        scenario::run(dir.path(), &NoFaults);
        scenario::assert_preserved(dir.path());
    }
}
#[test]
fn returned_errors_at_filesystem_boundaries_resume_without_loss() {
    for point in [
        FaultPoint::IntentSaved,
        FaultPoint::StagePartial,
        FaultPoint::Verified,
        FaultPoint::OriginalPublished,
        FaultPoint::HistoryPublished,
        FaultPoint::CommitPublished,
        FaultPoint::ProjectionPublished,
        FaultPoint::LocalAck,
        FaultPoint::BeforeProjection,
        FaultPoint::BeforeUnlink,
        FaultPoint::UnlinkMember,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let manifest = scenario::setup(dir.path());
        let fault = support::faults::Faults::default();
        *fault.0.lock().unwrap() = Some(point);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scenario::run(
                dir.path(),
                &fault
            )))
            .is_err(),
            "{point:?} did not interrupt"
        );
        assert!(
            fault.0.lock().unwrap().is_none(),
            "{point:?} was not reached"
        );
        recover(dir.path(), &manifest);
        scenario::run(dir.path(), &NoFaults);
        scenario::assert_preserved(dir.path());
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
struct ApiManifest {
    intent: RatingIntentId,
    origin: String,
    asset: uuid::Uuid,
    report: photo_sorter::qualification::CapabilityReport,
}
fn api_client(base: &std::path::Path, m: &ApiManifest) -> photo_sorter::immich::ImmichClient {
    use photo_sorter::{immich::*, qualification::*};
    ImmichClient::new(url::Url::parse(&m.origin).unwrap(), &base.join("key"))
        .unwrap()
        .with_rating_policy(
            RatingWriteMode::BestEffort,
            CapabilityRegistry::new(vec![m.report.clone()]),
        )
}
fn api_setup(base: &std::path::Path, server: &support::http::RatingServer) -> ApiManifest {
    use photo_sorter::{immich::*, qualification::*};
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir(base.join("state")).unwrap();
    std::fs::write(base.join("key"), b"isolated-fixture-key").unwrap();
    std::fs::set_permissions(base.join("key"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let root = RootHandle::open(&base.join("state"), RootPolicy::Local).unwrap();
    let mut catalog = Catalog::open(&root).unwrap();
    let group = catalog.create_group(false).unwrap();
    catalog
        .attach_member(
            group,
            OriginalMember {
                path: ArchivePath::parse("a.JPG").unwrap(),
                digest: Digest::of(b"jpeg"),
                size: 4,
                role: MediaRole::Jpeg,
            },
            PairEvidence::UserSelected {
                reason: "isolated API crash fixture".into(),
            },
        )
        .unwrap();
    let client = ImmichClient::new(server.origin.clone(), &base.join("key")).unwrap();
    catalog
        .bind_asset(&server.origin, &client.asset(server.asset).unwrap(), group)
        .unwrap();
    let intent = catalog
        .prepare_rating_write(
            group,
            server.asset,
            RatingObservation::Stars(Rating::try_from(0).unwrap()),
            Rating::try_from(3).unwrap(),
            OperationId::new(),
            &server.origin,
        )
        .unwrap();
    let report = CapabilityReport {
        schema_version: 1,
        capability: "immich-ratings".into(),
        backend_or_application: "immich-fixture".into(),
        tested_version: "3.3.5".into(),
        mount_identity: None,
        config_fingerprint: rating_fingerprint(&server.origin, "3.3.5"),
        scope: ProbeScope::FixtureAsset {
            origin: server.origin.to_string(),
            asset: server.asset,
        },
        results: vec![ProbeResult {
            name: "ratings-round-trip".into(),
            outcome: ProbeOutcome::Pass,
            detail: "isolated HTTP fixture".into(),
        }],
        limitations: vec!["no server CAS".into()],
    };
    let manifest = ApiManifest {
        intent,
        origin: server.origin.to_string(),
        asset: server.asset,
        report,
    };
    std::fs::write(
        base.join("api.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    manifest
}
#[test]
fn api_subprocess_boundary() {
    let Some(base) = std::env::var_os("PHOTO_TEST_API_ROOT").map(std::path::PathBuf::from) else {
        return;
    };
    let m: ApiManifest =
        serde_json::from_slice(&std::fs::read(base.join("api.json")).unwrap()).unwrap();
    let root = RootHandle::open(&base.join("state"), RootPolicy::Local).unwrap();
    let mut catalog = Catalog::open(&root).unwrap();
    let client = api_client(&base, &m);
    photo_sorter::immich::RatingSynchronizer {
        catalog: &mut catalog,
        client: &client,
    }
    .advance_with_faults(
        m.intent,
        &ProcessCrash(std::env::var("PHOTO_TEST_CRASH_POINT").unwrap()),
    )
    .unwrap();
}
#[test]
fn api_process_crashes_read_back_without_resending() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    for point in [FaultPoint::ApiSent, FaultPoint::ApiReceived] {
        let dir = tempfile::tempdir().unwrap();
        let server = support::http::RatingServer::new();
        let m = api_setup(dir.path(), &server);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "api_subprocess_boundary", "--nocapture"])
            .env("PHOTO_TEST_API_ROOT", dir.path())
            .env("PHOTO_TEST_CRASH_POINT", format!("{point:?}"))
            .process_group(0)
            .output()
            .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGKILL),
            "{point:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(server.state.writes.lock().unwrap().len(), 1);
        let root = RootHandle::open(&dir.path().join("state"), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let client = api_client(dir.path(), &m);
        assert!(
            photo_sorter::immich::RatingSynchronizer {
                catalog: &mut catalog,
                client: &client
            }
            .advance(m.intent)
            .unwrap()
            .confirmed
        );
        assert_eq!(server.state.writes.lock().unwrap().len(), 1);
    }
}
#[test]
fn api_returned_errors_keep_sent_uncertainty_until_readback() {
    for point in [FaultPoint::ApiSent, FaultPoint::ApiReceived] {
        let dir = tempfile::tempdir().unwrap();
        let server = support::http::RatingServer::new();
        let m = api_setup(dir.path(), &server);
        let root = RootHandle::open(&dir.path().join("state"), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let client = api_client(dir.path(), &m);
        let fault = support::faults::Faults::default();
        *fault.0.lock().unwrap() = Some(point);
        assert!(
            photo_sorter::immich::RatingSynchronizer {
                catalog: &mut catalog,
                client: &client
            }
            .advance_with_faults(m.intent, &fault)
            .is_err()
        );
        assert!(fault.0.lock().unwrap().is_none());
        assert!(
            photo_sorter::immich::RatingSynchronizer {
                catalog: &mut catalog,
                client: &client
            }
            .advance(m.intent)
            .unwrap()
            .confirmed
        );
        assert_eq!(server.state.writes.lock().unwrap().len(), 1);
    }
}

fn recover(base: &std::path::Path, m: &scenario::Manifest) {
    let mut store = photo_sorter::archive::ArchiveStore::open(
        RootHandle::open(&base.join("archive"), RootPolicy::TestArchive).unwrap(),
        m.archive,
        m.writer,
    )
    .unwrap();
    if base
        .join("archive/.photo-sorter/locks/publication")
        .exists()
    {
        store.recover_lock(true, true).unwrap();
    }
}
struct DiskFull;
impl FaultInjector for DiskFull {
    fn hit(&self, point: FaultPoint) -> photo_sorter::error::AppResult<()> {
        if point == FaultPoint::StagePartial {
            let mut error = photo_sorter::error::AppError::new(
                photo_sorter::error::ErrorCode::Capacity,
                "isolated ENOSPC injection",
            );
            error.errno = Some(libc::ENOSPC);
            return Err(error);
        }
        Ok(())
    }
}
#[test]
fn enospc_retains_partial_and_source_then_resumes_exactly() {
    let r = support::TestRig::new();
    let bytes = vec![3u8; 128 * 1024];
    std::fs::write(r.source.join("A.ORF"), &bytes).unwrap();
    let job = JobId::new();
    let stage = staging_path(job, 1);
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    local
        .mkdir_all(&ArchivePath::parse(stage.as_str().rsplit_once('/').unwrap().0).unwrap())
        .unwrap();
    local.write_new(&stage, &bytes[..4096]).unwrap();
    let source = RootHandle::open(&r.source, RootPolicy::Local).unwrap();
    let spec = TransferSpec {
        job,
        generation: 1,
        source: SourceLocation {
            root: r.source.clone(),
            path: ArchivePath::parse("A.ORF").unwrap(),
        },
        destination_root: r.local.clone(),
        staged_path: stage.clone(),
        expected_digest: Digest::of(&bytes),
        expected_size: bytes.len() as u64,
    };
    let error =
        TransferEngine::stage_confined_with_faults(&spec, &source, &local, &DiskFull).unwrap_err();
    assert_eq!(error.errno, Some(libc::ENOSPC));
    assert_eq!(local.read(&stage, 4096).unwrap(), bytes[..4096]);
    assert_eq!(std::fs::read(r.source.join("A.ORF")).unwrap(), bytes);
    assert!(!r.local.join("A.ORF").exists());
    let receipt = TransferEngine::stage_confined(&spec, &source, &local).unwrap();
    assert_eq!(receipt.digest, Digest::of(&bytes));
    assert_eq!(local.read(&stage, bytes.len() as u64).unwrap(), bytes);
}
