use crate::{
    catalog::{JobKind, JobRecord, JobState},
    config::Config,
    domain::{JobId, OperationId},
    error::{AppError, AppResult},
    service::{Client, protocol::*},
};
use serde_json::Value;
use std::{sync::mpsc, time::Duration};

const HEARTBEAT: Duration = Duration::from_secs(2);
const POLL_TIMEOUT: Duration = Duration::from_millis(250);

pub(super) fn send(config: &Config, mut request: Request) -> AppResult<Response> {
    if let Command::Import { source, .. } = &request.command {
        eprintln!("Checking card {}; contacting service", source.display());
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
    eprintln!("Job {job}: accepted; checking progress");
    request.command = Command::JobWait { job };
    let client = Client::connect(config)?;
    let (tx, rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _ = tx.send(client.send(request));
    });
    let mut last = format!("Job {job}: checking progress");
    update(config, job, &mut last);
    loop {
        match rx.recv_timeout(HEARTBEAT) {
            Ok(result) => {
                waiter
                    .join()
                    .map_err(|_| AppError::invalid("service wait thread failed"))?;
                let response = result?;
                if let ResponseOutcome::Job { job, detail } = &response.outcome {
                    eprintln!("{}", describe(job, detail));
                }
                return Ok(response);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                update(config, job, &mut last);
                eprintln!("{last}");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = waiter.join();
                return Err(AppError::invalid("service wait thread disconnected"));
            }
        }
    }
}

fn update(config: &Config, job: JobId, last: &mut String) {
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
    }
}

fn describe(job: &JobRecord, detail: &Value) -> String {
    let phase = match job.state {
        JobState::WaitingNas => "waiting for archive",
        JobState::WaitingCapacity => "waiting for local space",
        JobState::WaitingSource => "waiting for card",
        JobState::WaitingImmich => "waiting for Immich",
        JobState::WaitingApplication => "waiting for application",
        JobState::Conflict => "conflict",
        JobState::Failed => "failed",
        JobState::Cancelled | JobState::CancelRequested => "cancelled or cancelling",
        JobState::Complete => "complete",
        _ if matches!(job.request.kind, JobKind::Import { .. }) => {
            if job.local_ready {
                "waiting for archive"
            } else if detail["scan_complete"].as_bool() == Some(true) {
                "local copy"
            } else {
                "checking card"
            }
        }
        _ => "working",
    };
    let mut line = format!("Job {}: {phase}", job.id);
    if matches!(job.request.kind, JobKind::Import { .. }) {
        let count = |key: &str| detail[key].as_u64().unwrap_or(0);
        let checked = count("selected") + count("skipped");
        let scope = if detail["scan_complete"].as_bool() == Some(true) {
            "scan complete"
        } else {
            "so far"
        };
        line.push_str(&format!(
            "; checked {checked} ({scope}); recognised {}; archive ready {}; local ready {}; pending upload {}; pending JPEG checks {}",
            count("recognized"), count("archive_committed"), count("local_ready"), count("pending_archive"), count("pending_jpeg_checks")
        ));
    }
    if let Some(reason) = &job.reason {
        line.push_str(&format!("; {reason}"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::DurableRequest;

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
        assert!(line.contains("waiting for archive"), "{line}");
        assert!(line.contains("recognised 10"), "{line}");
        assert!(line.contains("archive ready 10"), "{line}");
        assert!(line.contains("pending upload 2"), "{line}");
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
