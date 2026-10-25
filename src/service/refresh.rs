use crate::{
    catalog::*,
    config::Config,
    domain::*,
    edit::RefreshReport,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
    storage::*,
    transfer::*,
};
#[derive(Default)]
pub(crate) struct RefreshApplication {
    active: Option<Active>,
}
struct Active {
    job: JobId,
    group: GroupId,
    ticket: WorkerTicket,
    reservations: Vec<ReservationId>,
}
impl RefreshApplication {
    pub fn active_group(&self) -> Option<GroupId> {
        self.active.as_ref().map(|a| a.group)
    }
    pub fn active_job(&self) -> Option<JobId> {
        self.active.as_ref().map(|a| a.job)
    }
    pub fn poll(
        &mut self,
        catalog: &mut Catalog,
        worker: &mut WorkerSupervisor,
        job: JobId,
    ) -> AppResult<Option<RefreshReport>> {
        let Some(active) = self.active.take() else {
            return Ok(None);
        };
        if active.job != job {
            self.active = Some(active);
            return Ok(None);
        }
        match worker.poll(active.ticket)? {
            WorkerState::Active | WorkerState::TimedOutStillAlive | WorkerState::Reaped => {
                self.active = Some(active);
                Ok(None)
            }
            result => {
                for reservation in active.reservations {
                    catalog.db.execute(
                        "DELETE FROM reservations WHERE id=?",
                        [reservation.to_string()],
                    )?;
                }
                match result {
                    WorkerState::Completed(NasReply::Refreshed(report)) => Ok(Some(report)),
                    WorkerState::Failed(error) => Err(error),
                    _ => Err(AppError::integrity("unexpected refresh receipt")),
                }
            }
        }
    }
    pub fn start(
        &mut self,
        catalog: &mut Catalog,
        config: &Config,
        worker: &mut WorkerSupervisor,
        job: JobId,
        request: crate::edit::RefreshRequest,
    ) -> AppResult<()> {
        let group = *request
            .groups
            .first()
            .ok_or_else(|| AppError::invalid("refresh requires a group"))?;
        let session = request.application_session;
        let operation = request.operation_id;
        if self.active.is_some() || !worker.available()? {
            return Ok(());
        }
        catalog
            .db
            .execute("DELETE FROM reservations WHERE job=?", [job.to_string()])?;
        let mut q=catalog.db.prepare("SELECT p.committed,l.version FROM projections p JOIN sidecars s ON s.id=p.sidecar LEFT JOIN local_heads l ON l.sidecar=s.id WHERE s.group_id=? AND p.pending=1 AND p.committed IS NOT NULL ORDER BY p.sidecar LIMIT 128")?;
        let plans = q
            .query_map([group.to_string()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);
        let (mut local_bytes, mut state_bytes) = (0u64, 0u64);
        for (encoded, previous) in plans {
            let plan: crate::archive::ProjectionPlan = serde_json::from_str(&encoded)?;
            if !crate::archive::owns_projection(&plan.owner, &plan.path) {
                continue;
            }
            let incoming = match plan.body {
                VersionBody::Content { size, .. } => size,
                VersionBody::Reset => 0,
            };
            let prior = match previous {
                Some(id) => match catalog.version(serde_json::from_value(id.into())?)?.body {
                    VersionBody::Content { size, .. } => size,
                    VersionBody::Reset => 0,
                },
                None => 0,
            };
            local_bytes = local_bytes
                .checked_add(incoming)
                .ok_or_else(|| AppError::invalid("refresh size overflow"))?;
            state_bytes = state_bytes
                .checked_add(prior.max(incoming))
                .ok_or_else(|| AppError::invalid("refresh size overflow"))?;
        }
        let local = RootHandle::open(&config.working_root, RootPolicy::Local)?;
        let state = RootHandle::open(&config.state_root, RootPolicy::Local)?;
        let measurement = catalog.admission_measurement(&local, &state)?;
        let required = local_bytes
            .checked_add(state_bytes)
            .and_then(|n| n.checked_add(64 * 1024))
            .ok_or_else(|| AppError::invalid("refresh reservation overflow"))?;
        let available = measurement
            .filesystem_free
            .min(measurement.state_filesystem_free);
        if !matches!(
            admit_with_budget_reservations(
                &AdmissionInput {
                    budget: config.budget,
                    occupied: measurement
                        .local
                        .total
                        .checked_add(measurement.state.total)
                        .ok_or_else(|| AppError::invalid("occupancy overflow"))?,
                    reserved: catalog.total_remaining_reservations()?,
                    available,
                    reserve: config.reserve,
                    required,
                    existing: 0,
                    partial: 0,
                    overhead: 0,
                    nas_available: false,
                    allow_direct: false
                },
                catalog.total_remaining_reservations()?
            ),
            AdmissionDecision::Local { .. }
        ) {
            return Err(AppError::new(
                ErrorCode::Capacity,
                "refresh waits for capacity; staged metadata and live bytes retained",
            ));
        }
        let reservations = vec![
            catalog.reserve(job, FilesystemId(local.device()), local_bytes)?,
            catalog.reserve(job, FilesystemId(state.device()), state_bytes + 64 * 1024)?,
        ];
        match worker.submit(NasCommand::ApplyRefresh {
            local: config.working_root.clone(),
            state: config.state_root.clone(),
            job,
            group,
            session,
            operation,
        }) {
            Ok(ticket) => {
                self.active = Some(Active {
                    job,
                    group,
                    ticket,
                    reservations,
                });
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}
