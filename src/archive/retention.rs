use super::*;
use crate::{
    catalog::{GeneratedRetentionPlan, GeneratedRetentionReceipt},
    domain::*,
    error::{AppError, AppResult},
    transfer::digest_file,
};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetirementProof {
    schema_version: u32,
    archive: ArchiveId,
    version: Version,
    commit: ArchivePath,
}
fn receipt_path(version: VersionId) -> AppResult<ArchivePath> {
    path(&format!(
        ".photo-sorter/generated-retirements/{version}.json"
    ))
}
impl ArchiveStore {
    pub(super) fn retired_version_proven(&self, version: &Version) -> AppResult<bool> {
        let bytes = match self
            .root
            .read(&receipt_path(version.id)?, MAX_COMMIT_BYTES as u64)
        {
            Ok(bytes) => bytes,
            Err(e) if e.errno == Some(libc::ENOENT) => return Ok(false),
            Err(e) => return Err(e),
        };
        let proof: RetirementProof = serde_json::from_slice(&bytes)?;
        if proof.schema_version != 1
            || proof.archive != self.marker.archive_id
            || proof.version.id != version.id
            || proof.version.sidecar != version.sidecar
            || proof.version.body != version.body
        {
            return Err(AppError::integrity("retirement receipt version differs"));
        }
        let commit = self.read_commit_raw(&proof.commit)?;
        let records = commit.records()?;
        let [ArchiveRecord::GeneratedPayloadsRetired { current, versions }] = records.as_slice()
        else {
            return Err(AppError::integrity(
                "retirement receipt references another record",
            ));
        };
        if current.sidecar != version.sidecar
            || current.version == version.id
            || !versions.contains(&version.id)
        {
            return Err(AppError::integrity(
                "retirement receipt does not retire exact version",
            ));
        }
        self.verify_generated_owner(current)?;
        Ok(true)
    }
    pub fn retire_generated(
        &self,
        guard: &mut PublicationGuard,
        plan: GeneratedRetentionPlan,
        faults: &dyn FaultInjector,
    ) -> AppResult<GeneratedRetentionReceipt> {
        self.require_generated_replacement()?;
        self.retire_generated_protocol(guard, plan, faults)
    }
    pub(crate) fn retire_generated_protocol(
        &self,
        guard: &mut PublicationGuard,
        plan: GeneratedRetentionPlan,
        faults: &dyn FaultInjector,
    ) -> AppResult<GeneratedRetentionReceipt> {
        self.require_writable()?;
        self.validate_guard(guard)?;
        if !guard.replayed()
            || !matches!(
                self.marker.mode,
                PublicationMode::SingleWriter | PublicationMode::LocalTest
            )
        {
            return Err(AppError::integrity(
                "retirement requires fresh SingleWriter publication fence",
            ));
        }
        let auth = &plan.authorization;
        let current = auth.current();
        let records = plan.commit.records()?;
        let [
            ArchiveRecord::GeneratedPayloadsRetired {
                current: recorded,
                versions,
            },
        ] = records.as_slice()
        else {
            return Err(AppError::integrity(
                "retirement commit must contain exact retirement record",
            ));
        };
        if auth.archive() != self.marker.archive_id
            || recorded != current
            || versions.len() != auth.versions().len()
            || !auth.versions().iter().all(|v| {
                versions.contains(&v.id) && v.sidecar == current.sidecar && v.id != current.version
            })
        {
            return Err(AppError::integrity("retirement authorization differs"));
        }
        self.committed_projection(auth.publication(), current)?;
        self.verify_generated_owner(current)?;
        if self.generated_body(&current.path)? != current.body {
            return Err(AppError::integrity(
                "retirement current visible bytes differ",
            ));
        }
        let VersionBody::Content { digest, size } = &current.body else {
            return Err(AppError::integrity(
                "retirement current must contain content",
            ));
        };
        let mut history = self.root.open_relative(
            &history_path(digest)?,
            libc::O_RDONLY | libc::O_NONBLOCK,
            0,
        )?;
        if digest_file(&mut history)? != (digest.clone(), *size) {
            return Err(AppError::integrity("retirement current history corrupt"));
        }
        for projection in auth.projections() {
            if projection.sidecar != current.sidecar
                || projection.path != current.path
                || projection.owner != GENERATED_OUTPUT_OWNER
            {
                return Err(AppError::integrity(
                    "retirement projection ownership differs",
                ));
            }
        }
        match self.apply_generated_protocol(current, faults)? {
            ProjectionOutcome::Applied | ProjectionOutcome::AlreadyCurrent => {}
            ProjectionOutcome::Conflict { .. } => {
                return Err(AppError::integrity(
                    "retirement current projection evidence conflicts",
                ));
            }
        }
        let completed = path(&format!(
            ".photo-sorter/generated/{}/completed-{}.json",
            current.sidecar, current.version
        ))?;
        let completed_bytes = serde_json::to_vec(current)?;
        if self.root.read(&completed, 65536)? != completed_bytes {
            return Err(AppError::integrity(
                "retirement lacks completed current projection",
            ));
        }
        self.publish_originals(
            guard,
            PublicationPlan {
                commit: plan.commit.clone(),
                staged: vec![],
                attempt: OperationId::new(),
            },
            faults,
        )?;
        faults.hit(FaultPoint::GeneratedRetirementCommitted)?;
        self.root
            .mkdir_all(&path(".photo-sorter/generated-retirements")?)?;
        let commit_path = path(&format!(
            ".photo-sorter/commits/{}/{}-{}.json",
            plan.commit.writer_id, plan.commit.writer_sequence, plan.commit.commit_id
        ))?;
        for version in auth.versions() {
            if !self.retired_version_proven(version)? {
                let proof = RetirementProof {
                    schema_version: 1,
                    archive: self.marker.archive_id,
                    version: version.clone(),
                    commit: commit_path.clone(),
                };
                self.publish_generated_marker(
                    &receipt_path(version.id)?,
                    &serde_json::to_vec(&proof)?,
                    FaultPoint::GeneratedRetirementReceiptStaged,
                    faults,
                )?;
            }
        }
        faults.hit(FaultPoint::GeneratedRetirementReceiptsPublished)?;
        let mut removed_digests = Vec::new();
        for removable_digest in auth.removable() {
            if removable_digest == digest {
                return Err(AppError::integrity(
                    "retirement authorization includes current digest",
                ));
            }
            let version = auth
                .versions()
                .iter()
                .find(|v| matches!(&v.body, VersionBody::Content { digest: d, .. } if d == removable_digest))
                .ok_or_else(|| AppError::integrity("retirement digest lacks version proof"))?;
            self.unlink_retired(&history_path(removable_digest)?, &version.body)?;
            removed_digests.push(removable_digest.clone());
            faults.hit(FaultPoint::GeneratedRetirementPayloadRemoved)?;
        }
        for projection in auth.projections() {
            let retained = path(&format!(
                ".photo-sorter/generated/{}/retained-{}",
                current.sidecar, projection.version
            ))?;
            self.unlink_retired(&retained, &projection.expected)?;
            faults.hit(FaultPoint::GeneratedRetirementRetainedRemoved)?;
        }
        self.validate_guard(guard)?;
        Ok(GeneratedRetentionReceipt {
            commit: plan.commit,
            removed_digests,
        })
    }
    fn unlink_retired(&self, target: &ArchivePath, expected: &VersionBody) -> AppResult<()> {
        let observed = self.generated_body(target)?;
        if observed == VersionBody::Reset {
            return Ok(());
        }
        if &observed != expected {
            return Err(AppError::integrity(
                "retirement target contains unknown bytes; preserved",
            ));
        }
        self.root.unlink(target, false)
    }
}
