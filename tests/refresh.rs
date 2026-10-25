mod support;
use photo_sorter::{domain::*, edit::*};
#[test]
fn refresh_without_closed_assertion_stages_only() {
    let sessions = ApplicationSessions::default();
    assert!(sessions.authorize(false, false).is_err());
}
#[test]
fn refresh_disconnect_or_restart_revokes_authorization() {
    let sessions = ApplicationSessions::default();
    let guard = sessions.authorize(true, false).unwrap();
    let id = guard.id();
    assert!(sessions.authorized(id));
    drop(guard);
    assert!(!sessions.authorized(id));
    assert!(!ApplicationSessions::default().authorized(id));
}
#[test]
fn waiting_refresh_requires_new_assertion() {
    let sessions = ApplicationSessions::default();
    let old = sessions.authorize(true, false).unwrap();
    sessions.revoke(old.id());
    assert!(!sessions.authorized(old.id()));
    let fresh = sessions.authorize(true, false).unwrap();
    assert!(sessions.authorized(fresh.id()));
    assert_ne!(old.id(), fresh.id());
}
#[test]
fn refresh_no_wait_cannot_apply() {
    assert!(
        ApplicationSessions::default()
            .authorize(true, true)
            .is_err()
    );
}
fn setup(
    r: &support::TestRig,
) -> (
    photo_sorter::catalog::Catalog,
    GroupId,
    SidecarId,
    VersionId,
) {
    use photo_sorter::{archive::*, catalog::*, fs::*};
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&state).unwrap();
    let group = c.create_group(false).unwrap();
    let sidecar = SidecarId::new();
    let path = ArchivePath::parse("RAW/a.ORF.rrdata").unwrap();
    c.register_sidecar(sidecar, Some(group), path.clone(), "rapidraw-base")
        .unwrap();
    std::fs::create_dir(r.local.join("RAW")).unwrap();
    std::fs::write(r.local.join(path.as_str()), br#"{"rating":3}"#).unwrap();
    let intent = SnapshotIntent {
        sidecar,
        parents: vec![],
        operation: OperationId::new(),
        payload: SnapshotPayload::Content(br#"{"rating":3}"#.to_vec()),
        origin: VersionOrigin::Observed,
    };
    let old = VersionStore::new(&mut c, &state)
        .record(intent.clone())
        .unwrap();
    let archive = ArchiveId::new();
    let writer = WriterId::new();
    let initial = CommitEnvelope::new(
        archive,
        writer,
        1,
        vec![],
        vec![
            ArchiveRecord::VersionAdded {
                version: c.version(old).unwrap(),
                operation: intent.operation,
            },
            ArchiveRecord::HeadsChanged {
                sidecar,
                expected: vec![],
                heads: vec![old],
            },
        ],
    )
    .unwrap();
    c.apply_commit(&initial).unwrap();
    c.acknowledge_snapshot(sidecar, old).unwrap();
    let incoming = Version {
        id: VersionId::new(),
        sidecar,
        parents: vec![old],
        body: VersionBody::Content {
            digest: Digest::of(br#"{"rating":5}"#),
            size: 12,
        },
        origin: VersionOrigin::Derived,
    };
    let plan = ProjectionPlan {
        sidecar,
        path,
        version: incoming.id,
        expected: c.version(old).unwrap().body,
        body: incoming.body.clone(),
        owner: "rapidraw-base".into(),
    };
    let id = incoming.id;
    c.apply_commit(
        &CommitEnvelope::new(
            archive,
            writer,
            2,
            vec![initial.commit_id],
            vec![
                ArchiveRecord::VersionAdded {
                    version: incoming,
                    operation: OperationId::new(),
                },
                ArchiveRecord::HeadsChanged {
                    sidecar,
                    expected: vec![old],
                    heads: vec![id],
                },
                ArchiveRecord::ProjectionPlanned { plan },
            ],
        )
        .unwrap(),
    )
    .unwrap();
    (c, group, sidecar, id)
}
#[test]
fn refresh_snapshots_displaced_bytes() {
    use photo_sorter::fs::*;
    let r = support::TestRig::new();
    let (mut c, group, _, incoming) = setup(&r);
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let sessions = ApplicationSessions::default();
    let guard = sessions.authorize(true, false).unwrap();
    let mut refresh = RefreshCoordinator {
        catalog: &mut c,
        local: &local,
        state: &state,
        sessions: &sessions,
    };
    refresh.stage(incoming, br#"{"rating":5}"#).unwrap();
    let report = refresh
        .apply(
            RefreshRequest {
                groups: vec![group],
                application_session: guard.id(),
                operation_id: OperationId::new(),
            },
            &NoFaults,
        )
        .unwrap();
    assert_eq!(report.applied, vec![incoming]);
    assert_eq!(
        std::fs::read(r.local.join("RAW/a.ORF.rrdata")).unwrap(),
        br#"{"rating":5}"#
    );
    let old = state
        .read(
            &ArchivePath::parse(&format!(
                ".photo-sorter/snapshots/{}",
                &Digest::of(br#"{"rating":3}"#).as_str()[7..]
            ))
            .unwrap(),
            100,
        )
        .unwrap();
    assert_eq!(old, br#"{"rating":3}"#);
}
struct LateSave(std::path::PathBuf);
impl FaultInjector for LateSave {
    fn hit(&self, point: FaultPoint) -> photo_sorter::error::AppResult<()> {
        if point == FaultPoint::BeforeProjection {
            std::fs::write(&self.0, br#"{"rating":4}"#)?;
        }
        Ok(())
    }
}
#[test]
fn refresh_activity_invalidates_boundary() {
    use photo_sorter::fs::*;
    let r = support::TestRig::new();
    let (mut c, group, sidecar, incoming) = setup(&r);
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let sessions = ApplicationSessions::default();
    let guard = sessions.authorize(true, false).unwrap();
    let mut refresh = RefreshCoordinator {
        catalog: &mut c,
        local: &local,
        state: &state,
        sessions: &sessions,
    };
    refresh.stage(incoming, br#"{"rating":5}"#).unwrap();
    let report = refresh
        .apply(
            RefreshRequest {
                groups: vec![group],
                application_session: guard.id(),
                operation_id: OperationId::new(),
            },
            &LateSave(r.local.join("RAW/a.ORF.rrdata")),
        )
        .unwrap();
    assert_eq!(report.conflicts, vec![sidecar]);
    assert!(!sessions.authorized(guard.id()));
    let retained = std::fs::read_dir(r.local.join(".photo-sorter/refresh"))
        .unwrap()
        .any(|entry| std::fs::read(entry.unwrap().path()).unwrap() == br#"{"rating":4}"#);
    assert!(retained);
}
#[test]
fn watcher_expected_write_not_new_edit() {
    use photo_sorter::fs::*;
    let r = support::TestRig::new();
    let (mut c, group, sidecar, incoming) = setup(&r);
    let local = RootHandle::open(&r.local, RootPolicy::Local).unwrap();
    let state = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let sessions = ApplicationSessions::default();
    let guard = sessions.authorize(true, false).unwrap();
    let mut refresh = RefreshCoordinator {
        catalog: &mut c,
        local: &local,
        state: &state,
        sessions: &sessions,
    };
    refresh.stage(incoming, br#"{"rating":5}"#).unwrap();
    refresh
        .apply(
            RefreshRequest {
                groups: vec![group],
                application_session: guard.id(),
                operation_id: OperationId::new(),
            },
            &NoFaults,
        )
        .unwrap();
    let owner = SidecarOwner {
        id: sidecar,
        group: Some(group),
        path: ArchivePath::parse("RAW/a.ORF.rrdata").unwrap(),
        kind: SidecarKind::RapidRaw,
        virtual_copy: None,
    };
    assert!(matches!(
        Snapshotter::new(&local, &state, &mut c, &r.clock, 2, 100)
            .capture(&owner)
            .unwrap(),
        SnapshotObservation::StableContent { changed: false, .. }
    ));
    assert!(!c.snapshot_dirty(sidecar).unwrap());
}
