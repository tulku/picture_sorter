use crate::{
    config::Config, domain::*, error::AppResult, fs::ManagedSpaceMeasurement, transfer::*,
};
#[derive(Default)]
pub(crate) struct SpaceMeter {
    pub observed_at: Option<Timestamp>,
    pub measurement: Option<ManagedSpaceMeasurement>,
    pub reason: Option<String>,
    active: Option<(WorkerTicket, u64)>,
    next_attempt: Timestamp,
}
impl SpaceMeter {
    pub fn advance(
        &mut self,
        config: &Config,
        catalog: &crate::catalog::Catalog,
        worker: &mut WorkerSupervisor,
        now: Timestamp,
    ) -> AppResult<()> {
        if let Some((ticket, epoch)) = self.active.take() {
            match worker.poll(ticket)? {
                WorkerState::Reaped => {
                    self.active = Some((ticket, epoch));
                    self.reason=Some("space worker disappeared; process quiescence must be proved before replacement".into());
                    return Ok(());
                }
                WorkerState::Active | WorkerState::TimedOutStillAlive => {
                    self.active = Some((ticket, epoch));
                    return Ok(());
                }
                WorkerState::Completed(NasReply::LocalSpace(measurement)) => {
                    catalog.accept_admission_measurement(epoch, measurement.clone())?;
                    self.measurement = Some(measurement);
                    self.observed_at = Some(now);
                    self.reason = None;
                }
                WorkerState::Failed(error) => self.reason = Some(error.to_string()),
                WorkerState::Completed(_) => {
                    self.reason = Some("space worker returned an unexpected receipt".into())
                }
            }
            self.next_attempt = now + config.reconcile_seconds.min(5);
            return Ok(());
        }
        if (catalog.needs_admission_measurement() || now >= self.next_attempt)
            && worker.available()?
        {
            let epoch = catalog.admission_epoch()?;
            self.active = Some((
                worker.submit(NasCommand::MeasureLocalSpace {
                    local: config.working_root.clone(),
                    state: config.state_root.clone(),
                })?,
                epoch,
            ));
        }
        Ok(())
    }
}
