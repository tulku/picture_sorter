use crate::{
    catalog::{JobRecord, ResolutionRequest},
    domain::*,
    error::{AppError, ErrorCode},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_MESSAGE: usize = 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u32,
    pub request_id: OperationId,
    pub command: Command,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub no_wait: bool,
    #[serde(default)]
    pub wait_for: Milestone,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Milestone {
    #[default]
    Ingested,
    Archived,
    Ready,
}
#[derive(Debug, Clone, Serialize, Deserialize, clap::Args)]
#[serde(default, deny_unknown_fields)]
pub struct StatusQuery {
    #[arg(long)]
    pub groups_after: Option<GroupId>,
    #[arg(long)]
    pub jobs_after: Option<JobId>,
    #[arg(long)]
    pub conflicts_after: Option<ConflictId>,
    #[arg(long, default_value_t = 128)]
    pub limit: usize,
}
impl Default for StatusQuery {
    fn default() -> Self {
        Self {
            groups_after: None,
            jobs_after: None,
            conflicts_after: None,
            limit: 128,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Status,
    StatusPage {
        query: StatusQuery,
    },
    ConflictsPage {
        after: Option<ConflictId>,
        limit: usize,
    },
    Import {
        source: PathBuf,
        strict: bool,
    },
    Bootstrap {
        scope: Option<ArchivePath>,
    },
    Pin {
        target: String,
    },
    Unpin {
        group: GroupId,
    },
    Refresh {
        groups: Vec<GroupId>,
        editor_closed: bool,
        stage_only: bool,
    },
    Clean,
    JobShow {
        job: JobId,
    },
    JobWait {
        job: JobId,
    },
    JobCancel {
        job: JobId,
    },
    ConflictsList,
    ConflictResolve {
        request: ResolutionRequest,
    },
    Verify {
        archive: bool,
    },
    ArchiveInit,
    RecoverLock {
        prior_owner_stopped: bool,
        all_writers_quiescent: bool,
    },
    CatalogBackup {
        destination: PathBuf,
    },
    CatalogRebuild {
        replacement: PathBuf,
    },
    Doctor,
    Qualify {
        request: serde_json::Value,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub protocol_version: u32,
    pub request_id: OperationId,
    pub outcome: ResponseOutcome,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseOutcome {
    Data {
        data: serde_json::Value,
    },
    Accepted {
        job: JobId,
    },
    Job {
        job: JobRecord,
        detail: serde_json::Value,
    },
    Error {
        error: AppError,
    },
}
impl Response {
    pub fn error(id: OperationId, error: AppError) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: id,
            outcome: ResponseOutcome::Error { error },
        }
    }
    pub fn data(id: OperationId, data: impl Serialize) -> crate::error::AppResult<Self> {
        Ok(Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: id,
            outcome: ResponseOutcome::Data {
                data: serde_json::to_value(data)?,
            },
        })
    }
    pub fn exit_code(&self) -> i32 {
        match &self.outcome {
            ResponseOutcome::Error { error } => error_exit(error),
            ResponseOutcome::Job { job, .. } => match job.state {
                crate::catalog::JobState::Failed => 5,
                crate::catalog::JobState::Conflict => 4,
                crate::catalog::JobState::Complete => 0,
                _ => 3,
            },
            _ => 0,
        }
    }
}
pub fn error_exit(error: &AppError) -> i32 {
    match error.code {
        ErrorCode::InvalidConfig | ErrorCode::Unsupported => 2,
        ErrorCode::Authentication
        | ErrorCode::Permission
        | ErrorCode::Unavailable
        | ErrorCode::NotFound
        | ErrorCode::Capacity => 3,
        ErrorCode::Conflict | ErrorCode::Integrity | ErrorCode::SourceChanged => 4,
        ErrorCode::Io
            if matches!(
                error.errno,
                Some(libc::ENOENT | libc::ECONNREFUSED | libc::ENOTCONN)
            ) =>
        {
            3
        }
        _ => 5,
    }
}
