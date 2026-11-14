mod support;
use std::{time::Duration, time::Instant};
use support::service::ServiceFixture;

fn voluntary_switches(pid: u32) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .find_map(|line| {
            line.strip_prefix("voluntary_ctxt_switches:")
                .map(|value| value.trim().parse().unwrap())
        })
        .unwrap()
}

#[test]
fn idle_service_blocks_and_socket_request_wakes_it() {
    let service = ServiceFixture::new();
    std::thread::sleep(Duration::from_secs(3));
    let before = voluntary_switches(service.child.id());
    std::thread::sleep(Duration::from_millis(800));
    let wakeups = voluntary_switches(service.child.id()) - before;
    assert!(wakeups < 20, "idle service woke {wakeups} times in 800ms");
    let start = Instant::now();
    let _ = service.status();
    assert!(start.elapsed() < Duration::from_millis(500));
}

#[test]
#[ignore = "manual 30-second idle resource measurement"]
fn measure_idle_with_twenty_thousand_historical_jobs() {
    use photo_sorter::{
        catalog::Catalog,
        fs::{RootHandle, RootPolicy},
    };
    let service = ServiceFixture::with_config(|_, rig| {
        let state = RootHandle::open(&rig.state, RootPolicy::Local).unwrap();
        let catalog = Catalog::open(&state).unwrap();
        drop(catalog);
        let mut db = rusqlite::Connection::open(rig.state.join("catalog.sqlite")).unwrap();
        let tx = db.transaction().unwrap();
        let request = serde_json::to_string(&photo_sorter::catalog::DurableRequest {
            operation_id: photo_sorter::domain::OperationId::new(),
            kind: photo_sorter::catalog::JobKind::Clean,
        })
        .unwrap();
        for _ in 0..20_000 {
            let id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO jobs(id,operation,request,state) VALUES(?1,?1,?2,'\"Complete\"')",
                rusqlite::params![id, request],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    });
    std::thread::sleep(Duration::from_secs(4));
    let status = service.status();
    let mut pids = vec![service.child.id()];
    for key in ["pid", "local_pid", "observation_pid", "maintenance_pid"] {
        pids.push(status["worker"][key].as_u64().unwrap() as u32);
    }
    let sample = || {
        let mut ticks = 0u64;
        let mut rss = 0u64;
        for pid in &pids {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
            let fields: Vec<_> = stat
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .collect();
            ticks += fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
            rss += fields[21].parse::<u64>().unwrap();
        }
        (ticks, rss)
    };
    let before = sample();
    let start = Instant::now();
    std::thread::sleep(Duration::from_secs(30));
    let after = sample();
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as f64;
    let cpu = 100.0 * (after.0 - before.0) as f64 / hz / start.elapsed().as_secs_f64();
    println!(
        "idle measurement: CPU={cpu:.3}% of one core; aggregate RSS={:.2}MiB; 20000 historical jobs; {:.1}s",
        after.1 as f64 * page / 1048576.0,
        start.elapsed().as_secs_f64()
    );
    assert!(cpu < 1.0, "idle CPU exceeded 1%: {cpu:.3}%");
}

#[test]
fn zero_debounce_does_not_spin_while_idle() {
    let service = ServiceFixture::with_config(|config, _| config.debounce_seconds = 0);
    std::thread::sleep(Duration::from_secs(3));
    let ticks = || {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", service.child.id())).unwrap();
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap()
    };
    let before = ticks();
    std::thread::sleep(Duration::from_millis(800));
    let used = ticks() - before;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    assert!(
        (used as f64 / hz) < 0.08,
        "idle CPU used {used} ticks in 800ms"
    );
}

#[test]
fn repeated_reconciliation_keeps_mismatches_without_rereading_or_sidecar_backlog() {
    use photo_sorter::{
        catalog::Catalog,
        domain::{Digest, SidecarId},
        fs::{RootHandle, RootPolicy},
    };
    let service = ServiceFixture::with_config(|config, rig| {
        config.reconcile_seconds = 2;
        let state = RootHandle::open(&rig.state, RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&state).unwrap();
        let group = catalog.create_group(false).unwrap();
        let expected = vec![0u8; 1024 * 1024];
        let modified = vec![1u8; expected.len()];
        let digest = Digest::of(&expected);
        drop(catalog);
        let mut db = rusqlite::Connection::open(rig.state.join("catalog.sqlite")).unwrap();
        let tx = db.transaction().unwrap();
        tx.execute(
            "INSERT INTO blobs(digest,size) VALUES(?,?)",
            rusqlite::params![digest.as_str(), expected.len()],
        )
        .unwrap();
        for index in 0..32 {
            let path = format!("photo-{index:03}.JPG");
            std::fs::write(rig.local.join(&path), &modified).unwrap();
            tx.execute(
                "INSERT INTO placements(id,group_id,path,digest,local_ready) VALUES(?,?,?,?,1)",
                rusqlite::params![index.to_string(), group.to_string(), path, digest.as_str()],
            )
            .unwrap();
        }
        for index in 0..12_000 {
            tx.execute(
                "INSERT INTO sidecars(id,path,owner) VALUES(?,?,'rapidraw-base')",
                rusqlite::params![
                    SidecarId::new().to_string(),
                    format!("remote-{index}.ORF.rrdata")
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    });
    let db = rusqlite::Connection::open(service.r.state.join("catalog.sqlite")).unwrap();
    let count = |sql| db.query_row(sql, [], |row| row.get::<_, u64>(0)).unwrap();
    let start = Instant::now();
    let status = loop {
        let status = service.status();
        if status["watcher"]["startup_complete"] == true
            && count("SELECT count(*) FROM physical_findings WHERE status='Corrupt'") == 32
            && count("SELECT count(*) FROM startup_checks") == 0
        {
            break status;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "reconciliation did not finish: {status}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let hash_bytes = status["session_metrics"]["streamed_hash_bytes"]
        .as_u64()
        .unwrap();
    assert!(hash_bytes >= 32 * 1024 * 1024);
    let mut pids = vec![service.child.id()];
    for key in ["pid", "local_pid", "observation_pid", "maintenance_pid"] {
        pids.push(status["worker"][key].as_u64().unwrap() as u32);
    }
    let resources = || {
        pids.iter()
            .map(|pid| {
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
                let fields: Vec<_> = stat
                    .rsplit_once(')')
                    .unwrap()
                    .1
                    .split_whitespace()
                    .collect();
                (
                    fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap(),
                    fields[21].parse::<u64>().unwrap(),
                )
            })
            .fold((0, 0), |sum, process| {
                (sum.0 + process.0, sum.1 + process.1)
            })
    };
    let before = resources();
    let measured = Instant::now();
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(2200));
        let status = service.status();
        assert_eq!(status["session_metrics"]["streamed_hash_bytes"], hash_bytes);
        assert_eq!(
            count("SELECT count(*) FROM physical_findings WHERE status='Corrupt'"),
            32
        );
        assert_eq!(
            count("SELECT count(*) FROM placements WHERE local_ready<>0"),
            0
        );
        assert_eq!(count("SELECT count(*) FROM capture_requests"), 0);
        assert_eq!(count("SELECT count(*) FROM residency WHERE protected=0"), 0);
    }
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as f64;
    let after = resources();
    let cpu = 100.0 * (after.0 - before.0) as f64 / hz / measured.elapsed().as_secs_f64();
    println!(
        "reconciliation measurement: 32 mismatching MiB, 12000 remote-only sidecars, {:.1}s, CPU={cpu:.3}% of one core, aggregate RSS={:.2}MiB, repeated hash bytes=0",
        measured.elapsed().as_secs_f64(),
        after.1 as f64 * page / 1048576.0
    );
}
