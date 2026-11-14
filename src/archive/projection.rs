use super::*;
use crate::{
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
    transfer::digest_file,
};
use serde::{Deserialize, Serialize};
use std::io::Write;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionPlan {
    pub sidecar: SidecarId,
    pub path: ArchivePath,
    pub version: VersionId,
    pub expected: VersionBody,
    pub body: VersionBody,
    pub owner: String,
}
pub fn owns_projection(owner: &str, path: &ArchivePath) -> bool {
    owner != "jpeg-xmp-external" && !path.as_str().starts_with(".photo-sorter/")
}
pub struct CommittedProjection {
    plan: ProjectionPlan,
    authorization: Option<crate::catalog::GeneratedProjectionAuthorization>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProjectionOutcome {
    Applied,
    AlreadyCurrent,
    Conflict {
        observed: VersionBody,
        preserved: Option<ArchivePath>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionPublication {
    pub commit: CommitEnvelope,
    pub history: Vec<StagedHistory>,
    pub projections: Vec<ProjectionPlan>,
    pub attempt: OperationId,
    #[serde(default)]
    pub generated_authorizations: Vec<crate::catalog::GeneratedProjectionAuthorization>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedHistory {
    pub staged: ArchivePath,
    pub digest: Digest,
    pub size: u64,
}
impl ArchiveStore {
    pub fn committed_projection(
        &self,
        commit: &CommitEnvelope,
        plan: &ProjectionPlan,
    ) -> AppResult<CommittedProjection> {
        self.validate()?;
        let name = path(&format!(
            ".photo-sorter/commits/{}/{}-{}.json",
            commit.writer_id, commit.writer_sequence, commit.commit_id
        ))?;
        if commit.archive_id != self.marker.archive_id
            || self.root.read(&name, MAX_COMMIT_BYTES as u64)? != commit.encode()?
        {
            return Err(AppError::integrity(
                "projection has no committed journal proof",
            ));
        }
        if !commit.records()?.iter().any(|record|matches!(record,ArchiveRecord::ProjectionPlanned{plan:expected} if expected==plan)){return Err(AppError::integrity("projection intent not present in commit"));}
        Ok(CommittedProjection {
            plan: plan.clone(),
            authorization: None,
        })
    }
    pub fn authorize_projection(
        &self,
        mut proof: CommittedProjection,
        authorization: crate::catalog::GeneratedProjectionAuthorization,
    ) -> AppResult<CommittedProjection> {
        if !authorization.matches(self.marker.archive_id, &proof.plan) {
            return Err(AppError::integrity(
                "generated authorization mismatches projection",
            ));
        }
        proof.authorization = Some(authorization);
        Ok(proof)
    }
    pub fn apply_projection(&self, proof: &CommittedProjection) -> AppResult<ProjectionOutcome> {
        self.apply_projection_with_faults(proof, &NoFaults)
    }
    pub fn apply_projection_with_faults(
        &self,
        proof: &CommittedProjection,
        faults: &dyn FaultInjector,
    ) -> AppResult<ProjectionOutcome> {
        self.require_writable()?;
        let plan = &proof.plan;
        if plan.owner == GENERATED_OUTPUT_OWNER {
            if !(self.root.is_test_archive() && self.marker.mode == PublicationMode::LocalTest)
                && !proof
                    .authorization
                    .as_ref()
                    .is_some_and(|a| a.matches(self.marker.archive_id, plan))
            {
                return Err(AppError::integrity(
                    "generated projection requires fresh catalog ownership authorization",
                ));
            }
            return self.apply_generated_projection(plan, faults);
        }
        validate_sidecar_path(&plan.path)?;
        self.validate()?;
        if !owns_projection(&plan.owner, &plan.path) {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "projection is not a tool-owned live sidecar",
            ));
        }
        let observed =
            match self
                .root
                .open_relative(&plan.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)
            {
                Ok(mut file) => {
                    let (digest, size) = digest_file(&mut file)?;
                    VersionBody::Content { digest, size }
                }
                Err(e) if e.errno == Some(libc::ENOENT) => VersionBody::Reset,
                Err(e) => return Err(e),
            };
        if observed == plan.body {
            return Ok(ProjectionOutcome::AlreadyCurrent);
        }
        if observed != plan.expected {
            return Ok(ProjectionOutcome::Conflict {
                observed,
                preserved: None,
            });
        }
        if matches!(observed, VersionBody::Content { .. }) {
            self.require_exchange()?;
        }
        let displaced = path(&format!(
            ".photo-sorter/staging/displaced-{}-{}",
            plan.sidecar,
            OperationId::new()
        ))?;
        if let Some((parent, _)) = plan.path.as_str().rsplit_once('/') {
            self.root.mkdir_all(&path(parent)?)?;
        }
        match &plan.body {
            VersionBody::Reset => {
                if observed != VersionBody::Reset {
                    publish_noreplace(&self.root, &plan.path, &displaced)?;
                    let displaced_body = self.observed_body(&displaced)?;
                    if displaced_body != plan.expected {
                        return Ok(ProjectionOutcome::Conflict {
                            observed: displaced_body,
                            preserved: Some(displaced),
                        });
                    }
                }
            }
            VersionBody::Content { digest, size } => {
                let mut history = self.root.open_relative(
                    &history_path(digest)?,
                    libc::O_RDONLY | libc::O_NONBLOCK,
                    0,
                )?;
                if digest_file(&mut history)? != (digest.clone(), *size) {
                    return Err(AppError::integrity("projection history corrupt"));
                }
                use std::io::{Seek, SeekFrom};
                history.seek(SeekFrom::Start(0))?;
                let staged = path(&format!(
                    ".photo-sorter/staging/projection-{}",
                    OperationId::new()
                ))?;
                let mut file = self.root.create_new(&staged)?;
                std::io::copy(&mut history, &mut file)?;
                file.flush()?;
                file.sync_all()?;
                if observed == VersionBody::Reset {
                    if publish_noreplace(&self.root, &staged, &plan.path)? == PublishOutcome::Exists
                    {
                        return Ok(ProjectionOutcome::Conflict {
                            observed: self.observed_body(&plan.path)?,
                            preserved: None,
                        });
                    }
                } else {
                    self.root.exchange(&staged, &plan.path)?;
                    let displaced_body = self.observed_body(&staged)?;
                    if displaced_body != plan.expected {
                        return Ok(ProjectionOutcome::Conflict {
                            observed: displaced_body,
                            preserved: Some(staged),
                        });
                    }
                }
            }
        }
        self.validate()?;
        Ok(ProjectionOutcome::Applied)
    }
    pub(crate) fn observed_body(&self, path: &ArchivePath) -> AppResult<VersionBody> {
        let mut file = self
            .root
            .open_relative(path, libc::O_RDONLY | libc::O_NONBLOCK, 0)?;
        let (digest, size) = digest_file(&mut file)?;
        Ok(VersionBody::Content { digest, size })
    }
    pub fn publish_versions(
        &self,
        guard: &mut PublicationGuard,
        plan: VersionPublication,
        faults: &dyn FaultInjector,
    ) -> AppResult<PublicationReceipt> {
        for history in &plan.history {
            self.store_history(&history.staged, &history.digest, history.size)?;
            faults.hit(FaultPoint::HistoryPublished)?;
        }
        let mut receipt = self.publish_originals(
            guard,
            PublicationPlan {
                commit: plan.commit.clone(),
                staged: vec![],
                attempt: plan.attempt,
            },
            faults,
        )?;
        for projection in &plan.projections {
            let mut proof = self.committed_projection(&plan.commit, projection)?;
            if let Some(authorization) = plan
                .generated_authorizations
                .iter()
                .find(|a| a.matches(self.marker.archive_id, projection))
            {
                proof = self.authorize_projection(proof, authorization.clone())?;
            }
            if matches!(
                self.apply_projection_with_faults(&proof, faults)?,
                ProjectionOutcome::Conflict { .. }
            ) {
                receipt.projection_pending = true;
            }
            faults.hit(FaultPoint::ProjectionPublished)?;
        }
        Ok(receipt)
    }
}
