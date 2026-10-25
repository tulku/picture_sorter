use crate::{
    catalog::PairEvidence,
    domain::*,
    error::{AppError, AppResult},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub const MAX_COMMIT_BYTES: usize = 8 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitEnvelope {
    pub schema_version: u32,
    pub archive_id: ArchiveId,
    pub commit_id: CommitId,
    pub writer_id: WriterId,
    pub writer_sequence: u64,
    pub dependencies: Vec<CommitId>,
    pub payload: String,
    pub payload_digest: Digest,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireRecord {
    pub schema_version: u32,
    #[serde(flatten)]
    pub operation: ArchiveRecord,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArchiveRecord {
    CaptureRecorded {
        capture: crate::import::CaptureRecord,
    },
    SequenceRecorded {
        sequence: crate::import::SequenceRecord,
    },
    AmbiguityRecorded {
        id: ConflictId,
        kind: String,
        group: Option<GroupId>,
        details: serde_json::Value,
    },
    SemanticAmbiguityResolved {
        request: crate::catalog::ResolutionRequest,
        witness: serde_json::Value,
    },
    AmbiguityResolved {
        request: crate::catalog::ResolutionRequest,
    },
    MetadataCached {
        result: crate::import::MetadataResult,
    },
    ProjectionPlanned {
        plan: super::ProjectionPlan,
    },
    GroupCreated {
        id: GroupId,
    },
    MemberAttached {
        group: GroupId,
        member: OriginalMember,
        evidence: PairEvidence,
    },
    GroupsMerged {
        groups: Vec<GroupId>,
        evidence: PairEvidence,
    },
    SidecarCreated {
        id: SidecarId,
        group: Option<GroupId>,
        path: ArchivePath,
        owner: String,
    },
    VersionAdded {
        version: Version,
        operation: OperationId,
    },
    HeadsChanged {
        sidecar: SidecarId,
        expected: Vec<VersionId>,
        heads: Vec<VersionId>,
    },
    ConflictRecorded {
        id: ConflictId,
        group: Option<GroupId>,
        kind: String,
        heads: Vec<VersionId>,
    },
    ConflictResolved {
        id: ConflictId,
        expected_revision: u64,
        version: VersionId,
    },
    MappingRecorded {
        remote_checksum: String,
        external_path: String,
        server: String,
        asset: uuid::Uuid,
        library: Option<uuid::Uuid>,
        path: ArchivePath,
        digest: Digest,
        group: GroupId,
    },
    FavoriteConverged {
        evidence: crate::catalog::FavoriteConvergence,
    },
    FavoriteObserved {
        operation: OperationId,
        group: GroupId,
        local: bool,
        remote: bool,
    },
    RatingConverged {
        evidence: crate::catalog::RatingConvergence,
    },
    RatingObserved {
        operation: OperationId,
        group: GroupId,
        local: RatingObservation,
        remote: RatingObservation,
    },
}
impl CommitEnvelope {
    pub fn new(
        archive_id: ArchiveId,
        writer_id: WriterId,
        writer_sequence: u64,
        dependencies: Vec<CommitId>,
        records: Vec<ArchiveRecord>,
    ) -> AppResult<Self> {
        let records: Vec<_> = records
            .into_iter()
            .map(|operation| WireRecord {
                schema_version: 1,
                operation,
            })
            .collect();
        let payload = serde_json::to_string(&records)?;
        Ok(Self {
            schema_version: 1,
            archive_id,
            commit_id: CommitId::new(),
            writer_id,
            writer_sequence,
            dependencies,
            payload_digest: Digest::of(payload.as_bytes()),
            payload,
        })
    }
    pub fn records(&self) -> AppResult<Vec<ArchiveRecord>> {
        if self.schema_version != 1
            || self.writer_sequence == 0
            || self.dependencies.contains(&self.commit_id)
            || self.dependencies.iter().collect::<BTreeSet<_>>().len() != self.dependencies.len()
        {
            return Err(AppError::integrity(
                "invalid commit schema, sequence or dependencies",
            ));
        }
        if Digest::of(self.payload.as_bytes()) != self.payload_digest {
            return Err(AppError::integrity("payload digest mismatch"));
        }
        let records: Vec<WireRecord> = serde_json::from_str(&self.payload)?;
        if records.iter().any(|r| r.schema_version != 1) {
            return Err(AppError::integrity("unsupported record schema"));
        }
        Ok(records.into_iter().map(|r| r.operation).collect())
    }
    pub fn encode(&self) -> AppResult<Vec<u8>> {
        if self.payload.len() > MAX_COMMIT_BYTES {
            return Err(AppError::integrity("oversized commit"));
        }
        self.records()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_COMMIT_BYTES {
            return Err(AppError::integrity("oversized commit"));
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8], expected: ArchiveId) -> AppResult<Self> {
        if bytes.len() > MAX_COMMIT_BYTES {
            return Err(AppError::integrity("oversized commit"));
        }
        let c: Self = serde_json::from_slice(bytes)?;
        if c.archive_id != expected {
            return Err(AppError::integrity("archive identity mismatch"));
        }
        c.records()?;
        Ok(c)
    }
}
