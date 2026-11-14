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
