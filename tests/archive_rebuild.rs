#[path = "support/crash_scenario.rs"]
mod scenario;
mod support;
use photo_sorter::{archive::*, catalog::*, domain::*, fs::*};
#[test]
fn rebuild_matches_committed_graph_aliases_mapping_and_rating_facts() {
    let temp = tempfile::tempdir().unwrap();
    let manifest = scenario::setup(temp.path());
    scenario::run(temp.path(), &NoFaults);
    let state = RootHandle::open(&temp.path().join("state"), RootPolicy::Local).unwrap();
    let mut source = Catalog::open(&state).unwrap();
    let store = ArchiveStore::open(
        RootHandle::open(&temp.path().join("archive"), RootPolicy::TestArchive).unwrap(),
        manifest.archive,
        manifest.writer,
    )
    .unwrap();
    let reset: VersionPublication = serde_json::from_slice(
        &state
            .read(&scenario::path("reset-plan.json"), 1024 * 1024)
            .unwrap(),
    )
    .unwrap();
    let alias = GroupId::new();
    let asset = uuid::Uuid::new_v4();
    let operation = OperationId::new();
    let records = vec![
        ArchiveRecord::GroupCreated { id: alias },
        ArchiveRecord::GroupsMerged {
            groups: vec![manifest.group, alias],
            evidence: PairEvidence::UserSelected {
                reason: "reconstruction fixture association".into(),
            },
        },
        ArchiveRecord::MappingRecorded {
            server: "https://fixture.example".into(),
            asset,
            library: None,
            path: scenario::path("JPEG/a.JPG"),
            digest: Digest::of(b"jpeg original"),
            group: manifest.group,
            remote_checksum: "AAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
            external_path: "/external/a.JPG".into(),
        },
        ArchiveRecord::RatingObserved {
            operation,
            group: manifest.group,
            local: RatingObservation::Stars(Rating::try_from(3).unwrap()),
            remote: RatingObservation::Stars(Rating::try_from(3).unwrap()),
        },
    ];
    let commit = CommitEnvelope::new(
        manifest.archive,
        manifest.writer,
        4,
        vec![reset.commit.commit_id],
        records,
    )
    .unwrap();
    let mut guard = store.acquire_publication().unwrap();
    loop {
        let batch = store.journal_batch(&mut guard, 128).unwrap();
        for entry in &batch.entries {
            source
                .apply_commit(&store.read_commit(&guard, &entry.path).unwrap())
                .unwrap();
        }
        store.ack_journal(&mut guard, batch.token).unwrap();
        if batch.end {
            break;
        }
    }
    let receipt = store
        .publish_originals(
            &mut guard,
            PublicationPlan {
                commit,
                staged: vec![],
                attempt: OperationId::new(),
            },
            &NoFaults,
        )
        .unwrap();
    source.apply_commit(&receipt.commit).unwrap();
    store.release(guard).unwrap();
    let canonical = source.canonical_group(manifest.group).unwrap();
    assert_eq!(source.canonical_group(alias).unwrap(), canonical);
    source.request_pin(canonical, OperationId::new()).unwrap();
    let expected = source.group(canonical).unwrap();
    let heads = source.heads(manifest.sidecar).unwrap();
    let reset_version = source.version(heads[0]).unwrap();
    drop(source);
    std::fs::write(
        temp.path().join("local/private.rrdata"),
        b"unpublished saved bytes",
    )
    .unwrap();
    let replacement = temp.path().join("replacement");
    std::fs::create_dir(&replacement).unwrap();
    let mut journal = store.read_only_journal().unwrap();
    let mut commits = vec![];
    loop {
        let batch = journal.next_batch(128).unwrap();
        for entry in batch.entries {
            commits.push(journal.read(&entry.path).unwrap());
        }
        if batch.end {
            break;
        }
    }
    let report = CatalogRecovery::rebuild(RebuildRequest {
        state_root: temp.path().join("state"),
        working_root: temp.path().join("local"),
        replacement: replacement.clone(),
        profile: ProfileId::new(),
        archive: manifest.archive,
        commits,
    })
    .unwrap();
    assert!(report.private_state_lost);
    assert_ne!(report.writer, manifest.writer);
    let mut rebuilt =
        Catalog::open(&RootHandle::open(&replacement, RootPolicy::Local).unwrap()).unwrap();
    assert_eq!(rebuilt.canonical_group(alias).unwrap(), canonical);
    assert_eq!(rebuilt.group(canonical).unwrap().members, expected.members);
    assert_eq!(rebuilt.heads(manifest.sidecar).unwrap(), heads);
    assert_eq!(rebuilt.version(heads[0]).unwrap(), reset_version);
    assert!(!rebuilt.protection(canonical).unwrap().pinned);
    assert!(!rebuilt.readiness(canonical).unwrap().originals_ready);
    assert_eq!(
        std::fs::read(temp.path().join("local/private.rrdata")).unwrap(),
        b"unpublished saved bytes"
    );
    assert!(replacement.join("preserved/catalog.sqlite").exists());
    scenario::assert_preserved(temp.path());
    let fresh = photo_sorter::immich::PinCoordinator {
        catalog: &mut rebuilt,
        client: None,
        origin: url::Url::parse("https://fixture.example").unwrap(),
        mappings: &[],
    }
    .request(
        &[url::Url::parse(&format!("https://fixture.example/photos/{asset}")).unwrap()],
        OperationId::new(),
    )
    .unwrap();
    assert_eq!(fresh[0].group, Some(canonical));
}
