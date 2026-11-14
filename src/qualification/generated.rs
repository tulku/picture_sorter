use super::*;
use crate::{archive::PublicationMode, error::AppResult, fs::RootHandle};
pub const GENERATED_IMPLEMENTATION: &str =
    concat!("photo-generated-replacement-v2/", env!("CARGO_PKG_VERSION"));
pub const GENERATED_CHECKS: [&str; 10] = [
    "first_claim",
    "replacement",
    "recover_staged",
    "recover_partial",
    "recover_owner_marker",
    "recover_prepared_marker",
    "recover_retained",
    "recover_published",
    "foreign_target",
    "retained_collision",
];
pub fn generated_fingerprint(
    root: &RootHandle,
    archive: ArchiveId,
    writer: WriterId,
    mode: &PublicationMode,
    revision: u64,
) -> AppResult<Digest> {
    Ok(Digest::of(&serde_json::to_vec(&(
        GENERATED_IMPLEMENTATION,
        nas_fingerprint(root, archive, writer, mode, revision)?,
    ))?))
}
impl CapabilityRegistry {
    pub(super) fn generated_decision(
        &self,
        mount: &str,
        fingerprint: &Digest,
        mode: &PublicationMode,
    ) -> CapabilityDecision {
        let allowed = *mode == PublicationMode::SingleWriter
            && self.reports.iter().any(|r| {
                r.schema_version == 1
                    && r.capability == "nas-generated-replacement"
                    && r.backend_or_application == "linux-filesystem"
                    && r.tested_version == GENERATED_IMPLEMENTATION
                    && r.mount_identity.as_deref() == Some(mount)
                    && &r.config_fingerprint == fingerprint
                    && matches!(r.scope, ProbeScope::SingleClient)
                    && GENERATED_CHECKS.iter().all(|n| {
                        r.results
                            .iter()
                            .any(|p| p.name == *n && matches!(p.outcome, ProbeOutcome::Pass))
                    })
            });
        CapabilityDecision {
            allowed,
            reason: if allowed {
                "matching generated replacement runtime evidence"
            } else {
                "generated replacement requires qualified SingleWriter runtime evidence"
            }
            .into(),
        }
    }
}
use crate::{
    archive::*,
    config::Config,
    error::{AppError, ErrorCode},
    fs::*,
};
use std::os::unix::fs::MetadataExt;
fn p(s: &str) -> AppResult<ArchivePath> {
    ArchivePath::parse(s)
}
fn body(bytes: &[u8]) -> VersionBody {
    VersionBody::Content {
        digest: Digest::of(bytes),
        size: bytes.len() as u64,
    }
}
struct Stop(FaultPoint);
impl FaultInjector for Stop {
    fn hit(&self, point: FaultPoint) -> AppResult<()> {
        if point == self.0 {
            Err(AppError::integrity("scratch fault injection"))
        } else {
            Ok(())
        }
    }
}
fn checked(condition: bool, message: &str) -> AppResult<()> {
    if condition {
        Ok(())
    } else {
        Err(AppError::integrity(message))
    }
}
fn scratch_plan(
    store: &ArchiveStore,
    name: &str,
    bytes: &[u8],
    expected: VersionBody,
    sidecar: SidecarId,
    sequence: u64,
) -> AppResult<ProjectionPlan> {
    let root = RootHandle::open(
        store.root_path(),
        RootPolicy::Archive {
            expected_device: std::fs::metadata(store.root_path())?.dev(),
        },
    )?;
    let staged = p(&format!(
        ".photo-sorter/staging/input-{}",
        OperationId::new()
    ))?;
    root.write_new(&staged, bytes)?;
    store.store_history(&staged, &Digest::of(bytes), bytes.len() as u64)?;
    let plan = ProjectionPlan {
        sidecar,
        path: p(name)?,
        version: VersionId::new(),
        expected,
        body: body(bytes),
        owner: GENERATED_OUTPUT_OWNER.into(),
    };
    let group = GroupId::new();
    let commit = CommitEnvelope::new(
        store.marker().archive_id,
        store.marker().writer_id,
        sequence,
        vec![],
        vec![
            ArchiveRecord::GroupCreated { id: group },
            ArchiveRecord::SidecarCreated {
                id: sidecar,
                group: Some(group),
                path: plan.path.clone(),
                owner: plan.owner.clone(),
            },
            ArchiveRecord::VersionAdded {
                version: Version {
                    id: plan.version,
                    sidecar,
                    parents: vec![],
                    body: plan.body.clone(),
                    origin: VersionOrigin::Observed,
                },
                operation: OperationId::new(),
            },
            ArchiveRecord::HeadsChanged {
                sidecar,
                expected: vec![],
                heads: vec![plan.version],
            },
            ArchiveRecord::ProjectionPlanned { plan: plan.clone() },
        ],
    )?;
    let mut guard = store.acquire_publication()?;
    loop {
        let batch = store.journal_batch(&mut guard, 128)?;
        let end = batch.end;
        store.ack_journal(&mut guard, batch.token)?;
        if end {
            break;
        }
    }
    store.publish_originals(
        &mut guard,
        PublicationPlan {
            commit: commit.clone(),
            staged: vec![],
            attempt: OperationId::new(),
        },
        &NoFaults,
    )?;
    store.committed_projection(&commit, &plan)?;
    store.release(guard)?;
    Ok(plan)
}
pub fn qualify_generated_outputs(config: &Config) -> AppResult<CapabilityReport> {
    let archive = config
        .archive_id
        .ok_or_else(|| AppError::invalid("generated probe requires archive identity"))?;
    let policy = if config.local_test_backend {
        RootPolicy::TestArchive
    } else {
        RootPolicy::Archive {
            expected_device: std::fs::metadata(&config.archive_root)?.dev(),
        }
    };
    let target = RootHandle::open(&config.archive_root, policy)?;
    let target_store = ArchiveStore::open(target.try_clone()?, archive, config.writer_id)?;
    if target_store.marker().mode != PublicationMode::SingleWriter {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "generated replacement probe requires SingleWriter archive",
        ));
    }
    let marker = target_store.marker();
    let fingerprint = generated_fingerprint(
        &target,
        archive,
        config.writer_id,
        &marker.mode,
        marker.mode_revision,
    )?;
    let scratch_name = format!(
        ".photo-sorter/qualification/generated-{}",
        OperationId::new()
    );
    target.mkdir_all(&p(".photo-sorter/qualification")?)?;
    target.mkdir_exclusive(&p(&scratch_name)?)?;
    let scratch_path = config.archive_root.join(&scratch_name);
    let scratch = RootHandle::open(
        &scratch_path,
        RootPolicy::Archive {
            expected_device: target.device(),
        },
    )?;
    checked(
        scratch.mount_number() == target.mount_number(),
        "scratch changed mount",
    )?;
    scratch.write_new(&p("probe-intent.json")?,&serde_json::to_vec(&serde_json::json!({"archive":archive,"writer":config.writer_id,"fingerprint":fingerprint}))?)?;
    let scratch_id = ArchiveId::new();
    let base_results = super::nas::basic(&scratch);
    checked(
        base_results
            .iter()
            .filter(|r| r.name != "rename_exchange")
            .all(|r| matches!(r.outcome, ProbeOutcome::Pass)),
        "scratch basic filesystem checks failed",
    )?;
    let base = CapabilityReport {
        schema_version: 1,
        capability: "nas-publication".into(),
        backend_or_application: "linux-filesystem".into(),
        tested_version: NAS_IMPLEMENTATION.into(),
        mount_identity: Some(scratch.mount_identity()?),
        config_fingerprint: nas_fingerprint(
            &scratch,
            scratch_id,
            config.writer_id,
            &PublicationMode::SingleWriter,
            1,
        )?,
        scope: ProbeScope::SingleClient,
        results: base_results,
        limitations: vec![],
    };
    let registry = CapabilityRegistry::new(vec![base]);
    ArchiveStore::initialize_qualified(
        &scratch,
        InitArchive {
            archive_id: scratch_id,
            writer_id: config.writer_id,
        },
        &registry,
    )?;
    let store = ArchiveStore::open(scratch.try_clone()?, scratch_id, config.writer_id)?
        .with_registry(registry);
    let mut sequence = 0;
    let mut run =
        |name: &str,
         action: &mut dyn FnMut(&ArchiveStore, &RootHandle, &mut u64) -> AppResult<()>| {
            let result = action(&store, &scratch, &mut sequence);
            ProbeResult {
                name: name.into(),
                outcome: if result.is_ok() {
                    ProbeOutcome::Pass
                } else {
                    ProbeOutcome::Fail
                },
                detail: match result {
                    Ok(()) => "fresh scratch protocol bytes verified after reopen".into(),
                    Err(e) => e.to_string(),
                },
            }
        };
    let mut results = vec![];
    results.push(run("first_claim", &mut |s, r, n| {
        *n += 1;
        let plan = scratch_plan(
            s,
            "first.tif",
            b"new",
            VersionBody::Reset,
            SidecarId::new(),
            *n,
        )?;
        r.write_new(&plan.path, b"new")?;
        checked(
            matches!(
                s.apply_generated_protocol(&plan, &NoFaults)?,
                ProjectionOutcome::Conflict { .. }
            ),
            "identical foreign first target adopted",
        )?;
        checked(r.read(&plan.path, 16)? == b"new", "foreign bytes changed")
    }));
    for (name, point) in [
        ("recover_owner_marker", FaultPoint::GeneratedOwnerStaged),
        (
            "recover_prepared_marker",
            FaultPoint::GeneratedPreparedStaged,
        ),
    ] {
        results.push(run(name, &mut |s, r, n| {
            *n += 1;
            let plan = scratch_plan(
                s,
                &format!("{name}.tif"),
                b"new",
                VersionBody::Reset,
                SidecarId::new(),
                *n,
            )?;
            checked(
                s.apply_generated_protocol(&plan, &Stop(point)).is_err(),
                "marker staging interruption did not occur",
            )?;
            let marker = if point == FaultPoint::GeneratedOwnerStaged {
                "owner.json".into()
            } else {
                format!("prepared-{}.json", plan.version)
            };
            checked(
                r.read(
                    &p(&format!(
                        ".photo-sorter/generated/{}/{marker}",
                        plan.sidecar
                    ))?,
                    65536,
                )
                .is_err_and(|e| e.errno == Some(libc::ENOENT)),
                "unfinished marker became visible",
            )?;
            checked(
                matches!(
                    s.apply_generated_protocol(&plan, &NoFaults)?,
                    ProjectionOutcome::Applied
                ),
                "marker staging recovery failed",
            )?;
            checked(
                r.read(&plan.path, 16)? == b"new",
                "marker staging recovery bytes differ",
            )
        }));
    }
    results.push(run("recover_partial", &mut |s, r, n| {
        *n += 1;
        let plan = scratch_plan(
            s,
            "stage-recovery.tif",
            b"new",
            VersionBody::Reset,
            SidecarId::new(),
            *n,
        )?;
        checked(
            s.apply_generated_protocol(&plan, &Stop(FaultPoint::IntentSaved))
                .is_err(),
            "partial intent interruption failed",
        )?;
        let staged = p(&format!(
            ".photo-sorter/generated/{}/staged-{}",
            plan.sidecar, plan.version
        ))?;
        r.write_new(&staged, b"partial")?;
        checked(
            matches!(
                s.apply_generated_protocol(&plan, &NoFaults)?,
                ProjectionOutcome::Applied
            ),
            "partial stage recovery failed",
        )?;
        checked(
            r.read(&plan.path, 16)? == b"new",
            "partial stage recovery bytes differ",
        )
    }));
    for (name, fault) in [
        ("replacement", None),
        ("recover_staged", Some(FaultPoint::Verified)),
        ("recover_retained", Some(FaultPoint::OriginalPublished)),
        ("recover_published", Some(FaultPoint::ProjectionPublished)),
    ] {
        results.push(run(name, &mut |s, r, n| {
            let sidecar = SidecarId::new();
            let filename = format!("{name}.tif");
            *n += 1;
            let old = scratch_plan(s, &filename, b"old", VersionBody::Reset, sidecar, *n)?;
            checked(
                matches!(
                    s.apply_generated_protocol(&old, &NoFaults)?,
                    ProjectionOutcome::Applied
                ),
                "first publish failed",
            )?;
            *n += 1;
            let new = scratch_plan(s, &filename, b"new", body(b"old"), sidecar, *n)?;
            if let Some(point) = fault {
                checked(
                    s.apply_generated_protocol(&new, &Stop(point)).is_err(),
                    "scratch interruption did not occur",
                )?;
            }
            checked(
                matches!(
                    s.apply_generated_protocol(&new, &NoFaults)?,
                    ProjectionOutcome::Applied | ProjectionOutcome::AlreadyCurrent
                ),
                "replacement recovery failed",
            )?;
            let reopened = RootHandle::open(
                r.path(),
                RootPolicy::Archive {
                    expected_device: r.device(),
                },
            )?;
            checked(
                reopened.read(&new.path, 16)? == b"new",
                "new visible bytes differ",
            )?;
            checked(
                reopened.read(
                    &p(&format!(
                        ".photo-sorter/generated/{}/retained-{}",
                        sidecar, new.version
                    ))?,
                    16,
                )? == b"old",
                "retained bytes differ",
            )
        }));
    }
    for (name, retained) in [("foreign_target", false), ("retained_collision", true)] {
        results.push(run(name, &mut |s, r, n| {
            let sidecar = SidecarId::new();
            let filename = format!("{name}.tif");
            *n += 1;
            let old = scratch_plan(s, &filename, b"old", VersionBody::Reset, sidecar, *n)?;
            s.apply_generated_protocol(&old, &NoFaults)?;
            *n += 1;
            let new = scratch_plan(s, &filename, b"new", body(b"old"), sidecar, *n)?;
            let foreign = if retained {
                p(&format!(
                    ".photo-sorter/generated/{}/retained-{}",
                    sidecar, new.version
                ))?
            } else {
                r.unlink(&new.path, false)?;
                new.path.clone()
            };
            r.write_new(&foreign, b"unknown")?;
            checked(
                matches!(
                    s.apply_generated_protocol(&new, &NoFaults)?,
                    ProjectionOutcome::Conflict { .. }
                ),
                "foreign bytes were accepted",
            )?;
            checked(r.read(&foreign, 16)? == b"unknown", "foreign bytes changed")
        }));
    }
    let report=CapabilityReport{schema_version:1,capability:"nas-generated-replacement".into(),backend_or_application:if config.local_test_backend{"local-test"}else{"linux-filesystem"}.into(),tested_version:GENERATED_IMPLEMENTATION.into(),mount_identity:Some(target.mount_identity()?),config_fingerprint:fingerprint,scope:ProbeScope::SingleClient,results,limitations:vec![format!("Scratch retained at {}",scratch_path.display()),"SingleWriter only; interrupted protocol steps were resumed and bytes reopened; storage power loss was not simulated.".into()]};
    scratch.write_new(&p("report.json")?, &serde_json::to_vec(&report)?)?;
    Ok(report)
}
