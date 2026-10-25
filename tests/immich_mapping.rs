mod support;
use photo_sorter::{
    catalog::Catalog,
    domain::*,
    fs::{RootHandle, RootPolicy},
    immich::*,
};
use std::os::unix::fs::PermissionsExt;
use url::Url;
fn key(r: &support::TestRig) -> std::path::PathBuf {
    let path = r.state.join("key");
    std::fs::write(&path, b"isolated-fixture-key").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}
#[test]
fn configured_origin_only() {
    let server = Url::parse("https://configured.example").unwrap();
    let id = uuid::Uuid::new_v4();
    assert_eq!(
        parse_photo_url(&format!("https://configured.example/photos/{id}"), &server).unwrap(),
        id
    );
    assert!(parse_photo_url(&format!("https://other.example/photos/{id}"), &server).is_err());
    assert!(
        parse_photo_url(
            &format!("https://configured.example/photos/%2e%2e/{id}"),
            &server
        )
        .is_err()
    );
}
#[test]
fn userinfo_or_redirect_no_secret_leak() {
    let r = support::TestRig::new();
    let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    let (origin, rx, handle) = support::http::serve(
        302,
        String::new(),
        vec![(
            "Location".into(),
            format!("http://{}/api/server/about", target.local_addr().unwrap()),
        )],
    );
    let client = ImmichClient::new(origin.clone(), &key(&r)).unwrap();
    assert!(client.server_info().is_err());
    let request = rx.recv().unwrap();
    assert!(
        request
            .to_lowercase()
            .contains("x-api-key: isolated-fixture-key")
    );
    assert!(target.accept().is_err());
    handle.join().unwrap();
    assert!(
        parse_photo_url(
            "http://user:password@configured.example/photos/00000000-0000-0000-0000-000000000001",
            &Url::parse("http://configured.example").unwrap()
        )
        .is_err()
    );
}
fn asset(id: uuid::Uuid, path: &str) -> AssetInfo {
    AssetInfo {
        id,
        kind: "IMAGE".into(),
        original_path: path.into(),
        original_file_name: path.rsplit('/').next().unwrap().into(),
        original_mime_type: Some("image/jpeg".into()),
        library_id: None,
        checksum: "fixture-checksum".into(),
        exif_info: None,
    }
}
#[test]
fn path_prefix_boundary_and_traversal() {
    let mapping = PrefixMapping {
        source: "/external/photos".into(),
        target: Some(ArchivePath::parse("JPEG").unwrap()),
        library: None,
    };
    assert_eq!(
        mapping
            .translate("/external/photos/a.JPG")
            .unwrap()
            .as_str(),
        "JPEG/a.JPG"
    );
    for bad in ["/external/photos-other/a.JPG", "/external/photos/../a.JPG"] {
        assert!(mapping.translate(bad).is_err());
    }
}
#[test]
fn tiff_is_not_eligible_jpeg() {
    let r = support::TestRig::new();
    let c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    assert!(resolve_asset(&asset(uuid::Uuid::new_v4(), "/external/a.TIFF"), &[], &c).is_err());
}
#[test]
fn known_url_works_offline() {
    let r = support::TestRig::new();
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
        photo_sorter::catalog::PairEvidence::UserSelected {
            reason: "fixture".into(),
        },
    )
    .unwrap();
    let origin = Url::parse("http://fixture.invalid").unwrap();
    let a = asset(uuid::Uuid::new_v4(), "/external/a.JPG");
    let mapping = PrefixMapping {
        source: "/external".into(),
        target: Some(ArchivePath::parse("JPEG").unwrap()),
        library: None,
    };
    let resolved = resolve_asset(&a, &[mapping], &c).unwrap();
    c.bind_asset(&origin, &a, resolved).unwrap();
    let mut pins = PinCoordinator {
        catalog: &mut c,
        client: None,
        origin: origin.clone(),
        mappings: &[],
    };
    let result = pins
        .request(
            &[origin.join(&format!("/photos/{}", a.id)).unwrap()],
            OperationId::new(),
        )
        .unwrap();
    assert_eq!(result[0].group, Some(group));
    assert!(pins.catalog.protection(group).unwrap().pinned);
}
#[test]
fn unknown_url_waits_for_immich() {
    let r = support::TestRig::new();
    let mut c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    let origin = Url::parse("http://fixture.invalid").unwrap();
    let mut pins = PinCoordinator {
        catalog: &mut c,
        client: None,
        origin: origin.clone(),
        mappings: &[],
    };
    let result = pins
        .request(
            &[origin
                .join(&format!("/photos/{}", uuid::Uuid::new_v4()))
                .unwrap()],
            OperationId::new(),
        )
        .unwrap();
    assert!(result[0].job.is_some());
    assert!(result[0].group.is_none());
}
#[test]
fn real_http_asset_and_status_errors() {
    for status in [200, 401, 403, 404, 500] {
        let r = support::TestRig::new();
        let id = uuid::Uuid::new_v4();
        let mut body: serde_json::Value =
            serde_json::from_str(include_str!("fixtures/immich/asset.json")).unwrap();
        body["id"] = serde_json::json!(id);
        let body = serde_json::to_string(&body).unwrap();
        let (origin, rx, handle) = support::http::serve(status, body, vec![]);
        let client = ImmichClient::new(origin, &key(&r)).unwrap();
        let result = client.asset(id);
        if status == 200 {
            assert_eq!(result.unwrap().id, id);
        } else {
            let error = result.unwrap_err();
            assert!(!error.message.contains("isolated-fixture-key"));
        }
        assert!(
            rx.recv()
                .unwrap()
                .starts_with(&format!("GET /api/assets/{id} "))
        );
        handle.join().unwrap();
    }
}
#[test]
fn pending_url_retry_resolves_into_same_job() {
    let r = support::TestRig::new();
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
        photo_sorter::catalog::PairEvidence::UserSelected {
            reason: "fixture".into(),
        },
    )
    .unwrap();
    let origin = Url::parse("http://fixture.invalid").unwrap();
    let asset = asset(uuid::Uuid::new_v4(), "/external/a.JPG");
    let url = origin.join(&format!("/photos/{}", asset.id)).unwrap();
    let operation = OperationId::new();
    let first = PinCoordinator {
        catalog: &mut c,
        client: None,
        origin: origin.clone(),
        mappings: &[],
    }
    .request(std::slice::from_ref(&url), operation)
    .unwrap()
    .remove(0);
    c.bind_asset(&origin, &asset, group).unwrap();
    let second = PinCoordinator {
        catalog: &mut c,
        client: None,
        origin,
        mappings: &[],
    }
    .request(&[url], operation)
    .unwrap()
    .remove(0);
    assert_eq!(first.job, second.job);
    assert_eq!(second.group, Some(group));
    assert!(c.protection(group).unwrap().pinned);
}
#[test]
fn ambiguous_stem_not_paired() {
    let r = support::TestRig::new();
    let mut c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    for path in ["JPEG/one/a.JPG", "JPEG/two/a.JPG"] {
        let group = c.create_group(false).unwrap();
        c.attach_member(
            group,
            OriginalMember {
                path: ArchivePath::parse(path).unwrap(),
                digest: Digest::of(path.as_bytes()),
                size: path.len() as u64,
                role: MediaRole::Jpeg,
            },
            photo_sorter::catalog::PairEvidence::UserSelected {
                reason: "fixture".into(),
            },
        )
        .unwrap();
    }
    let mapping = PrefixMapping {
        source: "/external".into(),
        target: Some(ArchivePath::parse("JPEG").unwrap()),
        library: None,
    };
    assert!(
        resolve_asset(
            &asset(uuid::Uuid::new_v4(), "/external/a.JPG"),
            &[mapping],
            &c
        )
        .is_err()
    );
}
#[test]
fn reindexed_asset_cannot_rebind_unrelated_photo() {
    let r = support::TestRig::new();
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
        photo_sorter::catalog::PairEvidence::UserSelected {
            reason: "fixture".into(),
        },
    )
    .unwrap();
    let origin = Url::parse("http://fixture.invalid").unwrap();
    let mut a = asset(uuid::Uuid::new_v4(), "/external/a.JPG");
    c.bind_asset(&origin, &a, group).unwrap();
    a.checksum = "different-content".into();
    assert!(c.bind_asset(&origin, &a, group).is_err());
}
#[test]
fn multi_url_partial_results() {
    let r = support::TestRig::new();
    let mut c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    let origin = Url::parse("http://fixture.invalid").unwrap();
    let urls = [
        origin
            .join(&format!("/photos/{}", uuid::Uuid::new_v4()))
            .unwrap(),
        Url::parse("https://wrong.invalid/photos/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap(),
    ];
    let result = PinCoordinator {
        catalog: &mut c,
        client: None,
        origin,
        mappings: &[],
    }
    .request(&urls, OperationId::new())
    .unwrap();
    assert_eq!(result.len(), 2);
    assert!(result[0].job.is_some());
    assert!(result[1].job.is_none());
}
#[test]
fn re_fetch_keeps_local_sidecars() {
    use photo_sorter::{archive::*, config::Config, transfer::*};
    let r = support::TestRig::new();
    let archive = ArchiveId::new();
    let writer = WriterId::new();
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    ArchiveStore::initialize(
        &root,
        InitArchive {
            archive_id: archive,
            writer_id: writer,
        },
    )
    .unwrap();
    let store = ArchiveStore::open(root, archive, writer).unwrap();
    let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
    root.write_new(
        &ArchivePath::parse(".photo-sorter/staging/jpeg").unwrap(),
        b"jpeg",
    )
    .unwrap();
    let group = GroupId::new();
    let member = OriginalMember {
        path: ArchivePath::parse("JPEG/a.JPG").unwrap(),
        digest: Digest::of(b"jpeg"),
        size: 4,
        role: MediaRole::Jpeg,
    };
    let sidecar = SidecarId::new();
    let version = VersionId::new();
    let incoming = br#"{"version":1,"rating":3,"adjustments":null}"#;
    let incoming_digest = Digest::of(incoming);
    let history_stage = ArchivePath::parse(".photo-sorter/staging/metadata").unwrap();
    root.write_new(&history_stage, incoming).unwrap();
    store
        .store_history(&history_stage, &incoming_digest, incoming.len() as u64)
        .unwrap();
    let commit = CommitEnvelope::new(
        archive,
        writer,
        1,
        vec![],
        vec![
            ArchiveRecord::GroupCreated { id: group },
            ArchiveRecord::SidecarCreated {
                id: sidecar,
                group: Some(group),
                path: ArchivePath::parse("JPEG/a.JPG.rrdata").unwrap(),
                owner: "rapidraw-base".into(),
            },
            ArchiveRecord::VersionAdded {
                version: Version {
                    id: version,
                    sidecar,
                    parents: vec![],
                    body: VersionBody::Content {
                        digest: incoming_digest.clone(),
                        size: incoming.len() as u64,
                    },
                    origin: VersionOrigin::Observed,
                },
                operation: OperationId::new(),
            },
            ArchiveRecord::HeadsChanged {
                sidecar,
                expected: vec![],
                heads: vec![version],
            },
            ArchiveRecord::MemberAttached {
                group,
                member: member.clone(),
                evidence: photo_sorter::catalog::PairEvidence::ObservedPlacement {
                    path: member.path.clone(),
                    digest: member.digest.clone(),
                },
            },
        ],
    )
    .unwrap();
    let mut guard = store.acquire_publication().unwrap();
    let batch = store.journal_batch(&mut guard, 128).unwrap();
    store.ack_journal(&mut guard, batch.token).unwrap();
    let receipt = store
        .publish_originals(
            &mut guard,
            PublicationPlan {
                commit,
                staged: vec![StagedMember {
                    member: member.clone(),
                    staged: ArchivePath::parse(".photo-sorter/staging/jpeg").unwrap(),
                }],
                attempt: OperationId::new(),
            },
            &NoFaults,
        )
        .unwrap();
    store.release(guard).unwrap();
    let mut c = Catalog::open(&RootHandle::open(&r.state, RootPolicy::Local).unwrap()).unwrap();
    c.apply_commit(&receipt.commit).unwrap();
    std::fs::create_dir(r.local.join("JPEG")).unwrap();
    std::fs::write(r.local.join("JPEG/a.JPG.rrdata"), br#"{"rating":5}"#).unwrap();
    let job = c.request_pin(group, OperationId::new()).unwrap();
    let mut config = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
    config.archive_id = Some(archive);
    config.writer_id = writer;
    config.reserve = 0;
    let mut worker =
        WorkerSupervisor::new(env!("CARGO_BIN_EXE_photo_sorter").into(), vec![]).unwrap();
    let mut driver = PinFetchDriver::default();
    for _ in 0..1000 {
        if driver
            .advance(&mut c, &config, &mut worker, job)
            .unwrap()
            .originals_ready
            && r.state
                .join(format!(
                    ".photo-sorter/snapshots/{}",
                    &incoming_digest.as_str()[7..]
                ))
                .exists()
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(std::fs::read(r.local.join("JPEG/a.JPG")).unwrap(), b"jpeg");
    assert_eq!(
        std::fs::read(r.local.join("JPEG/a.JPG.rrdata")).unwrap(),
        br#"{"rating":5}"#
    );
    assert_eq!(
        std::fs::read(r.state.join(format!(
            ".photo-sorter/snapshots/{}",
            &incoming_digest.as_str()[7..]
        )))
        .unwrap(),
        incoming
    );
    assert!(c.protection(group).unwrap().pinned);
}
#[test]
fn bad_schema_and_secret_permissions_are_explicit() {
    let r = support::TestRig::new();
    let (origin, rx, handle) = support::http::serve(200, "{}".into(), vec![]);
    let client = ImmichClient::new(origin, &key(&r)).unwrap();
    assert_eq!(
        client.asset(uuid::Uuid::new_v4()).unwrap_err().code,
        photo_sorter::error::ErrorCode::Unsupported
    );
    rx.recv().unwrap();
    handle.join().unwrap();
    let path = key(&r);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(ImmichClient::new(Url::parse("http://fixture.invalid").unwrap(), &path).is_err());
}
#[test]
fn stalled_http_times_out_without_credentials_in_error() {
    let r = support::TestRig::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let thread = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));
    });
    let client =
        ImmichClient::new_with_timeout(origin, &key(&r), std::time::Duration::from_millis(30))
            .unwrap();
    let error = client.server_info().unwrap_err();
    assert_eq!(error.code, photo_sorter::error::ErrorCode::Unavailable);
    assert!(!error.message.contains("isolated-fixture-key"));
    thread.join().unwrap();
}
