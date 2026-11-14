use super::*;
use crate::{
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
    transfer::digest_file,
};
use serde::{Deserialize, Serialize};
use std::io::{Seek, SeekFrom, Write};
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GeneratedOwner {
    schema_version: u32,
    archive: ArchiveId,
    sidecar: SidecarId,
    path: ArchivePath,
    owner: String,
}
impl ArchiveStore {
    pub(crate) fn require_generated_replacement(&self) -> AppResult<()> {
        self.require_writable()?;
        if self.marker.mode == PublicationMode::LocalTest && self.root.is_test_archive() {
            return Ok(());
        }
        let mount = self.root.mount_identity()?;
        let fingerprint = crate::qualification::generated_fingerprint(
            &self.root,
            self.marker.archive_id,
            self.writer,
            &self.marker.mode,
            self.marker.mode_revision,
        )?;
        let decision =
            self.registry
                .allows(crate::qualification::OperationKind::GeneratedReplacement {
                    mount_identity: &mount,
                    fingerprint: &fingerprint,
                    mode: &self.marker.mode,
                });
        if decision.allowed {
            Ok(())
        } else {
            Err(AppError::new(ErrorCode::Unsupported, decision.reason))
        }
    }
    pub(crate) fn apply_generated_projection(
        &self,
        plan: &ProjectionPlan,
        faults: &dyn FaultInjector,
    ) -> AppResult<ProjectionOutcome> {
        self.require_generated_replacement()?;
        self.apply_generated_protocol(plan, faults)
    }
    pub(crate) fn apply_generated_protocol(
        &self,
        plan: &ProjectionPlan,
        faults: &dyn FaultInjector,
    ) -> AppResult<ProjectionOutcome> {
        self.require_writable()?;
        validate_saved_path(&plan.path, &plan.owner)?;
        let VersionBody::Content { digest, size } = &plan.body else {
            return Err(AppError::integrity("generated output cannot project Reset"));
        };
        let mut history = self.root.open_relative(
            &history_path(digest)?,
            libc::O_RDONLY | libc::O_NONBLOCK,
            0,
        )?;
        if digest_file(&mut history)? != (digest.clone(), *size) {
            return Err(AppError::integrity("generated projection history corrupt"));
        }
        let directory = path(&format!(".photo-sorter/generated/{}", plan.sidecar))?;
        let manifest = path(&format!("{}/owner.json", directory.as_str()))?;
        let owner = GeneratedOwner {
            schema_version: 1,
            archive: self.marker.archive_id,
            sidecar: plan.sidecar,
            path: plan.path.clone(),
            owner: plan.owner.clone(),
        };
        let bytes = serde_json::to_vec(&owner)?;
        match self.root.read(&manifest, 65536) {
            Ok(existing) if existing == bytes => {}
            Ok(_) => return Err(AppError::integrity("generated owner manifest differs")),
            Err(e) if e.errno == Some(libc::ENOENT) => {
                let observed = self.generated_body(&plan.path)?;
                if observed != VersionBody::Reset {
                    return Ok(ProjectionOutcome::Conflict {
                        observed,
                        preserved: None,
                    });
                }
                if plan.expected != VersionBody::Reset {
                    return Err(AppError::integrity(
                        "replacement lacks initial generated owner manifest",
                    ));
                }
                self.root.mkdir_all(&directory)?;
                self.publish_generated_marker(
                    &manifest,
                    &bytes,
                    FaultPoint::GeneratedOwnerStaged,
                    faults,
                )?;
            }
            Err(e) => return Err(e),
        }
        faults.hit(FaultPoint::IntentSaved)?;
        let retained = path(&format!("{}/retained-{}", directory.as_str(), plan.version))?;
        let staged = path(&format!("{}/staged-{}", directory.as_str(), plan.version))?;
        let prepared = path(&format!(
            "{}/prepared-{}.json",
            directory.as_str(),
            plan.version
        ))?;
        let retained_body = self.generated_body(&retained)?;
        let observed = self.generated_body(&plan.path)?;
        if retained_body != VersionBody::Reset && retained_body != plan.expected {
            return Ok(ProjectionOutcome::Conflict {
                observed: retained_body,
                preserved: Some(retained),
            });
        }
        let prepared_bytes = serde_json::to_vec(plan)?;
        let ready = match self.root.read(&prepared, 65536) {
            Ok(bytes) if bytes == prepared_bytes => true,
            Ok(_) => return Err(AppError::integrity("generated prepared intent differs")),
            Err(e) if e.errno == Some(libc::ENOENT) => false,
            Err(e) => return Err(e),
        };
        let mut staged_body = self.generated_body(&staged)?;
        if ready
            && staged_body == VersionBody::Reset
            && observed == plan.body
            && (plan.expected == VersionBody::Reset || retained_body == plan.expected)
        {
            return Ok(ProjectionOutcome::AlreadyCurrent);
        }
        if observed != plan.expected
            && !(observed == VersionBody::Reset
                && retained_body == plan.expected
                && plan.expected != VersionBody::Reset)
        {
            return Ok(ProjectionOutcome::Conflict {
                observed,
                preserved: None,
            });
        }
        if !ready && staged_body != VersionBody::Reset && staged_body != plan.body {
            let incomplete = path(&format!(
                "{}/incomplete-{}-{}",
                directory.as_str(),
                plan.version,
                OperationId::new()
            ))?;
            if publish_noreplace(&self.root, &staged, &incomplete)? != PublishOutcome::Published {
                return Err(AppError::integrity("incomplete stage retention collided"));
            }
            staged_body = VersionBody::Reset;
        }
        if staged_body == VersionBody::Reset {
            history.seek(SeekFrom::Start(0))?;
            let mut target = self.root.create_new(&staged)?;
            std::io::copy(&mut history, &mut target)?;
            target.flush()?;
            target.sync_all()?;
        }
        if self.generated_body(&staged)? != plan.body {
            return Err(AppError::integrity("generated staged bytes differ"));
        }
        if !ready {
            self.publish_generated_marker(
                &prepared,
                &prepared_bytes,
                FaultPoint::GeneratedPreparedStaged,
                faults,
            )?;
        }
        faults.hit(FaultPoint::Verified)?;
        if observed != VersionBody::Reset {
            if retained_body != VersionBody::Reset {
                return Ok(ProjectionOutcome::Conflict {
                    observed: retained_body,
                    preserved: Some(retained),
                });
            }
            if publish_noreplace(&self.root, &plan.path, &retained)? == PublishOutcome::Exists {
                return Ok(ProjectionOutcome::Conflict {
                    observed: self.generated_body(&retained)?,
                    preserved: Some(retained),
                });
            }
            let displaced = self.generated_body(&retained)?;
            if displaced != plan.expected {
                return Ok(ProjectionOutcome::Conflict {
                    observed: displaced,
                    preserved: Some(retained),
                });
            }
        }
        faults.hit(FaultPoint::OriginalPublished)?;
        if let Some((parent, _)) = plan.path.as_str().rsplit_once('/') {
            self.root.mkdir_all(&path(parent)?)?;
        }
        if publish_noreplace(&self.root, &staged, &plan.path)? == PublishOutcome::Exists {
            return Ok(ProjectionOutcome::Conflict {
                observed: self.generated_body(&plan.path)?,
                preserved: None,
            });
        }
        faults.hit(FaultPoint::ProjectionPublished)?;
        self.validate()?;
        Ok(ProjectionOutcome::Applied)
    }
    fn publish_generated_marker(
        &self,
        destination: &ArchivePath,
        bytes: &[u8],
        point: FaultPoint,
        faults: &dyn FaultInjector,
    ) -> AppResult<()> {
        let (directory, _) = destination
            .as_str()
            .rsplit_once('/')
            .ok_or_else(|| AppError::invalid("generated marker requires parent"))?;
        let staged = path(&format!("{directory}/marker-{}", OperationId::new()))?;
        self.root.write_new(&staged, bytes)?;
        faults.hit(point)?;
        if publish_noreplace(&self.root, &staged, destination)? == PublishOutcome::Exists
            && self.root.read(destination, 65536)? != bytes
        {
            return Err(AppError::integrity("generated marker publication collided"));
        }
        Ok(())
    }
    fn generated_body(&self, path: &ArchivePath) -> AppResult<VersionBody> {
        match self.observed_body(path) {
            Err(e) if e.errno == Some(libc::ENOENT) => Ok(VersionBody::Reset),
            other => other,
        }
    }
}
