mod support;
use photo_sorter::{catalog::*, domain::*, edit::*, fs::*, immich::*, qualification::*};
use std::os::unix::fs::PermissionsExt;
struct Fixture {
    r: support::TestRig,
    server: support::http::RatingServer,
    c: Catalog,
    client: ImmichClient,
    group: GroupId,
    sidecar: SidecarId,
}
impl Fixture {
    fn new() -> Self {
        let r = support::TestRig::new();
        let server = support::http::RatingServer::new();
        let key = r.state.join("key");
        std::fs::write(&key, b"fixture").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let report = qualify_immich(ImmichProbeRequest {
            origin: server.origin.clone(),
            key_file: key.clone(),
            asset: server.asset,
            isolated_asset_confirmed: true,
            fixture: true,
            favorites: true,
        })
        .unwrap();
        let client = ImmichClient::new(server.origin.clone(), &key)
            .unwrap()
            .with_rating_policy(
                RatingWriteMode::BestEffort,
                CapabilityRegistry::new(vec![report]),
            );
        let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
        let mut c = Catalog::open(&root).unwrap();
        let group = c.create_group(false).unwrap();
        for (p, role) in [
            ("RAW/a.ORF", MediaRole::Raw),
            ("JPEG/a.JPG", MediaRole::Jpeg),
        ] {
            c.attach_member(
                group,
                OriginalMember {
                    path: ArchivePath::parse(p).unwrap(),
                    digest: Digest::of(b"fixture"),
                    size: 7,
                    role,
                },
                PairEvidence::UserSelected {
                    reason: "fixture".into(),
                },
            )
            .unwrap();
        }
        c.bind_asset(&server.origin, &client.asset(server.asset).unwrap(), group)
            .unwrap();
        let sidecar = SidecarId::new();
        c.register_sidecar(
            sidecar,
            Some(group),
            ArchivePath::parse("RAW/a.ORF.rrdata").unwrap(),
            "rapidraw-base",
        )
        .unwrap();
        Self {
            r,
            server,
            c,
            client,
            group,
            sidecar,
        }
    }
    fn save(&mut self, flag: serde_json::Value) -> VersionId {
        let root = RootHandle::open(&self.r.state, RootPolicy::Local).unwrap();
        let parents = self
            .c
            .local_head(self.sidecar)
            .unwrap()
            .into_iter()
            .collect();
        VersionStore::new(&mut self.c,&root).record(SnapshotIntent{sidecar:self.sidecar,parents,operation:OperationId::new(),origin:VersionOrigin::Observed,payload:SnapshotPayload::Content(serde_json::to_vec(&serde_json::json!({"version":1,"rating":3,"flag":flag,"adjustments":{"masks":[1,2]},"future":42})).unwrap())}).unwrap()
    }
    fn poll(&mut self) -> RatingPollReport {
        FavoriteSynchronizer {
            catalog: &mut self.c,
            client: &self.client,
        }
        .poll(RatingScope {
            state_root: self.r.state.clone(),
            editor_version: "1.6.6".into(),
            after: None,
            limit: 128,
        })
        .unwrap()
    }
    fn advance(&mut self) -> RatingSyncReport {
        let (_, id) = self.c.next_favorite_intent(0).unwrap().unwrap();
        FavoriteSynchronizer {
            catalog: &mut self.c,
            client: &self.client,
        }
        .advance(id)
        .unwrap()
    }
    fn pending(&self) -> serde_json::Value {
        let id = self
            .c
            .favorite_pending_version(self.group)
            .unwrap()
            .unwrap();
        let VersionBody::Content { digest, size } = self.c.version(id).unwrap().body else {
            panic!()
        };
        serde_json::from_slice(
            &RootHandle::open(&self.r.state, RootPolicy::Local)
                .unwrap()
                .read(&snapshot_path(&digest).unwrap(), size)
                .unwrap(),
        )
        .unwrap()
    }
}
#[test]
fn saved_pick_and_clearing_publish_independently_of_stars() {
    let mut f = Fixture::new();
    f.save(serde_json::Value::Null);
    f.poll();
    for (flag, wanted) in [
        (serde_json::json!("pick"), true),
        (serde_json::Value::Null, false),
    ] {
        f.save(flag);
        f.poll();
        assert!(f.advance().confirmed);
        f.poll();
        assert_eq!(*f.server.state.favorite.lock().unwrap(), Some(wanted));
        assert!(f.server.state.writes.lock().unwrap().is_empty());
    }
}
#[test]
fn incoming_favorite_is_staged_without_changing_live_observation_or_echoing() {
    let mut f = Fixture::new();
    let local = f.save(serde_json::Value::Null);
    f.poll();
    *f.server.state.favorite.lock().unwrap() = Some(true);
    f.poll();
    let doc = f.pending();
    assert_eq!(doc["flag"], "pick");
    assert_eq!(doc["rating"], 3);
    assert_eq!(doc["adjustments"]["masks"], serde_json::json!([1, 2]));
    assert_eq!(doc["future"], 42);
    assert_eq!(f.c.local_head(f.sidecar).unwrap(), Some(local));
    let pending = f.c.favorite_pending_version(f.group).unwrap();
    f.poll();
    assert_eq!(f.c.favorite_pending_version(f.group).unwrap(), pending);
    assert!(f.c.next_favorite_intent(0).unwrap().is_none());
}
#[test]
fn reject_cancels_pending_pick_and_clears_favorite_without_conflict() {
    let mut f = Fixture::new();
    f.save(serde_json::Value::Null);
    f.poll();
    *f.server.state.favorite.lock().unwrap() = Some(true);
    f.poll();
    let picked = f.c.favorite_pending_version(f.group).unwrap().unwrap();
    let rejected = f.save(serde_json::json!("reject"));
    let report = f.poll();
    assert_eq!(report.conflicts, 0);
    let replacement = f.c.favorite_pending_version(f.group).unwrap().unwrap();
    assert_ne!(replacement, picked);
    assert!(f.c.is_ancestor(picked, replacement).unwrap());
    assert!(f.c.is_ancestor(rejected, replacement).unwrap());
    assert_eq!(f.pending()["flag"], "reject");
    assert!(f.advance().confirmed);
    f.poll();
    assert_eq!(*f.server.state.favorite.lock().unwrap(), Some(false));
    assert_eq!(f.pending()["flag"], "reject");
}
#[test]
fn reject_wins_on_initial_observation_even_with_star_reject_conflict() {
    let mut f = Fixture::new();
    f.save(serde_json::json!("reject"));
    f.c.record_rating_conflict(
        f.group,
        "rating",
        serde_json::json!({"local":"Rejected","remote":{"Stars":3},"pending":null}),
    )
    .unwrap();
    *f.server.state.favorite.lock().unwrap() = Some(true);
    assert_eq!(f.poll().conflicts, 0);
    assert!(f.advance().confirmed);
    assert_eq!(*f.server.state.favorite.lock().unwrap(), Some(false));
}
#[test]
fn unavailable_favorite_does_not_clear_pick() {
    let mut f = Fixture::new();
    f.save(serde_json::json!("pick"));
    *f.server.state.favorite.lock().unwrap() = None;
    f.poll();
    assert!(f.c.next_favorite_intent(0).unwrap().is_none());
    assert!(f.c.favorite_pending_version(f.group).unwrap().is_none());
}
#[test]
fn unsent_intent_cannot_outlive_its_saved_source() {
    let mut f = Fixture::new();
    f.save(serde_json::Value::Null);
    f.poll();
    f.save(serde_json::json!("pick"));
    f.poll();
    f.save(serde_json::json!("reject"));
    let report = f.advance();
    assert!(!report.confirmed);
    assert_eq!(*f.server.state.favorite.lock().unwrap(), Some(false));
}
#[test]
fn uncertain_favorite_is_read_back_after_catalog_reopen_without_resend() {
    let mut f = Fixture::new();
    f.save(serde_json::Value::Null);
    f.poll();
    f.save(serde_json::json!("pick"));
    f.poll();
    f.server
        .state
        .drop_put
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(f.advance().waiting);
    let n = f.server.state.favorite_writes.lock().unwrap().len();
    let root = RootHandle::open(&f.r.state, RootPolicy::Local).unwrap();
    drop(f.c);
    f.c = Catalog::open(&root).unwrap();
    assert!(f.advance().confirmed);
    assert_eq!(f.server.state.favorite_writes.lock().unwrap().len(), n);
}
#[test]
fn selecting_reject_during_initial_favorite_disagreement_still_clears_favorite() {
    let mut f = Fixture::new();
    f.save(serde_json::Value::Null);
    *f.server.state.favorite.lock().unwrap() = Some(true);
    assert_eq!(f.poll().conflicts, 1);
    f.save(serde_json::json!("reject"));
    assert_eq!(f.poll().conflicts, 0);
    assert!(f.advance().confirmed);
    assert_eq!(*f.server.state.favorite.lock().unwrap(), Some(false));
}
#[test]
fn association_preserves_favorite_outbox_and_baseline() {
    let mut f = Fixture::new();
    f.save(serde_json::Value::Null);
    f.poll();
    f.save(serde_json::json!("pick"));
    f.poll();
    let other = loop {
        let id = f.c.create_group(false).unwrap();
        if id < f.group {
            break id;
        }
    };
    let merged =
        f.c.merge_groups(
            &[f.group, other],
            PairEvidence::UserSelected {
                reason: "fixture merge".into(),
            },
        )
        .unwrap();
    f.group = merged;
    assert!(f.advance().confirmed);
    assert_eq!(*f.server.state.favorite.lock().unwrap(), Some(true));
}
#[test]
fn favorite_agreement_journals_conflict_resolution_independently_of_stars() {
    use photo_sorter::archive::*;
    let mut f = Fixture::new();
    let version = f.save(serde_json::json!("reject"));
    let conflict =
        f.c.record_rating_conflict(
            f.group,
            "favorite",
            serde_json::json!({"local":"Neutral","remote":true}),
        )
        .unwrap();
    let star =
        f.c.record_rating_conflict(
            f.group,
            "rating",
            serde_json::json!({"local":"Rejected","remote":{"Stars":3},"pending":null}),
        )
        .unwrap();
    let archive = ArchiveId::new();
    let db = rusqlite::Connection::open(f.r.state.join("catalog.sqlite")).unwrap();
    db.execute(
        "INSERT INTO archived_versions(version) VALUES(?)",
        [version.to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO heads(sidecar,version) VALUES(?,?)",
        rusqlite::params![f.sidecar.to_string(), version.to_string()],
    )
    .unwrap();
    f.poll();
    let rows = db
        .prepare("SELECT records FROM pending_shared_records WHERE committed=0")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(!f.c.conflict(conflict).unwrap().resolved);
    let records = rows
        .into_iter()
        .map(|s| serde_json::from_str::<Vec<ArchiveRecord>>(&s).unwrap())
        .find(|rs| {
            rs.iter()
                .any(|r| matches!(r, ArchiveRecord::FavoriteConverged { .. }))
        })
        .expect("shared favorite convergence must be queued");
    f.c.apply_commit(&CommitEnvelope::new(archive, WriterId::new(), 1, vec![], records).unwrap())
        .unwrap();
    assert!(f.c.conflict(conflict).unwrap().resolved);
    assert!(!f.c.conflict(star).unwrap().resolved);
}
