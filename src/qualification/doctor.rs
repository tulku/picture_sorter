use crate::{config::Config, error::AppResult};
use std::{
    io::Read,
    os::fd::AsRawFd,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn version(executable: &str, argument: &str) -> serde_json::Value {
    let child = match Command::new(executable)
        .arg(argument)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return serde_json::json!({"available":false}),
    };
    let mut child = crate::process::ChildGuard::new(child);
    let Some(mut output) = child.stdout.take() else {
        return serde_json::json!({"available":false});
    };
    let fd = output.as_raw_fd();
    // The owned child pipe is read nonblocking so an unavailable version command cannot hang doctor.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return serde_json::json!({"available":false});
    }
    let start = Instant::now();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        match output.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return serde_json::json!({"available":false}),
        }
        if bytes.len() > 16384 || start.elapsed() > Duration::from_secs(2) {
            return serde_json::json!({"available":false,"reason":"version command exceeded bound"});
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return serde_json::json!({"available":true,"version":String::from_utf8_lossy(&bytes).lines().next().unwrap_or_default()});
            }
            Ok(Some(_)) | Err(_) => return serde_json::json!({"available":false}),
            Ok(None) if start.elapsed() < Duration::from_secs(2) => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Ok(None) => {
                return serde_json::json!({"available":false,"reason":"version command did not exit within bound"});
            }
        }
    }
}
pub fn doctor(config: &Config) -> AppResult<serde_json::Value> {
    let registry = super::load_registry(config);
    Ok(
        serde_json::json!({"schema_version":1,"protocol_version":1,"profile":config.profile_id,"archive_probe":"unrun: no NAS access or implicit integration writes","local_test_backend":config.local_test_backend,"tools":{"rsync":version("rsync","--version"),"exiftool":version("exiftool","-ver")},"capability_reports":registry.as_ref().map(|r|r.reports().len()).unwrap_or(0),"capability_error":registry.err().map(|e|e.to_string()),"rapidraw":{"configured_version":config.rapidraw_version,"runtime_probe":"unrun unless an isolated runtime report is supplied"}}),
    )
}
