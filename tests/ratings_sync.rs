mod support;
use photo_sorter::{
    catalog::*,
    domain::*,
    fs::{RootHandle, RootPolicy},
    immich::*,
    qualification::*,
};
use std::os::unix::fs::PermissionsExt;
fn stars(n: u8) -> RatingObservation {
    RatingObservation::Stars(Rating::try_from(n).unwrap())
}
fn setup(
    r: &support::TestRig,
    server: &support::http::RatingServer,
) -> (Catalog, ImmichClient, GroupId) {
    let key = r.state.join("key");
    std::fs::write(&key, b"fixture-only-key").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let client = ImmichClient::new(server.origin.clone(), &key).unwrap();
    let mut c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    let group = c.create_group(false).unwrap();
    c.attach_member(
        group,
        OriginalMember {
            path: ArchivePath::parse("JPEG/a.JPG").unwrap(),
            digest: Digest::of(b"jpeg"),
            size: 4,
            role: MediaRole::Jpeg,
        },
        PairEvidence::UserSelected {
            reason: "fixture".into(),
        },
    )
    .unwrap();
    c.bind_asset(&server.origin, &client.asset(server.asset).unwrap(), group)
        .unwrap();
    (c, client, group)
}
fn evidence(server: &support::http::RatingServer) -> CapabilityRegistry {
    CapabilityRegistry::new(vec![CapabilityReport {
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
    }])
}
#[test]
fn outbound_disabled_until_configured() {
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (mut c, client, group) = setup(&r, &server);
    let id = c
        .prepare_rating_write(
            group,
            server.asset,
            stars(0),
            Rating::try_from(3).unwrap(),
            OperationId::new(),
            &server.origin,
        )
        .unwrap();
    let report = RatingSynchronizer {
        catalog: &mut c,
        client: &client,
    }
    .advance(id)
    .unwrap();
    assert!(report.waiting);
    assert!(server.state.writes.lock().unwrap().is_empty());
}
#[test]
fn one_sided_and_equal_changes_converge() {
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (mut c, client, group) = setup(&r, &server);
    let client = client.with_rating_policy(RatingWriteMode::BestEffort, evidence(&server));
    for (from, to) in [(0, 3), (3, 5), (5, 0)] {
        let id = c
            .prepare_rating_write(
                group,
                server.asset,
                stars(from),
                Rating::try_from(to).unwrap(),
                OperationId::new(),
                &server.origin,
            )
            .unwrap();
        assert!(
            RatingSynchronizer {
                catalog: &mut c,
                client: &client
            }
            .advance(id)
            .unwrap()
            .confirmed
        );
    }
    assert_eq!(
        *server.state.writes.lock().unwrap(),
        vec![Some(3), Some(5), None]
    );
}
#[test]
fn lost_response_then_new_remote_value_not_overwritten() {
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (mut c, client, group) = setup(&r, &server);
    let client = client.with_rating_policy(RatingWriteMode::BestEffort, evidence(&server));
    let id = c
        .prepare_rating_write(
            group,
            server.asset,
            stars(0),
            Rating::try_from(3).unwrap(),
            OperationId::new(),
            &server.origin,
        )
        .unwrap();
    server
        .state
        .drop_put
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let first = RatingSynchronizer {
        catalog: &mut c,
        client: &client,
    }
    .advance(id)
    .unwrap();
    assert!(first.waiting);
    *server.state.rating.lock().unwrap() = Some(5);
    let next = RatingSynchronizer {
        catalog: &mut c,
        client: &client,
    }
    .advance(id)
    .unwrap();
    assert!(next.conflict);
    assert_eq!(*server.state.rating.lock().unwrap(), Some(5));
    assert_eq!(server.state.writes.lock().unwrap().len(), 1);
}
#[test]
fn schema_upgrade_disables_writes() {
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (mut c, client, group) = setup(&r, &server);
    let client = client.with_rating_policy(RatingWriteMode::BestEffort, evidence(&server));
    *server.state.version.lock().unwrap() = "99.0.0".into();
    let id = c
        .prepare_rating_write(
            group,
            server.asset,
            stars(0),
            Rating::try_from(3).unwrap(),
            OperationId::new(),
            &server.origin,
        )
        .unwrap();
    assert!(
        RatingSynchronizer {
            catalog: &mut c,
            client: &client
        }
        .advance(id)
        .unwrap()
        .waiting
    );
    assert!(server.state.writes.lock().unwrap().is_empty());
}
#[test]
fn rating_save_does_not_clobber_pending_adjustments() {
    use photo_sorter::edit::*;
    let r = support::TestRig::new();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let mut c = Catalog::open(&root).unwrap();
    let group = c.create_group(false).unwrap();
    let sidecar = SidecarId::new();
    c.register_sidecar(
        sidecar,
        Some(group),
        ArchivePath::parse("RAW/a.ORF.rrdata").unwrap(),
        "rapidraw-base",
    )
    .unwrap();
    let base = VersionStore::new(&mut c, &root)
        .record(SnapshotIntent {
            sidecar,
            parents: vec![],
            operation: OperationId::new(),
            payload: SnapshotPayload::Content(
                br#"{"version":1,"rating":3,"adjustments":{"exposure":0}}"#.to_vec(),
            ),
            origin: VersionOrigin::Observed,
        })
        .unwrap();
    let local = VersionStore::new(&mut c, &root)
        .record(SnapshotIntent {
            sidecar,
            parents: vec![base],
            operation: OperationId::new(),
            payload: SnapshotPayload::Content(
                br#"{"version":1,"rating":3,"adjustments":{"exposure":1.2}}"#.to_vec(),
            ),
            origin: VersionOrigin::Observed,
        })
        .unwrap();
    let derived = derive_rating_version(
        &mut c,
        &root,
        RatingDerivation {
            sidecar,
            local: Some(local),
            remote: Some(base),
            previous: None,
            rating: Rating::try_from(5).unwrap(),
            operation: OperationId::new(),
        },
        &RapidRawAdapter::for_version("1.6.6").unwrap(),
    )
    .unwrap();
    assert_eq!(c.local_head(sidecar).unwrap(), Some(local));
    assert_ne!(derived, local);
    let VersionBody::Content { digest, .. } = c.version(derived).unwrap().body else {
        panic!("content expected")
    };
    let value: serde_json::Value = serde_json::from_slice(
        &root
            .read(
                &ArchivePath::parse(&format!(
                    ".photo-sorter/snapshots/{}",
                    &digest.as_str()[7..]
                ))
                .unwrap(),
                1024,
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["rating"], 5);
    assert_eq!(value["adjustments"]["exposure"], 1.2);
    assert!(!r.local.join("RAW/a.ORF").exists());
}
#[test]
fn both_changed_conflict() {
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (mut c, client, group) = setup(&r, &server);
    let client = client.with_rating_policy(RatingWriteMode::BestEffort, evidence(&server));
    let id = c
        .prepare_rating_write(
            group,
            server.asset,
            stars(0),
            Rating::try_from(3).unwrap(),
            OperationId::new(),
            &server.origin,
        )
        .unwrap();
    *server.state.rating.lock().unwrap() = Some(5);
    let report = RatingSynchronizer {
        catalog: &mut c,
        client: &client,
    }
    .advance(id)
    .unwrap();
    assert!(report.conflict);
    assert!(server.state.writes.lock().unwrap().is_empty());
    assert!(c.protection(group).unwrap().conflict);
}
#[test]
fn readonly_key_keeps_mapping_usable() {
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (mut c, client, group) = setup(&r, &server);
    let client = client.with_rating_policy(RatingWriteMode::BestEffort, evidence(&server));
    server
        .state
        .write_status
        .store(403, std::sync::atomic::Ordering::SeqCst);
    let id = c
        .prepare_rating_write(
            group,
            server.asset,
            stars(0),
            Rating::try_from(3).unwrap(),
            OperationId::new(),
            &server.origin,
        )
        .unwrap();
    let report = RatingSynchronizer {
        catalog: &mut c,
        client: &client,
    }
    .advance(id)
    .unwrap();
    assert!(report.waiting);
    assert!(client.asset(server.asset).is_ok());
    assert!(server.state.writes.lock().unwrap().is_empty());
}
fn saved_raw(r: &support::TestRig, c: &mut Catalog, group: GroupId) -> (SidecarId, VersionId) {
    use photo_sorter::edit::*;
    c.attach_member(
        group,
        OriginalMember {
            path: ArchivePath::parse("RAW/a.ORF").unwrap(),
            digest: Digest::of(b"raw"),
            size: 3,
            role: MediaRole::Raw,
        },
        PairEvidence::UserSelected {
            reason: "fixture RAW pair".into(),
        },
    )
    .unwrap();
    let sidecar = SidecarId::new();
    c.register_sidecar(
        sidecar,
        Some(group),
        ArchivePath::parse("RAW/a.ORF.rrdata").unwrap(),
        "rapidraw-base",
    )
    .unwrap();
    let root = RootHandle::open(&r.state, RootPolicy::Local).unwrap();
    let version = VersionStore::new(c, &root)
        .record(SnapshotIntent {
            sidecar,
            parents: vec![],
            operation: OperationId::new(),
            payload: SnapshotPayload::Content(
                br#"{"version":1,"rating":3,"adjustments":{"exposure":0.7}}"#.to_vec(),
            ),
            origin: VersionOrigin::Observed,
        })
        .unwrap();
    (sidecar, version)
}
#[test]
fn evicted_raw_not_downloaded_for_rating() {
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (mut c, client, group) = setup(&r, &server);
    let (sidecar, version) = saved_raw(&r, &mut c, group);
    *server.state.rating.lock().unwrap() = Some(3);
    let scope = || RatingScope {
        state_root: r.state.clone(),
        editor_version: "1.6.6".into(),
        after: None,
        limit: 128,
    };
    let first = RatingSynchronizer {
        catalog: &mut c,
        client: &client,
    }
    .poll(scope())
    .unwrap();
    assert_eq!(first.conflicts, 0);
    *server.state.rating.lock().unwrap() = Some(5);
    let second = RatingSynchronizer {
        catalog: &mut c,
        client: &client,
    }
    .poll(scope())
    .unwrap();
    assert_eq!(second.pending, 1);
    let reader = rusqlite::Connection::open(r.state.join("catalog.sqlite")).unwrap();
    let before: u64 = reader
        .query_row("SELECT count(*) FROM versions", [], |r| r.get(0))
        .unwrap();
    RatingSynchronizer {
        catalog: &mut c,
        client: &client,
    }
    .poll(scope())
    .unwrap();
    let after: u64 = reader
        .query_row("SELECT count(*) FROM versions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);

    assert_eq!(c.local_head(sidecar).unwrap(), Some(version));
    assert!(!r.local.join("RAW/a.ORF").exists());
    assert!(server.state.writes.lock().unwrap().is_empty());
}
#[test]
fn unknown_initial_values_conflict() {
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (mut c, client, group) = setup(&r, &server);
    saved_raw(&r, &mut c, group);
    *server.state.rating.lock().unwrap() = Some(5);
    let report = RatingSynchronizer {
        catalog: &mut c,
        client: &client,
    }
    .poll(RatingScope {
        state_root: r.state.clone(),
        editor_version: "1.6.6".into(),
        after: None,
        limit: 128,
    })
    .unwrap();
    assert_eq!(report.conflicts, 1);
    assert!(server.state.writes.lock().unwrap().is_empty());
}
#[test]
fn prepared_rating_never_sends_after_group_conflict() {
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (mut c, client, group) = setup(&r, &server);
    let client = client.with_rating_policy(RatingWriteMode::BestEffort, evidence(&server));
    let id = c
        .prepare_rating_write(
            group,
            server.asset,
            stars(0),
            Rating::try_from(3).unwrap(),
            OperationId::new(),
            &server.origin,
        )
        .unwrap();
    c.set_protection(group, false, true).unwrap();
    let report = RatingSynchronizer {
        catalog: &mut c,
        client: &client,
    }
    .advance(id)
    .unwrap();
    assert!(report.waiting);
    assert!(server.state.writes.lock().unwrap().is_empty());
}
#[test]
fn newer_unsent_local_rating_supersedes_old_prepared_intent() {
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (mut c, client, group) = setup(&r, &server);
    let client = client.with_rating_policy(RatingWriteMode::BestEffort, evidence(&server));
    let old = c
        .prepare_rating_write(
            group,
            server.asset,
            stars(0),
            Rating::try_from(3).unwrap(),
            OperationId::new(),
            &server.origin,
        )
        .unwrap();
    let new = c
        .prepare_rating_write(
            group,
            server.asset,
            stars(0),
            Rating::try_from(5).unwrap(),
            OperationId::new(),
            &server.origin,
        )
        .unwrap();
    let report = RatingSynchronizer {
        catalog: &mut c,
        client: &client,
    }
    .advance(old)
    .unwrap();
    assert!(!report.confirmed);
    assert!(server.state.writes.lock().unwrap().is_empty());
    assert!(
        RatingSynchronizer {
            catalog: &mut c,
            client: &client
        }
        .advance(new)
        .unwrap()
        .confirmed
    );
    assert_eq!(*server.state.writes.lock().unwrap(), vec![Some(5)]);
}
#[test]
fn reindexed_asset_cannot_supply_stars_to_old_mapping() {
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (mut c, client, _group) = setup(&r, &server);
    *server.state.checksum.lock().unwrap() = "changed-upstream-checksum".into();
    let report = RatingSynchronizer {
        catalog: &mut c,
        client: &client,
    }
    .poll(RatingScope {
        state_root: r.state,
        editor_version: "1.6.6".into(),
        after: None,
        limit: 128,
    })
    .unwrap();
    assert_eq!(report.observed, 0);
    assert_eq!(report.conflicts, 1);
}
#[test]
fn outbound_rating_refuses_replaced_managed_root_even_with_matching_runtime_evidence() {
    let r = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (_catalog, client, _group) = setup(&r, &server);
    let client = client
        .with_rating_policy(RatingWriteMode::BestEffort, evidence(&server))
        .with_managed_roots(vec![RootHandle::open(&r.local, RootPolicy::Local).unwrap()]);
    std::fs::rename(&r.local, r._temp.path().join("old-local")).unwrap();
    std::fs::create_dir(&r.local).unwrap();
    let checksum = server.state.checksum.lock().unwrap().clone();
    let result = client.set_rating(RatingWrite {
        asset: server.asset,
        expected: stars(0),
        desired: Rating::try_from(5).unwrap(),
        jpeg_checksum: checksum,
    });
    assert!(
        result
            .unwrap_err()
            .message
            .contains("managed root replaced")
    );
    assert!(server.state.writes.lock().unwrap().is_empty());
}

#[test]
fn rating_conflict_evidence_is_immutable_when_later_observations_change() {
    let rig = support::TestRig::new();
    let mut catalog =
        Catalog::open(&RootHandle::open(&rig.state, RootPolicy::Local).unwrap()).unwrap();
    let group = catalog.create_group(false).unwrap();
    let first = catalog
        .record_rating_conflict(group, "rating", serde_json::json!({"local":3,"remote":5}))
        .unwrap();
    let second = catalog
        .record_rating_conflict(group, "rating", serde_json::json!({"local":0,"remote":5}))
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(
        catalog.conflict(first).unwrap().details,
        serde_json::json!({"local":3,"remote":5})
    );
    assert_eq!(
        catalog
            .record_rating_conflict(group, "rating", serde_json::json!({"local":0,"remote":5}))
            .unwrap(),
        second
    );
}

#[test]
fn both_apps_agreement_journals_resolution_without_an_outgoing_write() {
    use photo_sorter::archive::*;
    let rig = support::TestRig::new();
    let server = support::http::RatingServer::new();
    let (mut catalog, client, group) = setup(&rig, &server);
    let (sidecar, version) = saved_raw(&rig, &mut catalog, group);
    let reader = rusqlite::Connection::open(rig.state.join("catalog.sqlite")).unwrap();
    let operation: String = reader
        .query_row(
            "SELECT operation FROM versions WHERE id=?",
            [version.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let mut records = vec![ArchiveRecord::GroupCreated { id: group }];
    for member in catalog.group(group).unwrap().members {
        records.push(ArchiveRecord::MemberAttached {
            group,
            evidence: PairEvidence::ObservedPlacement {
                path: member.path.clone(),
                digest: member.digest.clone(),
            },
            member,
        });
    }
    records.extend([
        ArchiveRecord::SidecarCreated {
            id: sidecar,
            group: Some(group),
            path: ArchivePath::parse("RAW/a.ORF.rrdata").unwrap(),
            owner: "rapidraw-base".into(),
        },
        ArchiveRecord::VersionAdded {
            version: catalog.version(version).unwrap(),
            operation: serde_json::from_value(operation.into()).unwrap(),
        },
        ArchiveRecord::HeadsChanged {
            sidecar,
            expected: vec![],
            heads: vec![version],
        },
    ]);
    let archive = ArchiveId::new();
    catalog
        .apply_commit(&CommitEnvelope::new(archive, WriterId::new(), 1, vec![], records).unwrap())
        .unwrap();
    let conflict = catalog
        .record_rating_conflict(group, "rating", serde_json::json!({"local":3,"remote":5}))
        .unwrap();
    *server.state.rating.lock().unwrap() = Some(3);
    RatingSynchronizer {
        catalog: &mut catalog,
        client: &client,
    }
    .poll(RatingScope {
        state_root: rig.state.clone(),
        editor_version: "1.6.6".into(),
        after: None,
        limit: 128,
    })
    .unwrap();
    assert!(
        !catalog.conflict(conflict).unwrap().resolved,
        "agreement must be shared before clearing"
    );
    let mut query = reader
        .prepare("SELECT records FROM pending_shared_records WHERE committed=0 ORDER BY rowid")
        .unwrap();
    let rows = query
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let convergence = rows
        .into_iter()
        .map(|s| serde_json::from_str::<Vec<ArchiveRecord>>(&s).unwrap())
        .find(|r| {
            r.iter()
                .any(|r| matches!(r, ArchiveRecord::RatingConverged { .. }))
        })
        .expect("fresh matching observations must queue shared convergence");
    catalog
        .apply_commit(
            &CommitEnvelope::new(archive, WriterId::new(), 1, vec![], convergence).unwrap(),
        )
        .unwrap();
    assert!(catalog.conflict(conflict).unwrap().resolved);
    assert!(server.state.writes.lock().unwrap().is_empty());
    assert!(!rig.local.join("RAW/a.ORF").exists());
}

#[test]
fn service_delivers_later_rating_when_oldest_intent_has_changed_server() {
    let old = support::http::RatingServer::new();
    let current = support::http::RatingServer::new();
    let mut later = None;
    let service = support::service::ServiceFixture::with_config(|config, r| {
        let (mut catalog, _, group) = setup(r, &old);
        catalog
            .prepare_rating_write(
                group,
                old.asset,
                stars(0),
                Rating::try_from(3).unwrap(),
                OperationId::new(),
                &old.origin,
            )
            .unwrap();
        catalog.set_protection(group, false, true).unwrap();
        let db = rusqlite::Connection::open(r.state.join("catalog.sqlite")).unwrap();
        db.execute(
            "UPDATE members SET path='JPEG/old.JPG' WHERE group_id=?",
            [group.to_string()],
        )
        .unwrap();
        drop(db);
        drop(catalog);
        let (mut catalog, _, group) = setup(r, &current);
        later = Some(
            catalog
                .prepare_rating_write(
                    group,
                    current.asset,
                    stars(0),
                    Rating::try_from(5).unwrap(),
                    OperationId::new(),
                    &current.origin,
                )
                .unwrap(),
        );
        config.immich = Some(photo_sorter::config::ImmichConfig {
            origin: current.origin.to_string(),
            key_file: r.state.join("key"),
            library_id: None,
            write_mode: "best_effort".into(),
        });
        persist_report(config, &evidence(&current).reports()[0]).unwrap();
    });
    let deadline = std::time::Instant::now();
    loop {
        let db = rusqlite::Connection::open(service.r.state.join("catalog.sqlite")).unwrap();
        let state: String = db
            .query_row(
                "SELECT state FROM rating_intents WHERE id=?",
                [later.unwrap().to_string()],
                |r| r.get(0),
            )
            .unwrap();
        if state == "confirmed" {
            break;
        }
        assert!(
            deadline.elapsed() < std::time::Duration::from_secs(8),
            "later intent remained {state}: {}",
            service.status()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(*current.state.writes.lock().unwrap(), vec![Some(5)]);
    assert!(old.state.writes.lock().unwrap().is_empty());
}
