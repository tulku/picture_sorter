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
fn completed_projection_retries_after_retained_cleanup() {
    let (r, store) = setup();
    let mut plan = ProjectionPlan {
        sidecar: SidecarId::new(),
        path: p("output.tif"),
        version: VersionId::new(),
        expected: VersionBody::Reset,
        body: body(b"old"),
        owner: GENERATED_OUTPUT_OWNER.into(),
    };
    history(&r, &store, b"old");
    let proof = commit(&store, &plan, 1);
    store.apply_projection(&proof).unwrap();
    plan.version = VersionId::new();
    plan.expected = body(b"old");
    plan.body = body(b"new");
    history(&r, &store, b"new");
    let proof = commit(&store, &plan, 2);
    store.apply_projection(&proof).unwrap();
    let retained = r.archive.join(format!(
        ".photo-sorter/generated/{}/retained-{}",
        plan.sidecar, plan.version
    ));
    std::fs::remove_file(&retained).unwrap();
    assert!(matches!(
        store.apply_projection(&proof).unwrap(),
        ProjectionOutcome::AlreadyCurrent
    ));
    std::fs::write(&retained, b"foreign").unwrap();
    assert!(matches!(
        store.apply_projection(&proof).unwrap(),
        ProjectionOutcome::Conflict { .. }
    ));
}
struct RetentionRig {
    r: support::TestRig,
    store: ArchiveStore,
    plans: Vec<ProjectionPlan>,
    commits: Vec<CommitEnvelope>,
}
impl RetentionRig {
    fn new() -> Self {
        let (r, store) = setup();
        Self {
            r,
            store,
            plans: vec![],
            commits: vec![],
        }
    }
    fn catalog(&self) -> photo_sorter::catalog::Catalog {
        photo_sorter::catalog::Catalog::open(
            &RootHandle::open(&self.r.state, RootPolicy::Local).unwrap(),
        )
        .unwrap()
    }
    fn save(&mut self, bytes: &[u8]) {
        history(&self.r, &self.store, bytes);
        let previous = self.plans.last();
        let sidecar = previous.map(|p| p.sidecar).unwrap_or_default();
        let plan = ProjectionPlan {
            sidecar,
            path: p("saved.tif"),
            version: VersionId::new(),
            expected: previous
                .map(|p| p.body.clone())
                .unwrap_or(VersionBody::Reset),
            body: body(bytes),
            owner: GENERATED_OUTPUT_OWNER.into(),
        };
        let mut records = vec![];
        if previous.is_none() {
            let group = GroupId::new();
            records.push(ArchiveRecord::GroupCreated { id: group });
            records.push(ArchiveRecord::SidecarCreated {
                id: sidecar,
                group: Some(group),
                path: plan.path.clone(),
                owner: plan.owner.clone(),
            });
        }
        records.push(ArchiveRecord::VersionAdded {
            version: Version {
                id: plan.version,
                sidecar,
                parents: previous.map(|p| p.version).into_iter().collect(),
                body: plan.body.clone(),
                origin: VersionOrigin::Observed,
            },
            operation: OperationId::new(),
        });
        records.push(ArchiveRecord::HeadsChanged {
            sidecar,
            expected: previous.map(|p| p.version).into_iter().collect(),
            heads: vec![plan.version],
        });
        records.push(ArchiveRecord::ProjectionPlanned { plan: plan.clone() });
        let sequence = self
            .commits
            .last()
            .map(|c| c.writer_sequence + 1)
            .unwrap_or(1);
        let commit = CommitEnvelope::new(
            self.store.marker().archive_id,
            self.store.marker().writer_id,
            sequence,
            vec![],
            records,
        )
        .unwrap();
        let mut guard = self.guard();
        self.store
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
        self.store.release(guard).unwrap();
        let proof = self.store.committed_projection(&commit, &plan).unwrap();
        self.store.apply_projection(&proof).unwrap();
        let mut catalog = self.catalog();
        catalog.apply_commit(&commit).unwrap();
        drop(catalog);
        let db = rusqlite::Connection::open(self.r.state.join("catalog.sqlite")).unwrap();
        db.execute("UPDATE nas_projections SET applied=1", [])
            .unwrap();
        drop(db);
        self.plans.push(plan);
        self.commits.push(commit);
    }
    fn guard(&self) -> PublicationGuard {
        let mut guard = self.store.acquire_publication().unwrap();
        loop {
            let batch = self.store.journal_batch(&mut guard, 128).unwrap();
            let end = batch.end;
            self.store.ack_journal(&mut guard, batch.token).unwrap();
            if end {
                break;
            }
        }
        guard
    }
    fn plan(&self) -> photo_sorter::catalog::GeneratedRetentionPlan {
        let current = self.plans.last().unwrap();
        self.catalog()
            .prepare_generated_retention(
                self.store.marker().archive_id,
                self.store.marker().writer_id,
                current.sidecar,
                current.version,
            )
            .unwrap()
    }
    fn retire(&mut self) {
        let plan = self.plan();
        let mut guard = self.guard();
        let receipt = self
            .store
            .retire_generated(&mut guard, plan, &NoFaults)
            .unwrap();
        self.store.release(guard).unwrap();
        let mut catalog = self.catalog();
        catalog.apply_commit(&receipt.commit).unwrap();
        catalog.acknowledge_generated_retention(&receipt).unwrap();
        self.commits.push(receipt.commit);
    }
    fn read_commit(&self, index: usize) -> photo_sorter::error::AppResult<CommitEnvelope> {
        let c = &self.commits[index];
        self.store.read_commit_readonly(&p(&format!(
            ".photo-sorter/commits/{}/{}-{}.json",
            c.writer_id, c.writer_sequence, c.commit_id
        )))
    }
}
struct Stop(FaultPoint);
impl FaultInjector for Stop {
    fn hit(&self, point: FaultPoint) -> photo_sorter::error::AppResult<()> {
        if point == self.0 {
            Err(photo_sorter::error::AppError::integrity(
                "injected interruption",
            ))
        } else {
            Ok(())
        }
    }
}
#[test]
fn cleanup_replays_old_history_and_preserves_current_across_successors() {
    let mut rig = RetentionRig::new();
    rig.save(b"A");
    rig.save(b"B");
    rig.retire();
    assert!(
        !rig.r
            .archive
            .join(history_path(&Digest::of(b"A")).unwrap().as_str())
            .exists()
    );
    assert!(rig.read_commit(0).is_ok());
    rig.save(b"C");
    rig.retire();
    assert!(rig.read_commit(0).is_ok());
    assert!(rig.read_commit(1).is_ok());
    assert_eq!(
        std::fs::read(
            rig.r
                .archive
                .join(history_path(&Digest::of(b"C")).unwrap().as_str())
        )
        .unwrap(),
        b"C"
    );
    assert_eq!(
        std::fs::read(rig.r.archive.join("saved.tif")).unwrap(),
        b"C"
    );
}
#[test]
fn absent_history_requires_exact_retirement_proof_and_present_corruption_fails() {
    let mut rig = RetentionRig::new();
    rig.save(b"A");
    rig.save(b"B");
    let old = rig
        .r
        .archive
        .join(history_path(&Digest::of(b"A")).unwrap().as_str());
    std::fs::remove_file(&old).unwrap();
    assert!(rig.read_commit(0).is_err());
    std::fs::write(&old, b"A").unwrap();
    rig.retire();
    assert!(rig.read_commit(0).is_ok());
    std::fs::write(&old, b"corrupt").unwrap();
    assert!(rig.read_commit(0).is_err());
}
#[test]
fn retirement_retries_each_durable_transition() {
    for point in [
        FaultPoint::GeneratedRetirementCommitted,
        FaultPoint::GeneratedRetirementReceiptStaged,
        FaultPoint::GeneratedRetirementReceiptsPublished,
        FaultPoint::GeneratedRetirementPayloadRemoved,
        FaultPoint::GeneratedRetirementRetainedRemoved,
    ] {
        let mut rig = RetentionRig::new();
        rig.save(b"A");
        rig.save(b"B");
        let plan = rig.plan();
        let mut guard = rig.guard();
        assert!(
            rig.store
                .retire_generated(&mut guard, plan.clone(), &Stop(point))
                .is_err()
        );
        let receipt = rig
            .store
            .retire_generated(&mut guard, plan, &NoFaults)
            .unwrap();
        rig.store.release(guard).unwrap();
        assert!(rig.read_commit(0).is_ok());
        assert_eq!(receipt.removed_digests, vec![Digest::of(b"A")]);
        let current = rig.plans.last().unwrap();
        let proof = rig
            .store
            .committed_projection(&rig.commits[1], current)
            .unwrap();
        assert!(matches!(
            rig.store.apply_projection(&proof).unwrap(),
            ProjectionOutcome::AlreadyCurrent
        ));
    }
}
#[test]
fn unknown_retained_and_payload_bytes_are_preserved() {
    for retained in [false, true] {
        let mut rig = RetentionRig::new();
        rig.save(b"A");
        rig.save(b"B");
        let current = rig.plans.last().unwrap();
        let target = if retained {
            rig.r.archive.join(format!(
                ".photo-sorter/generated/{}/retained-{}",
                current.sidecar, current.version
            ))
        } else {
            rig.r
                .archive
                .join(history_path(&Digest::of(b"A")).unwrap().as_str())
        };
        std::fs::write(&target, b"foreign").unwrap();
        let plan = rig.plan();
        let mut guard = rig.guard();
        assert!(
            rig.store
                .retire_generated(&mut guard, plan, &NoFaults)
                .is_err()
        );
        rig.store.release(guard).unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"foreign");
    }
}
#[test]
fn completion_marker_interruption_recovers_and_rejects_foreign_stage() {
    let mut rig = RetentionRig::new();
    rig.save(b"A");
    let plan = rig.plans[0].clone();
    let directory = rig
        .r
        .archive
        .join(format!(".photo-sorter/generated/{}", plan.sidecar));
    let completed = directory.join(format!("completed-{}.json", plan.version));
    std::fs::remove_file(&completed).unwrap();
    let proof = rig
        .store
        .committed_projection(&rig.commits[0], &plan)
        .unwrap();
    assert!(
        rig.store
            .apply_projection_with_faults(&proof, &Stop(FaultPoint::GeneratedCompletedStaged))
            .is_err()
    );
    assert!(!completed.exists());
    let staged_marker = std::fs::read_dir(&directory)
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().starts_with("marker-"))
        .unwrap()
        .path();
    std::fs::write(&staged_marker, b"{").unwrap();
    assert!(matches!(
        rig.store.apply_projection(&proof).unwrap(),
        ProjectionOutcome::AlreadyCurrent
    ));
    assert_eq!(std::fs::read(&staged_marker).unwrap(), b"{");
    let staged = directory.join(format!("staged-{}", plan.version));
    std::fs::write(&staged, b"foreign").unwrap();
    assert!(matches!(
        rig.store.apply_projection(&proof).unwrap(),
        ProjectionOutcome::Conflict { .. }
    ));
    assert_eq!(std::fs::read(staged).unwrap(), b"foreign");
}
#[test]
fn superseding_interrupted_cleanup_reuses_old_exact_receipt() {
    let mut rig = RetentionRig::new();
    rig.save(b"A");
    rig.save(b"B");
    let plan = rig.plan();
    let mut guard = rig.guard();
    assert!(
        rig.store
            .retire_generated(
                &mut guard,
                plan.clone(),
                &Stop(FaultPoint::GeneratedRetirementReceiptsPublished)
            )
            .is_err()
    );
    rig.store.release(guard).unwrap();
    rig.catalog().apply_commit(&plan.commit).unwrap();
    rig.commits.push(plan.commit);
    let receipt_path = rig.r.archive.join(format!(
        ".photo-sorter/generated-retirements/{}.json",
        rig.plans[0].version
    ));
    let receipt = std::fs::read(&receipt_path).unwrap();
    rig.save(b"C");
    rig.retire();
    assert_eq!(std::fs::read(receipt_path).unwrap(), receipt);
    assert!(rig.read_commit(0).is_ok());
    assert!(rig.read_commit(1).is_ok());
}
#[test]
fn retirement_requires_current_commit_and_history_proof() {
    for corrupt_history in [false, true] {
        let mut rig = RetentionRig::new();
        rig.save(b"A");
        rig.save(b"B");
        let plan = rig.plan();
        if corrupt_history {
            std::fs::write(
                rig.r
                    .archive
                    .join(history_path(&Digest::of(b"B")).unwrap().as_str()),
                b"corrupt",
            )
            .unwrap();
        } else {
            let commit = &rig.commits[1];
            std::fs::remove_file(rig.r.archive.join(format!(
                ".photo-sorter/commits/{}/{}-{}.json",
                commit.writer_id, commit.writer_sequence, commit.commit_id
            )))
            .unwrap();
        }
        let mut guard = rig.guard();
        assert!(
            rig.store
                .retire_generated(&mut guard, plan, &NoFaults)
                .is_err()
        );
        rig.store.release(guard).unwrap();
        assert_eq!(
            std::fs::read(
                rig.r
                    .archive
                    .join(history_path(&Digest::of(b"A")).unwrap().as_str())
            )
            .unwrap(),
            b"A"
        );
    }
}
#[test]
fn receipt_cannot_excuse_a_different_version_body() {
    let mut rig = RetentionRig::new();
    rig.save(b"A");
    rig.save(b"B");
    rig.retire();
    let receipt = rig.r.archive.join(format!(
        ".photo-sorter/generated-retirements/{}.json",
        rig.plans[0].version
    ));
    let mut proof: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    proof["version"]["body"] = serde_json::to_value(body(b"other")).unwrap();
    std::fs::write(receipt, serde_json::to_vec(&proof).unwrap()).unwrap();
    assert!(rig.read_commit(0).is_err());
}
