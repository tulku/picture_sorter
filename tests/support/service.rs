use super::TestRig;
use photo_sorter::{config::Config, domain::*};
use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
pub struct ServiceFixture {
    pub r: TestRig,
    pub profile: std::path::PathBuf,
    pub child: Child,
}
impl ServiceFixture {
    pub fn stop(&mut self) {
        if self.child.try_wait().unwrap().is_some() {
            return;
        }
        let status = self.status();
        let mut groups = Vec::new();
        for name in ["pid", "local_pid", "observation_pid", "maintenance_pid"] {
            if let Some(pid) = status["worker"][name].as_u64() {
                groups.push(pid as i32);
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
        }
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        let deadline = Instant::now();
        loop {
            let live = std::fs::read_dir("/proc")
                .unwrap()
                .filter_map(Result::ok)
                .filter_map(|entry| std::fs::read_to_string(entry.path().join("stat")).ok())
                .any(|stat| {
                    let Some((_, rest)) = stat.rsplit_once(") ") else {
                        return false;
                    };
                    let fields = rest.split_whitespace().collect::<Vec<_>>();
                    fields
                        .first()
                        .is_some_and(|state| *state != "Z" && *state != "X")
                        && fields
                            .get(2)
                            .and_then(|p| p.parse::<i32>().ok())
                            .is_some_and(|group| groups.contains(&group))
                });
            if !live {
                break;
            }
            assert!(
                deadline.elapsed() < Duration::from_secs(5),
                "old fixture writer group is not quiescent"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn restart(&mut self) {
        self.child = Command::new(env!("CARGO_BIN_EXE_photo_sorter"))
            .arg("--config")
            .arg(&self.profile)
            .args(["service", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now();
        loop {
            let out = self.cli(&["service", "status"]);
            if out.status.success() {
                break;
            }
            assert!(self.child.try_wait().unwrap().is_none());
            assert!(deadline.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub fn new() -> Self {
        Self::with_config(|_, _| {})
    }
    pub fn with_config(update: impl FnOnce(&mut Config, &TestRig)) -> Self {
        let r = TestRig::new();
        let profile = r._temp.path().join("profile.toml");
        let mut c = Config::new(r.local.clone(), r.state.clone(), r.archive.clone());
        c.archive_id = Some(ArchiveId::new());
        c.local_test_backend = true;
        update(&mut c, &r);
        std::fs::write(&profile, toml::to_string(&c).unwrap()).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_photo_sorter"))
            .arg("--config")
            .arg(&profile)
            .args(["service", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let start = Instant::now();
        loop {
            if r.state.join("service.sock").exists() {
                break;
            }
            assert!(child.try_wait().unwrap().is_none());
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
        Self { r, profile, child }
    }
    pub fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_photo_sorter"))
            .arg("--config")
            .arg(&self.profile)
            .args(args)
            .arg("--json")
            .output()
            .unwrap()
    }
    pub fn status(&self) -> serde_json::Value {
        let out = self.cli(&["status"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["outcome"]["data"].clone()
    }
}
impl Drop for ServiceFixture {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let out = self.cli(&["status"]);
            let status = serde_json::from_slice::<serde_json::Value>(&out.stdout)
                .unwrap_or_default()["outcome"]["data"]
                .clone();
            for name in ["pid", "local_pid", "observation_pid", "maintenance_pid"] {
                if let Some(pid) = status["worker"][name].as_u64() {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
