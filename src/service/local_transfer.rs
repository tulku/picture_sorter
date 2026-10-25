use crate::{
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    import::*,
    transfer::*,
};
pub(crate) struct LocalTransferDriver {
    pub worker: WorkerSupervisor,
    active: Option<(LocalImportAttempt, WorkerTicket)>,
}
impl LocalTransferDriver {
    pub fn new(worker: WorkerSupervisor) -> Self {
        Self {
            worker,
            active: None,
        }
    }
    pub fn advance(&mut self, engine: &mut ImportCoordinator, job: JobId) -> AppResult<()> {
        if let Some((attempt, ticket)) = self.active.take() {
            if attempt.parent != job {
                self.active = Some((attempt, ticket));
                return Ok(());
            }
            engine.reconcile_local_reservation(&attempt)?;
            match self.worker.poll(ticket)? {
                WorkerState::Active | WorkerState::TimedOutStillAlive => {
                    self.active = Some((attempt, ticket))
                }
                WorkerState::Completed(NasReply::Staged(receipt)) => {
                    engine.ack_local_attempt(attempt, receipt)?;
                }
                WorkerState::Completed(_) => {
                    return Err(AppError::integrity(
                        "unexpected local transfer worker response",
                    ));
                }
                WorkerState::Failed(error) => engine.local_attempt_failed(&attempt, &error)?,
                WorkerState::Reaped => {
                    self.active = Some((attempt, ticket));
                    return Err(AppError::new(
                        ErrorCode::Unavailable,
                        "local transfer worker disappeared; partial and reservation retained until recovery proves quiescence",
                    ));
                }
            }
            return Ok(());
        }
        if !self.worker.available()? {
            return Ok(());
        }
        if let Some(attempt) = engine.prepare_local_attempt(job)? {
            let ticket = self.worker.submit(NasCommand::LocalTransfer {
                spec: attempt.spec.clone(),
                destination: attempt.destination.clone(),
                reuse_only: attempt.reuse_only,
            })?;
            self.active = Some((attempt, ticket));
        }
        Ok(())
    }
    pub fn active_parent(&self) -> Option<JobId> {
        self.active.as_ref().map(|(a, _)| a.parent)
    }
}
