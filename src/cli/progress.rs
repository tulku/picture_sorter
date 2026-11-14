use crate::{
    catalog::{JobKind, JobRecord, JobState},
    config::Config,
    domain::{JobId, OperationId},
    error::{AppError, AppResult},
    service::{Client, protocol::*},
};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde_json::Value;
use std::{
    io::IsTerminal,
    sync::mpsc,
    time::{Duration, Instant},
};

const HEARTBEAT: Duration = Duration::from_secs(1);
const POLL_TIMEOUT: Duration = Duration::from_millis(250);

pub(super) fn send(config: &Config, mut request: Request) -> AppResult<Response> {
    let mut display = Display::new();
    if let Command::Import { source, .. } = &request.command {
        display.message(&format!(
            "Checking card {}; contacting service",
            source.display()
        ));
    }
    let job = match request.command {
        Command::Import { .. } => {
            let mut acceptance = request.clone();
            acceptance.no_wait = true;
            let response = Client::connect(config)?.send(acceptance)?;
            match response.outcome {
                ResponseOutcome::Accepted { job } => job,
                _ => return Ok(response),
            }
        }
        Command::JobWait { job } => job,
        _ => return Client::connect(config)?.send(request),
    };
    request.command = Command::JobWait { job };
    let client = Client::connect(config)?;
    let (tx, rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _ = tx.send(client.send(request));
    });
    let mut last = String::new();
    if let Some((job, detail)) = update(config, job, &mut last) {
        display.update(&job, &detail);
    }
    loop {
        match rx.recv_timeout(HEARTBEAT) {
            Ok(result) => {
                waiter
                    .join()
                    .map_err(|_| AppError::invalid("service wait thread failed"))?;
                return result;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some((job, detail)) = update(config, job, &mut last) {
                    display.update(&job, &detail);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = waiter.join();
                return Err(AppError::invalid("service wait thread disconnected"));
            }
        }
    }
}

struct Display {
    terminal: Option<(MultiProgress, [ProgressBar; 4])>,
    last: String,
    phase: String,
    printed: Instant,
}
impl Display {
    fn new() -> Self {
        let terminal = std::io::stderr().is_terminal().then(|| {
            let multi = MultiProgress::new();
            let bars = std::array::from_fn(|_| multi.add(ProgressBar::new_spinner()));
            for bar in &bars {
                bar.enable_steady_tick(Duration::from_millis(100));
            }
            (multi, bars)
        });
        Self {
            terminal,
            last: String::new(),
            phase: String::new(),
            printed: Instant::now(),
        }
    }
    fn message(&mut self, message: &str) {
        if let Some((_, bars)) = &self.terminal {
            bars[3].set_style(ProgressStyle::with_template("{spinner} {msg}").unwrap());
            bars[3].set_message(message.to_owned());
        } else {
            eprintln!("{message}");
        }
    }
    fn update(&mut self, job: &JobRecord, detail: &Value) {
        let phase = phase(job, detail);
        let line = describe(job, detail);
        if let Some((_, bars)) = &self.terminal {
            if !matches!(job.request.kind, JobKind::Import { .. }) {
                for bar in &bars[..3] {
                    bar.finish_and_clear();
                }
                self.message(&line);
                return;
            }
            let count = |key: &str| detail[key].as_u64().unwrap_or(0);
            let done = detail["scan_complete"].as_bool() == Some(true);
            let total = import_total(detail);
            let uploaded = uploaded(detail);
            let copied = detail["copy_ready"]
                .as_u64()
                .unwrap_or_else(|| count("local_ready"))
                .min(total);
            for (bar, label, position, total, known) in [
                (
                    &bars[0],
                    "Checking card",
                    count("selected") + count("skipped"),
                    count("selected") + count("skipped"),
                    done,
                ),
                (&bars[1], "Local copy", copied, total, done),
                (&bars[2], "Upload", uploaded, total, done),
            ] {
                bar.set_prefix(label);
                if known {
                    bar.set_style(
                        ProgressStyle::with_template(
                            "{prefix:>13} [{bar:24.cyan/blue}] {pos}/{len} {msg}",
                        )
                        .unwrap()
                        .progress_chars("=> "),
                    );
                    bar.set_length(total);
                } else {
                    bar.set_style(
                        ProgressStyle::with_template("{prefix:>13} {spinner} {pos} so far {msg}")
                            .unwrap(),
                    );
                }
                bar.set_position(position);
            }
            bars[0].set_message(format!("{} already known", count("recognized")));
            bars[2].set_message(format!("{} queued", count("pending_archive")));
            self.message(&format!(
                "{phase}{}",
                job.reason
                    .as_ref()
                    .map(|reason| format!(": {}", compact(reason)))
                    .unwrap_or_default()
            ));
        } else if line != self.last
            && (phase != self.phase
                || self.printed.elapsed() >= Duration::from_secs(5)
                || self.last.is_empty())
        {
            eprintln!("{line}");
            self.last = line;
            self.phase = phase.to_owned();
            self.printed = Instant::now();
        }
    }
}
impl Drop for Display {
    fn drop(&mut self) {
        if let Some((multi, bars)) = &self.terminal {
            for bar in bars {
                bar.finish_and_clear();
            }
            let _ = multi.clear();
        }
    }
}

fn update(config: &Config, job: JobId, last: &mut String) -> Option<(JobRecord, Value)> {
    let response = Client::connect_with_timeout(config, POLL_TIMEOUT).and_then(|client| {
        client.send(Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: OperationId::new(),
            command: Command::JobShow {
                job,
                applied_after: None,
            },
            dry_run: false,
            no_wait: true,
            wait_for: Milestone::Ingested,
        })
    });
    if let Ok(Response {
        outcome: ResponseOutcome::Job { job, detail },
        ..
    }) = response
    {
        *last = describe(&job, &detail);
        Some((job, detail))
    } else {
        None
    }
}
fn phase<'a>(job: &JobRecord, detail: &'a Value) -> &'a str {
    match job.state {
        JobState::WaitingNas => "Waiting for archive",
        JobState::WaitingCapacity
            if job.reason.as_deref().is_some_and(|r| {
                r.contains("measur") || r.contains("occupancy") || r.contains("reconciliation")
            }) =>
        {
            "Measuring cache usage"
        }
        JobState::WaitingCapacity => "Waiting for local space",
        JobState::WaitingSource => "Waiting for card",
        JobState::WaitingImmich => "Waiting for Immich",
        JobState::WaitingApplication => "Waiting for application",
        JobState::Conflict => "Conflict",
        JobState::Failed => "Failed",
        JobState::Cancelled | JobState::CancelRequested => "Cancelled or cancelling",
        JobState::Complete => "Complete",
        _ if matches!(job.request.kind, JobKind::Import { .. }) => {
            if job.local_ready {
                "Waiting for archive"
            } else if detail["scan_complete"].as_bool() == Some(true) {
                "Copying to local cache"
            } else {
                "Checking card"
            }
        }
        _ => "Working",
    }
}
fn import_total(detail: &Value) -> u64 {
    let selected = detail["selected"].as_u64().unwrap_or(0);
    if let Some(reused) = detail["archive_reused"].as_u64() {
        selected
            .saturating_sub(reused)
            .saturating_sub(detail["image_duplicates"].as_u64().unwrap_or(0))
    } else {
        selected.saturating_sub(detail["recognized"].as_u64().unwrap_or(0))
    }
}
fn uploaded(detail: &Value) -> u64 {
    let count = |key: &str| detail[key].as_u64().unwrap_or(0);
    if let Some(reused) = detail["archive_reused"].as_u64() {
        count("archive_committed").saturating_sub(reused)
    } else {
        (count("archive_committed") + count("image_duplicates")).saturating_sub(count("recognized"))
    }
}
pub(super) fn describe(job: &JobRecord, detail: &Value) -> String {
    let mut line = phase(job, detail).to_owned();
    if matches!(job.request.kind, JobKind::Import { .. }) {
        let count = |key: &str| detail[key].as_u64().unwrap_or(0);
        let total = import_total(detail);
        line.push_str(&format!(
            "; checked {}{}; {} already known; copied {}; {} uploaded; {} queued",
            count("selected") + count("skipped"),
            if detail["scan_complete"].as_bool() == Some(true) {
                " (scan complete)"
            } else {
                " so far"
            },
            count("recognized"),
            detail["copy_ready"]
                .as_u64()
                .unwrap_or_else(|| count("local_ready"))
                .min(total),
            uploaded(detail),
            count("pending_archive")
        ));
        if count("pending_jpeg_checks") > 0 {
            line.push_str(&format!(
                "; {} JPEG checks pending",
                count("pending_jpeg_checks")
            ));
        }
        if count("conflicting") > 0 {
            line.push_str(&format!(
                "; {} conflicts need attention",
                count("conflicting")
            ));
        }
        if count("skipped") > 0 {
            line.push_str(&format!(
                "; {} unsupported files skipped (not protected)",
                count("skipped")
            ));
        }
        if job.local_ready
            && count("skipped") == 0
            && count("conflicting") == 0
            && count("pending_jpeg_checks") == 0
            && !matches!(
                job.state,
                JobState::Conflict
                    | JobState::Failed
                    | JobState::Cancelled
                    | JobState::CancelRequested
            )
        {
            if job.archive_committed {
                line.push_str(". Card can be removed; archive complete.");
            } else {
                line.push_str(". Card can be removed; background upload continues until archived.");
            }
        }
    }
    if let Some(reason) = &job.reason {
        line.push_str(&format!("; {}", compact(reason)));
    }
    line
}

pub(super) fn status(data: &Value) -> String {
    let activity = &data["activity"];
    let active = activity["active_count"].as_u64().unwrap_or(0);
    let mut lines = vec![format!("Current work: {active} active jobs")];
    if let Some(jobs) = activity["active"].as_array() {
        for entry in jobs.iter().take(8) {
            if let Ok(job) = serde_json::from_value::<JobRecord>(entry["job"].clone()) {
                let label = match &job.request.kind {
                    JobKind::Import { source, .. } => {
                        format!("Import {}", compact(&source.display().to_string()))
                    }
                    JobKind::LocalTransfer { .. } => "Local copy".into(),
                    JobKind::SnapshotCapture { context, .. }
                        if context["owner"]["kind"].as_str() == Some("Generated") =>
                    {
                        format!(
                            "Saving {}",
                            compact(
                                context["owner"]["path"]
                                    .as_str()
                                    .unwrap_or("generated file")
                            )
                        )
                    }
                    _ => "Background work".into(),
                };
                lines.push(format!("  {label}: {}", describe(&job, &entry["detail"])));
            }
        }
        if active > jobs.len() as u64 {
            lines.push(format!("  … and {} more", active - jobs.len() as u64));
        }
    }
    lines.push(format!(
        "Upload: {} queued; archive {}",
        activity["pending_upload"].as_u64().unwrap_or(0),
        data["archive"]["availability"]
            .as_str()
            .unwrap_or("unknown")
    ));
    if activity["generated"]["total"].as_u64().unwrap_or(0) > 0 {
        lines.push(format!(
            "Generated files: {} tracked; {} pending",
            activity["generated"]["total"].as_u64().unwrap_or(0),
            activity["generated"]["pending"].as_u64().unwrap_or(0),
        ));
    }
    if let Some(reason) = data["worker"]["publication_reason"].as_str() {
        lines.push(format!("Waiting: {}", compact(reason)));
    }
    let space = &data["space"];
    let bytes = |value: &Value| {
        value
            .as_u64()
            .map(|v| format!("{:.1} GiB", v as f64 / 1073741824.0))
            .unwrap_or_else(|| "unmeasured".into())
    };
    lines.push(format!(
        "Cache: {} used / {} budget; {} filesystem free; {} reserved",
        bytes(&space["exclusive_total"]),
        bytes(&space["budget"]),
        bytes(&space["filesystem_free"]),
        bytes(&space["remaining_reservations"])
    ));
    if let Some(observed) = space["occupancy_observed_at"].as_u64() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(observed);
        lines.push(format!(
            "  Cache measurement: {}s ago",
            now.saturating_sub(observed)
        ));
    }
    if let Some(reason) = space["occupancy_reason"].as_str() {
        lines.push(format!("Warning: cache measurement: {}", compact(reason)));
    }
    if let Some(reason) = data["archive"]["reason"].as_str() {
        lines.push(format!("Warning: archive: {}", compact(reason)));
    }
    if let Some(conflicts) = data["conflicts"].as_array().filter(|v| !v.is_empty()) {
        lines.push(format!(
            "Warning: {} conflicts on this page; use conflicts list",
            conflicts.len()
        ));
    }
    if let Some(recent) = activity["recent"].as_array().filter(|v| !v.is_empty()) {
        lines.push(format!(
            "Recent completed jobs: {}",
            recent
                .iter()
                .take(3)
                .filter_map(|j| j["request"]["kind"]["type"].as_str())
                .map(|kind| kind.replace('_', " "))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    lines.join("\n")
}
fn compact(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(180)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::DurableRequest;

    #[test]
    fn current_import_counts_exclude_recognized_archive_items() {
        let job = JobRecord {
            id: JobId::new(),
            request: DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::Import {
                    source: "/card".into(),
                    strict: false,
                },
            },
            state: JobState::Running,
            generation: 1,
            local_ready: true,
            archive_committed: false,
            reason: None,
            cancel_requested: false,
        };
        let line = describe(
            &job,
            &serde_json::json!({
                "selected":5396,"recognized":5278,"image_duplicates":942,
                "archive_committed":4352,"local_ready":118,"pending_archive":102,"scan_complete":true
            }),
        );
        assert!(line.contains("copied 118"), "{line}");
        assert!(line.contains("16 uploaded"), "{line}");
        assert!(line.contains("102 queued"), "{line}");
        assert!(line.contains("Card can be removed"), "{line}");
        assert!(!line.contains(&job.id.to_string()), "{line}");
    }

    #[test]
    fn conflicts_never_advise_removing_the_card() {
        let job = JobRecord {
            id: JobId::new(),
            request: DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::Import {
                    source: "/card".into(),
                    strict: false,
                },
            },
            state: JobState::Conflict,
            generation: 1,
            local_ready: true,
            archive_committed: false,
            reason: None,
            cancel_requested: false,
        };
        let line = describe(
            &job,
            &serde_json::json!({"selected":2,"local_ready":1,"conflicting":1,"scan_complete":true}),
        );
        assert!(!line.contains("Card can be removed"), "{line}");
    }

    #[test]
    fn skipped_files_never_advise_removing_the_card() {
        let job = JobRecord {
            id: JobId::new(),
            request: DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::Import {
                    source: "/card".into(),
                    strict: false,
                },
            },
            state: JobState::Running,
            generation: 1,
            local_ready: true,
            archive_committed: false,
            reason: None,
            cancel_requested: false,
        };
        let line = describe(
            &job,
            &serde_json::json!({"selected":1,"skipped":1,"local_ready":1,"scan_complete":true}),
        );
        assert!(line.contains("not protected"), "{line}");
        assert!(!line.contains("Card can be removed"), "{line}");
    }

    #[test]
    fn non_import_terminal_progress_shows_only_activity() {
        let job = JobRecord {
            id: JobId::new(),
            request: DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::ArchiveInit,
            },
            state: JobState::Running,
            generation: 1,
            local_ready: false,
            archive_committed: false,
            reason: None,
            cancel_requested: false,
        };
        let multi = MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden());
        let bars = std::array::from_fn(|_| multi.add(ProgressBar::new_spinner()));
        let mut display = Display {
            terminal: Some((multi, bars)),
            last: String::new(),
            phase: String::new(),
            printed: Instant::now(),
        };
        display.update(&job, &serde_json::json!({}));
        let (_, bars) = display.terminal.as_ref().unwrap();
        for bar in &bars[..3] {
            assert!(
                bar.is_finished(),
                "import row still active for a non-import job"
            );
        }
        assert_eq!(bars[3].message(), "Working");
    }

    #[test]
    fn local_only_recognized_files_still_need_upload() {
        let job = JobRecord {
            id: JobId::new(),
            request: DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::Import {
                    source: "/card".into(),
                    strict: false,
                },
            },
            state: JobState::Running,
            generation: 1,
            local_ready: true,
            archive_committed: false,
            reason: None,
            cancel_requested: false,
        };
        let multi = MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden());
        let bars = std::array::from_fn(|_| multi.add(ProgressBar::new_spinner()));
        let mut display = Display {
            terminal: Some((multi, bars)),
            last: String::new(),
            phase: String::new(),
            printed: Instant::now(),
        };
        display.update(&job, &serde_json::json!({"selected":1,"recognized":1,"archive_reused":0,"local_ready":1,"archive_committed":0,"pending_archive":1,"scan_complete":true}));
        let (_, bars) = display.terminal.as_ref().unwrap();
        assert_eq!(bars[1].length(), Some(1));
        assert_eq!(bars[1].position(), 1);
        assert_eq!(bars[2].length(), Some(1));
        assert_eq!(bars[2].position(), 0);
    }

    #[test]
    fn archived_local_companions_do_not_complete_new_copy_progress() {
        let job = JobRecord {
            id: JobId::new(),
            request: DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::Import {
                    source: "/card".into(),
                    strict: false,
                },
            },
            state: JobState::Running,
            generation: 1,
            local_ready: true,
            archive_committed: false,
            reason: None,
            cancel_requested: false,
        };
        let multi = MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden());
        let bars = std::array::from_fn(|_| multi.add(ProgressBar::new_spinner()));
        let mut display = Display {
            terminal: Some((multi, bars)),
            last: String::new(),
            phase: String::new(),
            printed: Instant::now(),
        };
        display.update(&job, &serde_json::json!({"selected":2,"recognized":1,"archive_reused":1,"local_ready":1,"copy_ready":0,"archive_committed":1,"pending_archive":1,"scan_complete":true}));
        let line = describe(
            &job,
            &serde_json::json!({"selected":2,"archive_reused":1,"local_ready":1,"copy_ready":0,"pending_archive":1}),
        );
        assert!(line.contains("copied 0"), "{line}");
        let (_, bars) = display.terminal.as_ref().unwrap();
        assert_eq!(bars[1].length(), Some(1));
        assert_eq!(bars[1].position(), 0);
        assert_eq!(bars[2].length(), Some(1));
        assert_eq!(bars[2].position(), 0);
    }

    #[test]
    fn older_service_local_milestone_still_reports_archive_phase() {
        let job = JobRecord {
            id: JobId::new(),
            request: DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::Import {
                    source: "/card".into(),
                    strict: false,
                },
            },
            state: JobState::Running,
            generation: 1,
            local_ready: true,
            archive_committed: false,
            reason: None,
            cancel_requested: false,
        };
        let line = describe(
            &job,
            &serde_json::json!({"selected": 12, "recognized": 10, "archive_committed": 10, "local_ready": 2, "pending_archive": 2}),
        );
        assert!(line.contains("Waiting for archive"), "{line}");
        assert!(line.contains("10 already known"), "{line}");
        assert!(line.contains("2 queued"), "{line}");
        assert!(!line.contains("scan complete"), "{line}");
    }
}

#[cfg(test)]
mod connect_tests {
    use super::*;
    use std::os::{
        fd::AsRawFd,
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    };

    #[test]
    fn progress_update_returns_when_service_accept_backlog_is_full() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let socket = state.join("service.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let _queued = UnixStream::connect(&socket).unwrap();
        let config = Config::new(
            temp.path().join("local"),
            state,
            temp.path().join("archive"),
        );
        let (tx, rx) = mpsc::channel();
        let poller = std::thread::spawn(move || {
            let mut line = "previous progress".to_string();
            update(&config, JobId::new(), &mut line);
            tx.send(line).unwrap();
        });
        let result = rx.recv_timeout(Duration::from_millis(750));
        drop(listener);
        poller.join().unwrap();
        assert_eq!(result.unwrap(), "previous progress");
    }
}
