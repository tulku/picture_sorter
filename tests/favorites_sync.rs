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
fn start_service(
    f: Fixture,
    archive: bool,
) -> (
    support::service::ServiceFixture,
    support::http::RatingServer,
) {
    use photo_sorter::config::*;
    use std::process::{Command, Stdio};
    *f.server.state.rating.lock().unwrap() = Some(3);
    let v =
        f.c.version(f.c.local_head(f.sidecar).unwrap().unwrap())
            .unwrap();
    let VersionBody::Content { digest, size } = v.body else {
        panic!()
    };
    let bytes = RootHandle::open(&f.r.state, RootPolicy::Local)
        .unwrap()
        .read(&snapshot_path(&digest).unwrap(), size)
        .unwrap();
    std::fs::create_dir_all(f.r.local.join("RAW")).unwrap();
    std::fs::write(f.r.local.join("RAW/a.ORF.rrdata"), bytes).unwrap();
    let Fixture {
        r,
        server,
        c,
        client: _,
        group,
        sidecar: _,
    } = f;
    let key = r.state.join("key");
    let report = qualify_immich(ImmichProbeRequest {
        origin: server.origin.clone(),
        key_file: key.clone(),
        asset: server.asset,
        isolated_asset_confirmed: true,
        fixture: true,
        favorites: true,
    })
    .unwrap();
    let evidence = r._temp.path().join("favorite-evidence.json");
    std::fs::write(&evidence, serde_json::to_vec(&report).unwrap()).unwrap();
    std::fs::set_permissions(&evidence, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut config = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    config.local_test_backend = true;
    config.poll_seconds = 1;
    config.debounce_seconds = 1;
    if archive {
        use photo_sorter::archive::*;
        let archive = ArchiveId::new();
        config.archive_id = Some(archive);
        let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
        ArchiveStore::initialize(
            &root,
            InitArchive {
                archive_id: archive,
                writer_id: config.writer_id,
            },
        )
        .unwrap();
        let store =
            ArchiveStore::open(root.try_clone().unwrap(), archive, config.writer_id).unwrap();
        let mut records = vec![ArchiveRecord::GroupCreated { id: group }];
        let mut staged = vec![];
        for (index, member) in c.group(group).unwrap().members.into_iter().enumerate() {
            let path =
                ArchivePath::parse(&format!(".photo-sorter/staging/fixture-{index}")).unwrap();
            root.write_new(&path, b"fixture").unwrap();
            records.push(ArchiveRecord::MemberAttached {
                group,
                member: member.clone(),
                evidence: PairEvidence::UserSelected {
                    reason: "generated fixture archive".into(),
                },
            });
            staged.push(StagedMember {
                member,
                staged: path,
            });
        }
        let commit = CommitEnvelope::new(archive, config.writer_id, 1, vec![], records).unwrap();
        let mut guard = store.acquire_publication().unwrap();
        let batch = store.journal_batch(&mut guard, 128).unwrap();
        store.ack_journal(&mut guard, batch.token).unwrap();
        store
            .publish_originals(
                &mut guard,
                PublicationPlan {
                    commit,
                    staged,
                    attempt: OperationId::new(),
                },
                &NoFaults,
            )
            .unwrap();
        store.release(guard).unwrap();
    }
    drop(c);
    config.immich = Some(ImmichConfig {
        origin: server.origin.to_string(),
        key_file: key,
        library_id: None,
        write_mode: "best_effort".into(),
    });
    config.capability_evidence = vec![evidence];
    let profile = r._temp.path().join("profile.toml");
    std::fs::write(&profile, toml::to_string(&config).unwrap()).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_photo_sorter"))
        .arg("--config")
        .arg(&profile)
        .args(["service", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut service = support::service::ServiceFixture { r, profile, child };
    let deadline = std::time::Instant::now();
    while !service.cli(&["service", "status"]).status.success() {
        assert!(service.child.try_wait().unwrap().is_none());
        assert!(deadline.elapsed() < std::time::Duration::from_secs(5));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    (service, server)
}
#[test]
fn actual_service_sends_favorite_and_keeps_stars_untouched() {
    use std::time::{Duration, Instant};
    let mut f = Fixture::new();
    f.save(serde_json::Value::Null);
    f.poll();
    f.save(serde_json::json!("pick"));
    f.poll();
    f.c.prepare_rating_write(
        f.group,
        f.server.asset,
        RatingObservation::Stars(Rating::try_from(3).unwrap()),
        Rating::try_from(4).unwrap(),
        OperationId::new(),
        &f.server.origin,
    )
    .unwrap();
    let (service, server) = start_service(f, false);
    let start = Instant::now();
    while *server.state.favorite.lock().unwrap() != Some(true) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "favorite service worker did not send: {:?}",
            service.status()
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(server.state.writes.lock().unwrap().is_empty());
}

#[test]
fn actual_cli_refresh_applies_incoming_favorite_and_preserves_edits() {
    use std::time::{Duration, Instant};
    let mut f = Fixture::new();
    f.save(serde_json::Value::Null);
    f.poll();
    *f.server.state.favorite.lock().unwrap() = Some(true);
    f.poll();
    let group = f.group.to_string();
    let incoming = f.c.favorite_pending_version(f.group).unwrap().unwrap();
    let (service, _server) = start_service(f, true);
    let init = service.cli(&["archive", "init"]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stdout)
    );
    let start = Instant::now();
    loop {
        let db = rusqlite::Connection::open(service.r.state.join("catalog.sqlite")).unwrap();
        let ready: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM projections WHERE json_extract(committed,'$.version')=? AND pending=1)",
                [incoming.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        drop(db);
        if ready {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "incoming favorite was not published: {:?}",
            service.status()
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    let path = service.r.local.join("RAW/a.ORF.rrdata");
    let before: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(before["flag"].is_null());
    assert!(!service.r.local.join("RAW/a.ORF").exists());
    let pinned = service.cli(&["pin", &group]);
    let pin: serde_json::Value = serde_json::from_slice(&pinned.stdout).unwrap();
    assert_eq!(pin["outcome"]["job"]["state"], "WaitingApplication");
    assert_eq!(pin["outcome"]["detail"]["originals_ready"], true);
    let refreshed = service.cli(&["refresh", &group, "--editor-closed"]);
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stdout)
    );
    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(after["flag"], "pick");
    assert_eq!(after["rating"], 3);
    assert_eq!(after["adjustments"], before["adjustments"]);
    assert_eq!(after["future"], 42);
    assert_eq!(
        std::fs::read(service.r.local.join("RAW/a.ORF")).unwrap(),
        b"fixture"
    );
}
#[test]
fn reject_does_not_bypass_an_unrelated_identity_conflict() {
    let mut f = Fixture::new();
    f.save(serde_json::json!("reject"));
    *f.server.state.favorite.lock().unwrap() = Some(true);
    f.poll();
    f.c.record_rating_conflict(
        f.group,
        "mapping_identity",
        serde_json::json!({"reason":"changed identity"}),
    )
    .unwrap();
    assert!(f.advance().waiting);
    assert_eq!(*f.server.state.favorite.lock().unwrap(), Some(true));
}
#[test]
fn stale_jpeg_binding_blocks_favorite_write() {
    let mut f = Fixture::new();
    f.save(serde_json::Value::Null);
    f.poll();
    f.save(serde_json::json!("pick"));
    f.poll();
    *f.server.state.checksum.lock().unwrap() = "different".into();
    assert!(f.advance().waiting);
    assert_eq!(*f.server.state.favorite.lock().unwrap(), Some(false));
}
#[test]
fn incoming_clear_and_reject_while_remote_clear_is_pending_preserve_history() {
    let mut f = Fixture::new();
    f.save(serde_json::json!("pick"));
    *f.server.state.favorite.lock().unwrap() = Some(true);
    f.poll();
    *f.server.state.favorite.lock().unwrap() = Some(false);
    f.poll();
    let clear = f.c.favorite_pending_version(f.group).unwrap().unwrap();
    assert!(f.pending()["flag"].is_null());
    f.save(serde_json::json!("reject"));
    assert_eq!(f.poll().conflicts, 0);
    let reject = f.c.favorite_pending_version(f.group).unwrap().unwrap();
    assert!(f.c.is_ancestor(clear, reject).unwrap());
    assert_eq!(f.pending()["flag"], "reject");
    assert!(f.c.next_favorite_intent(0).unwrap().is_none());
}
#[test]
fn changed_incoming_flag_replaces_staged_pick_without_live_echo() {
    let mut f = Fixture::new();
    f.save(serde_json::Value::Null);
    f.poll();
    *f.server.state.favorite.lock().unwrap() = Some(true);
    f.poll();
    let picked = f.c.favorite_pending_version(f.group).unwrap().unwrap();
    *f.server.state.favorite.lock().unwrap() = Some(false);
    assert_eq!(f.poll().conflicts, 0);
    let cleared = f.c.favorite_pending_version(f.group).unwrap().unwrap();
    assert_ne!(picked, cleared);
    assert!(f.c.is_ancestor(picked, cleared).unwrap());
    assert!(f.pending()["flag"].is_null());
    assert!(f.c.next_favorite_intent(0).unwrap().is_none());
}
#[test]
fn reject_after_pick_publication_settles_without_sidecar_or_favorite_conflict() {
    use std::time::{Duration, Instant};
    let mut f = Fixture::new();
    f.save(serde_json::Value::Null);
    f.poll();
    *f.server.state.favorite.lock().unwrap() = Some(true);
    f.poll();
    let incoming = f.c.favorite_pending_version(f.group).unwrap().unwrap();
    let sidecar = f.sidecar.to_string();
    let (service, server) = start_service(f, true);
    let start = Instant::now();
    loop {
        let db = rusqlite::Connection::open(service.r.state.join("catalog.sqlite")).unwrap();
        let ready: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM heads WHERE version=?)",
                [incoming.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        drop(db);
        if ready {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "incoming publication timed out"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    let path = service.r.local.join("RAW/a.ORF.rrdata");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    doc["flag"] = "reject".into();
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let start = Instant::now();
    loop {
        let db = rusqlite::Connection::open(service.r.state.join("catalog.sqlite")).unwrap();
        let heads: u64 = db
            .query_row(
                "SELECT count(*) FROM heads WHERE sidecar=?",
                [&sidecar],
                |r| r.get(0),
            )
            .unwrap();
        let conflicts: u64 = db
            .query_row(
                "SELECT count(*) FROM conflicts WHERE resolved=0 AND kind IN('favorite','sidecar')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        drop(db);
        if *server.state.favorite.lock().unwrap() == Some(false) && heads == 1 && conflicts == 0 {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "Reject failed to settle: {}",
            {
                let db =
                    rusqlite::Connection::open(service.r.state.join("catalog.sqlite")).unwrap();
                let baseline: String = db
                    .query_row(
                        "SELECT group_concat(body) FROM favorite_baselines",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                let versions:String=db.query_row("SELECT json_group_array(json_object('id',id,'origin',origin,'parents',(SELECT json_group_array(parent) FROM version_parents WHERE version=v.id))) FROM versions v",[],|r|r.get(0)).unwrap();
                format!("baseline={baseline} versions={versions}")
            }
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["flag"], "reject");
}
#[test]
fn added_jpeg_authority_ambiguity_blocks_an_existing_favorite_intent() {
    let mut f = Fixture::new();
    f.save(serde_json::Value::Null);
    f.poll();
    f.save(serde_json::json!("pick"));
    f.poll();
    let mut second = f.client.asset(f.server.asset).unwrap();
    second.id = uuid::Uuid::new_v4();
    f.c.bind_asset(&f.server.origin, &second, f.group).unwrap();
    assert!(f.advance().waiting);
    assert_eq!(*f.server.state.favorite.lock().unwrap(), Some(false));
}
