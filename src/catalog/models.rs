use crate::domain::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobKind {
    ArchiveInit,
    RecoverLock {
        prior_owner_stopped: bool,
        all_writers_quiescent: bool,
    },
    Backup {
        destination: PathBuf,
    },
    Rebuild {
        replacement: PathBuf,
    },
    ResolveAmbiguity {
        request: super::ResolutionRequest,
    },
    Qualify {
        request: serde_json::Value,
    },
    Unpin {
        group: GroupId,
    },
    Import {
        source: PathBuf,
        strict: bool,
    },
    Pin {
        group: GroupId,
    },
    Bootstrap {
        scope: Option<ArchivePath>,
    },
    Refresh {
        groups: Vec<GroupId>,
    },
    PinUrl {
        server: String,
        asset: uuid::Uuid,
    },
    Clean,
    Evict {
        group: GroupId,
        revision: MembershipRevision,
        members: Vec<OriginalMember>,
        activity: Timestamp,
    },
    Resolve {
        conflict: ConflictId,
        expected_revision: u64,
        chosen: VersionId,
    },
    Verify {
        archive: bool,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableRequest {
    pub operation_id: OperationId,
    pub kind: JobKind,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobState {
    Queued,
    Running,
    WaitingSource,
    WaitingNas,
    WaitingImmich,
    WaitingCapacity,
    WaitingApplication,
    Conflict,
    Failed,
    CancelRequested,
    Cancelled,
    Complete,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: JobId,
    pub request: DurableRequest,
    pub state: JobState,
    pub generation: u64,
    pub local_ready: bool,
    pub archive_committed: bool,
    pub reason: Option<String>,
    pub cancel_requested: bool,
}
#[derive(Debug, Clone)]
pub enum JobTransition {
    Start,
    Wait(JobState, String),
    Ingested,
    Archived,
    Complete,
    RequestCancel,
    ConfirmCancelled,
}
