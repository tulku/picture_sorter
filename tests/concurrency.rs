mod support;
use photo_sorter::{archive::*, domain::*, fs::*};
use std::time::{Duration, Instant};
use support::service::ServiceFixture;
fn raw(status: &serde_json::Value) -> (String, String) {
    for group in status["groups"].as_array().unwrap() {
        for member in group["group"]["members"].as_array().unwrap() {
            if member["path"].as_str().unwrap().ends_with("A.ORF") {
                return (
                    group["group"]["id"].as_str().unwrap().to_owned(),
                    member["path"].as_str().unwrap().to_owned(),
                );
            }
        }
    }
    panic!("RAW group absent")
}
#[test]
fn two_clients_same_import_and_divergent_saved_edits() {
    concurrent_scenario(false);
}
#[test]
fn two_clients_merge_rating_and_adjustments_without_refetching_raw_or_overwriting_live_edits() {
    concurrent_scenario(true);
}
fn concurrent_scenario(mergeable: bool) {
    let first = ServiceFixture::with_config(|config, r| {
        let root = RootHandle::open(&r.archive, RootPolicy::TestArchive).unwrap();
        for dir in ["commits", "versions", "staging", "locks", "sequences"] {
            root.mkdir_all(&ArchivePath::parse(&format!(".photo-sorter/{dir}")).unwrap())
                .unwrap();
        }
        let marker = ArchiveMarker {
            schema_version: 1,
            archive_id: config.archive_id.unwrap(),
            writer_id: config.writer_id,
            mode: PublicationMode::LocalTest,
            mode_revision: 1,
        };
        root.write_new(
            &ArchivePath::parse(".photo-sorter/archive.json").unwrap(),
            &serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
    });
    let archive: ArchiveId = serde_json::from_value(first.status()["archive_id"].clone()).unwrap();
    let second = ServiceFixture::with_config(|config, _| {
        config.archive_root = first.r.archive.clone();
        config.archive_id = Some(archive);
    });
    for fixture in [&first, &second] {
        std::fs::write(fixture.r.source.join("A.ORF"), b"same raw").unwrap();
        std::fs::write(fixture.r.source.join("A.JPG"), b"same jpeg").unwrap();
    }
    std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            first.cli(&[
                "import",
                first.r.source.to_str().unwrap(),
                "--wait-for",
                "archived",
            ])
        });
        let b = scope.spawn(|| {
            second.cli(&[
                "import",
                second.r.source.to_str().unwrap(),
                "--wait-for",
                "archived",
            ])
        });
        for out in [a.join().unwrap(), b.join().unwrap()] {
            assert!(
                matches!(out.status.code(), Some(0 | 3)),
                "{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    });
    let deadline = Instant::now();
    while ![first.status(), second.status()].iter().all(|s| {
        s["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["request"]["kind"]["type"] == "import" && j["state"] == "Complete")
    }) {
        assert!(
            deadline.elapsed() < Duration::from_secs(15),
            "independent import did not resume after ordinary lock contention"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let (group, path) = raw(&first.status());
    let (_, peer_path) = raw(&second.status());
    assert_eq!(path, peer_path);
    assert_eq!(
        std::fs::read(first.r.archive.join(&path)).unwrap(),
        b"same raw"
    );
    let base = br#"{"version":1,"rating":3,"adjustments":{"exposure":1},"tags":null}"#;
    std::fs::write(first.r.local.join(format!("{path}.rrdata")), base).unwrap();
    let deadline = Instant::now();
    while std::fs::read(first.r.archive.join(format!("{path}.rrdata")))
        .ok()
        .as_deref()
        != Some(base.as_slice())
    {
        assert!(deadline.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
    let (peer_group, _) = raw(&second.status());
    let out = second.cli(&["pin", &peer_group]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let out = second.cli(&["refresh", "--editor-closed"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        std::fs::read(second.r.local.join(format!("{path}.rrdata"))).unwrap(),
        base
    );
    if mergeable {
        std::fs::remove_file(first.r.local.join(&path)).unwrap();
        std::fs::remove_file(second.r.local.join(&path)).unwrap();
    }
    let a = br#"{"version":1,"rating":3,"adjustments":{"exposure":2},"tags":null,"mask":"first"}"#;
    let b = br#"{"version":1,"rating":3,"adjustments":{"exposure":3},"tags":null,"mask":"second"}"#;
    let b: &[u8] = if mergeable {
        br#"{"version":1,"rating":5,"adjustments":{"exposure":1},"tags":null}"#
    } else {
        b
    };
    std::fs::write(first.r.local.join(format!("{path}.rrdata")), a).unwrap();
    std::fs::write(second.r.local.join(format!("{path}.rrdata")), b).unwrap();
    let deadline = Instant::now();
    loop {
        let histories = [a.as_slice(), b].iter().all(|bytes| {
            first
                .r
                .archive
                .join(history_path(&Digest::of(bytes)).unwrap().as_str())
                .exists()
        });
        if histories {
            break;
        }
        assert!(
            deadline.elapsed() < Duration::from_secs(12),
            "both saved histories were not archived"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    if mergeable {
        let deadline = Instant::now();
        loop {
            let bytes =
                std::fs::read(first.r.archive.join(format!("{path}.rrdata"))).unwrap_or_default();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            if value["rating"] == 5
                && value["adjustments"]["exposure"] == 2
                && value["mask"] == "first"
            {
                break;
            }
            assert!(
                deadline.elapsed() < Duration::from_secs(20),
                "metadata-only merge did not publish: {}",
                first.status()
            );
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(
            std::fs::read(first.r.local.join(format!("{path}.rrdata"))).unwrap(),
            a
        );
        assert_eq!(
            std::fs::read(second.r.local.join(format!("{path}.rrdata"))).unwrap(),
            b
        );
        assert!(!first.r.local.join(&path).exists());
        assert!(!second.r.local.join(&path).exists());
        assert!([first.status(), second.status()].iter().any(|status| {
            status["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|job| job["request"]["kind"]["type"] == "metadata_fetch")
        }));
        return;
    }
    let deadline = Instant::now();
    while ![first.status(), second.status()].iter().any(|s| {
        s["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["kind"] == "sidecar")
    }) {
        assert!(deadline.elapsed() < Duration::from_secs(7));
        std::thread::sleep(Duration::from_millis(20));
    }
    first.cli(&["pin", &group, "--no-wait"]);
    second.cli(&["pin", &peer_group, "--no-wait"]);
    let deadline = Instant::now();
    while ![first.status(), second.status()].iter().all(|s| {
        s["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["kind"] == "sidecar")
    }) {
        assert!(deadline.elapsed() < Duration::from_secs(7));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read(first.r.local.join(format!("{path}.rrdata"))).unwrap(),
        a
    );
    assert_eq!(
        std::fs::read(second.r.local.join(format!("{path}.rrdata"))).unwrap(),
        b
    );
    for bytes in [a.as_slice(), b] {
        assert_eq!(
            std::fs::read(
                first
                    .r
                    .archive
                    .join(history_path(&Digest::of(bytes)).unwrap().as_str())
            )
            .unwrap(),
            bytes
        );
    }
}
