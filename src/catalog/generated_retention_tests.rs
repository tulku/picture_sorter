use super::*;
#[test]
fn retirement_schema_refuses_downgrade_and_keeps_durable_batches() {
    let dir = tempfile::tempdir().unwrap();
    let root = RootHandle::open(dir.path(), crate::fs::RootPolicy::Local).unwrap();
    let catalog = Catalog::open(&root).unwrap();
    let version: i64 = catalog
        .db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    catalog
        .db
        .prepare("SELECT commit_id,current,cleaned FROM generated_retirement_batches")
        .unwrap();
    catalog
        .db
        .prepare("SELECT version FROM generated_retired_versions")
        .unwrap();
    catalog.db.pragma_update(None, "user_version", 4).unwrap();
    drop(catalog);
    assert!(Catalog::open(&root).is_err());
}

fn fixture(
    count: usize,
) -> (
    tempfile::TempDir,
    Catalog,
    crate::domain::ArchiveId,
    crate::domain::WriterId,
    crate::archive::ProjectionPlan,
    Vec<crate::domain::VersionId>,
) {
    use crate::{archive::*, domain::*};
    let dir = tempfile::tempdir().unwrap();
    let root = RootHandle::open(dir.path(), crate::fs::RootPolicy::Local).unwrap();
    let mut catalog = Catalog::open(&root).unwrap();
    let archive = ArchiveId::new();
    let writer = WriterId::new();
    let sidecar = SidecarId::new();
    let group = GroupId::new();
    let path = ArchivePath::parse("render.tif").unwrap();
    let mut records = vec![
        ArchiveRecord::GroupCreated { id: group },
        ArchiveRecord::SidecarCreated {
            id: sidecar,
            group: Some(group),
            path: path.clone(),
            owner: GENERATED_OUTPUT_OWNER.into(),
        },
    ];
    let mut ids = Vec::new();
    for n in 0..count {
        let id = VersionId::new();
        records.push(ArchiveRecord::VersionAdded {
            version: Version {
                id,
                sidecar,
                parents: ids.last().copied().into_iter().collect(),
                body: VersionBody::Content {
                    digest: Digest::of(n.to_string().as_bytes()),
                    size: n.to_string().len() as u64,
                },
                origin: VersionOrigin::Observed,
            },
            operation: OperationId::new(),
        });
        ids.push(id);
    }
    let current = *ids.last().unwrap();
    records.push(ArchiveRecord::HeadsChanged {
        sidecar,
        expected: vec![],
        heads: vec![current],
    });
    let plan = ProjectionPlan {
        sidecar,
        path,
        version: current,
        body: VersionBody::Content {
            digest: Digest::of((count - 1).to_string().as_bytes()),
            size: (count - 1).to_string().len() as u64,
        },
        expected: VersionBody::Reset,
        owner: GENERATED_OUTPUT_OWNER.into(),
    };
    records.push(ArchiveRecord::ProjectionPlanned { plan: plan.clone() });
    catalog
        .apply_commit(&CommitEnvelope::new(archive, writer, 1, vec![], records).unwrap())
        .unwrap();
    catalog
        .db
        .execute("UPDATE nas_projections SET applied=1", [])
        .unwrap();
    (dir, catalog, archive, writer, plan, ids)
}

#[test]
fn retention_retries_exact_batch_invalidates_cache_and_does_not_starve_after_128() {
    use crate::{catalog::GeneratedRetentionReceipt, domain::*};
    let (_dir, mut c, archive, writer, current, ids) = fixture(132);
    let digest = Digest::of(b"0");
    c.db.execute(
        "INSERT INTO history_receipts VALUES(?,?,1)",
        rusqlite::params![archive.to_string(), digest.as_str()],
    )
    .unwrap();
    assert_eq!(
        c.generated_retention_candidate().unwrap(),
        Some((current.sidecar, current.version))
    );
    let first = c
        .prepare_generated_retention(archive, writer, current.sidecar, current.version)
        .unwrap();
    assert_eq!(first.authorization.versions().len(), 128);
    assert!(first.authorization.removable().contains(&digest));
    assert_eq!(
        c.db.query_row("SELECT count(*) FROM history_receipts", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    c.apply_commit(&first.commit).unwrap();
    let retry = c
        .prepare_generated_retention(archive, writer, current.sidecar, current.version)
        .unwrap();
    assert_eq!(first.commit.commit_id, retry.commit.commit_id);
    c.acknowledge_generated_retention(&GeneratedRetentionReceipt {
        commit: first.commit,
        removed_digests: first.authorization.removable().to_vec(),
    })
    .unwrap();
    let second = c
        .prepare_generated_retention(archive, writer, current.sidecar, current.version)
        .unwrap();
    assert_eq!(second.authorization.versions().len(), 3);
    assert!(
        !second
            .authorization
            .versions()
            .iter()
            .any(|v| v.id == *ids.last().unwrap())
    );
    c.apply_commit(&second.commit).unwrap();
    c.acknowledge_generated_retention(&GeneratedRetentionReceipt {
        commit: second.commit,
        removed_digests: second.authorization.removable().to_vec(),
    })
    .unwrap();
    assert!(c.generated_retention_candidate().unwrap().is_none());
    assert_eq!(c.version(ids[0]).unwrap().id, ids[0]);
}

#[test]
fn ordinary_history_and_local_heads_protect_shared_digests() {
    use crate::{archive::*, domain::*};
    let (_dir, mut c, archive, writer, current, ids) = fixture(3);
    let other = SidecarId::new();
    let version = Version {
        id: VersionId::new(),
        sidecar: other,
        parents: vec![],
        body: c.version(ids[0]).unwrap().body,
        origin: VersionOrigin::Observed,
    };
    let ordinary = CommitEnvelope::new(
        archive,
        writer,
        2,
        vec![],
        vec![
            ArchiveRecord::SidecarCreated {
                id: other,
                group: None,
                path: ArchivePath::parse("ordinary.xmp").unwrap(),
                owner: "external-xmp".into(),
            },
            ArchiveRecord::VersionAdded {
                version,
                operation: OperationId::new(),
            },
        ],
    )
    .unwrap();
    c.apply_commit(&ordinary).unwrap();
    let plan = c
        .prepare_generated_retention(archive, writer, current.sidecar, current.version)
        .unwrap();
    assert!(!plan.authorization.removable().contains(&Digest::of(b"0")));
    assert!(plan.authorization.removable().contains(&Digest::of(b"1")));
    c.db.execute(
        "INSERT INTO local_heads VALUES(?,?,1)",
        rusqlite::params![current.sidecar.to_string(), ids[0].to_string()],
    )
    .unwrap();
    assert!(
        c.prepare_generated_retention(archive, writer, current.sidecar, current.version)
            .is_err()
    );
}

#[test]
fn replay_rejects_current_foreign_unarchived_and_duplicate_versions() {
    use crate::{archive::*, domain::*};
    for case in ["current", "duplicate", "foreign", "unarchived", "owner"] {
        let (_dir, mut c, archive, writer, mut current, ids) = fixture(3);
        let versions = match case {
            "current" => vec![current.version],
            "duplicate" => vec![ids[0], ids[0]],
            "foreign" => vec![VersionId::new()],
            _ => vec![ids[0]],
        };
        if case == "unarchived" {
            c.db.execute(
                "DELETE FROM archived_versions WHERE version=?",
                [ids[0].to_string()],
            )
            .unwrap();
        }
        if case == "owner" {
            current.owner = "ordinary".into();
        }
        let record = CommitEnvelope::new(
            archive,
            writer,
            2,
            vec![],
            vec![ArchiveRecord::GeneratedPayloadsRetired { current, versions }],
        )
        .unwrap();
        assert!(c.apply_commit(&record).is_err(), "{case}");
    }
}

#[test]
fn superseded_pending_batches_finish_without_repeating_completed_overlap() {
    use crate::{archive::*, domain::*};
    let (_dir, mut c, archive, writer, mut current, ids) = fixture(260);
    let pending = c
        .prepare_generated_retention(archive, writer, current.sidecar, current.version)
        .unwrap();
    c.apply_commit(&pending.commit).unwrap();
    let new = VersionId::new();
    let version = Version {
        id: new,
        sidecar: current.sidecar,
        parents: vec![current.version],
        body: VersionBody::Content {
            digest: Digest::of(b"latest"),
            size: 6,
        },
        origin: VersionOrigin::Observed,
    };
    let previous = current.version;
    current.version = new;
    current.body = version.body.clone();
    let publication = CommitEnvelope::new(
        archive,
        writer,
        3,
        vec![pending.commit.commit_id],
        vec![
            ArchiveRecord::VersionAdded {
                version,
                operation: OperationId::new(),
            },
            ArchiveRecord::HeadsChanged {
                sidecar: current.sidecar,
                expected: vec![previous],
                heads: vec![new],
            },
            ArchiveRecord::ProjectionPlanned {
                plan: current.clone(),
            },
        ],
    )
    .unwrap();
    c.apply_commit(&publication).unwrap();
    c.db.execute("UPDATE nas_projections SET applied=1", [])
        .unwrap();
    let mut seen = std::collections::BTreeSet::new();
    let mut batches = 0;
    while c.generated_retention_candidate().unwrap().is_some() {
        batches += 1;
        assert!(
            batches <= 4,
            "completed overlap must not starve pending versions"
        );
        let batch = c
            .prepare_generated_retention(archive, writer, current.sidecar, new)
            .unwrap();
        for v in batch.authorization.versions() {
            assert!(
                seen.insert(v.id),
                "version repeated after acknowledged cleanup"
            );
        }
        c.apply_commit(&batch.commit).unwrap();
        c.acknowledge_generated_retention(&crate::catalog::GeneratedRetentionReceipt {
            commit: batch.commit,
            removed_digests: batch.authorization.removable().to_vec(),
        })
        .unwrap();
    }
    assert_eq!(seen.len(), ids.len());
}

#[test]
fn deferred_retention_and_incomplete_replay_are_not_candidates() {
    use crate::domain::*;
    let (_dir, mut c, archive, writer, current, _ids) = fixture(3);
    c.db.execute(
        "INSERT INTO publication_failures VALUES(?,?,1,'retry')",
        rusqlite::params![
            format!("retention:{}:{}", current.sidecar, current.version),
            SystemClock::default().now() + 3600
        ],
    )
    .unwrap();
    assert!(c.generated_retention_candidate().unwrap().is_none());
    c.db.execute("DELETE FROM publication_failures", [])
        .unwrap();
    c.block_record("missing", "incomplete replay").unwrap();
    assert!(c.generated_retention_candidate().unwrap().is_none());
    assert!(
        c.prepare_generated_retention(archive, writer, current.sidecar, current.version)
            .is_err()
    );
}

#[test]
fn fresh_replay_accepts_retirement_without_local_projection_acknowledgements() {
    let (_dir, mut c, archive, writer, current, _ids) = fixture(3);
    let batch = c
        .prepare_generated_retention(archive, writer, current.sidecar, current.version)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = RootHandle::open(dir.path(), crate::fs::RootPolicy::Local).unwrap();
    let mut fresh = Catalog::open(&root).unwrap();
    fresh
        .apply_commit(batch.authorization.publication())
        .unwrap();
    fresh.apply_commit(&batch.commit).unwrap();
    assert!(matches!(
        fresh.publication_readiness().unwrap(),
        PublicationReadiness::Ready
    ));
    assert_eq!(
        fresh
            .db
            .query_row("SELECT count(*) FROM generated_retired_versions", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn another_generated_current_head_protects_the_same_history_payload() {
    use crate::{archive::*, domain::*};
    let (_dir, mut c, archive, writer, current, ids) = fixture(3);
    let sidecar = SidecarId::new();
    let group = GroupId::new();
    let version = Version {
        id: VersionId::new(),
        sidecar,
        parents: vec![],
        body: c.version(ids[0]).unwrap().body,
        origin: VersionOrigin::Observed,
    };
    let publication = CommitEnvelope::new(
        archive,
        writer,
        2,
        vec![],
        vec![
            ArchiveRecord::GroupCreated { id: group },
            ArchiveRecord::SidecarCreated {
                id: sidecar,
                group: Some(group),
                path: ArchivePath::parse("second.tif").unwrap(),
                owner: GENERATED_OUTPUT_OWNER.into(),
            },
            ArchiveRecord::VersionAdded {
                version: version.clone(),
                operation: OperationId::new(),
            },
            ArchiveRecord::HeadsChanged {
                sidecar,
                expected: vec![],
                heads: vec![version.id],
            },
        ],
    )
    .unwrap();
    c.apply_commit(&publication).unwrap();
    let batch = c
        .prepare_generated_retention(archive, writer, current.sidecar, current.version)
        .unwrap();
    assert!(!batch.authorization.removable().contains(&Digest::of(b"0")));
    assert!(batch.authorization.removable().contains(&Digest::of(b"1")));
    let query:String=c.db.query_row("EXPLAIN QUERY PLAN SELECT id FROM versions WHERE json_extract(body,'$.Content.digest')=?",[Digest::of(b"0").as_str()],|r|r.get(3)).unwrap();
    assert!(query.contains("versions_by_content_digest"), "{query}");
}

#[test]
fn projection_proofs_are_bounded_and_cover_every_retirement_batch() {
    use crate::archive::*;
    let (_dir, mut c, archive, writer, current, ids) = fixture(260);
    let mut previous = current.version;
    let mut records = Vec::new();
    for id in &ids {
        let plan = ProjectionPlan {
            version: *id,
            body: c.version(*id).unwrap().body,
            ..current.clone()
        };
        records.push(ArchiveRecord::HeadsChanged {
            sidecar: current.sidecar,
            expected: vec![previous],
            heads: vec![*id],
        });
        records.push(ArchiveRecord::ProjectionPlanned { plan });
        previous = *id;
    }
    c.apply_commit(&CommitEnvelope::new(archive, writer, 2, vec![], records).unwrap())
        .unwrap();
    c.db.execute("UPDATE nas_projections SET applied=1", [])
        .unwrap();
    let mut covered = std::collections::BTreeSet::new();
    while c.generated_retention_candidate().unwrap().is_some() {
        let batch = c
            .prepare_generated_retention(archive, writer, current.sidecar, current.version)
            .unwrap();
        let proofs = batch.authorization.projections();
        assert!(
            proofs.len() <= 129,
            "{} projection proofs exceed one bounded batch",
            proofs.len()
        );
        assert!(proofs.contains(&current));
        for version in batch.authorization.versions() {
            let expected = ProjectionPlan {
                version: version.id,
                body: version.body.clone(),
                ..current.clone()
            };
            assert!(
                proofs.contains(&expected),
                "retired projection proof missing"
            );
            covered.insert(version.id);
        }
        assert_eq!(proofs.len(), batch.authorization.versions().len() + 1);
        c.apply_commit(&batch.commit).unwrap();
        c.acknowledge_generated_retention(&crate::catalog::GeneratedRetentionReceipt {
            commit: batch.commit,
            removed_digests: batch.authorization.removable().to_vec(),
        })
        .unwrap();
    }
    assert_eq!(covered.len(), 259);
}

#[test]
fn idle_retention_candidate_ignores_large_ordinary_history_with_bounded_sql_work() {
    use crate::domain::*;
    let (_dir, mut catalog, _archive, _writer, current, _) = fixture(1);
    let body = encode(&current.body).unwrap();
    let origin = encode(&VersionOrigin::Observed).unwrap();
    let tx = catalog.db.transaction().unwrap();
    for n in 0..10_000 {
        let sidecar = SidecarId::new().to_string();
        let version = VersionId::new().to_string();
        tx.execute(
            "INSERT INTO sidecars(id,path,owner) VALUES(?,?,'external-xmp')",
            rusqlite::params![sidecar, format!("ordinary/{n}.xmp")],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO versions(id,sidecar,body,origin,operation) VALUES(?,?,?,?,?)",
            rusqlite::params![
                version,
                sidecar,
                body,
                origin,
                OperationId::new().to_string()
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO archived_versions(version) VALUES(?)",
            [&version],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO heads(sidecar,version) VALUES(?,?)",
            rusqlite::params![sidecar, version],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO nas_projections(sidecar,version,applied) VALUES(?,?,1)",
            rusqlite::params![sidecar, version],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    unsafe extern "C" fn budget(pointer: *mut std::ffi::c_void) -> i32 {
        let calls = unsafe { &mut *pointer.cast::<u32>() };
        *calls += 1;
        i32::from(*calls >= 20)
    }
    let mut calls = 0u32;
    let handle = unsafe { catalog.db.handle() };
    unsafe {
        rusqlite::ffi::sqlite3_progress_handler(
            handle,
            1000,
            Some(budget),
            std::ptr::from_mut(&mut calls).cast(),
        );
    }
    let result = catalog.generated_retention_candidate();
    unsafe {
        rusqlite::ffi::sqlite3_progress_handler(handle, 0, None, std::ptr::null_mut());
    }
    assert!(
        matches!(result, Ok(None)),
        "idle retention candidate exceeded its 20,000-step SQL budget: {result:?}"
    );
}
