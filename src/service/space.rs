use crate::{
    config::Config, domain::*, error::AppResult, fs::ManagedSpaceMeasurement, transfer::*,
};
#[derive(Default)]
pub(crate) struct SpaceMeter {
    pub observed_at: Option<Timestamp>,
    pub measurement: Option<ManagedSpaceMeasurement>,
    pub reason: Option<String>,
    active: Option<(WorkerTicket, crate::catalog::admission::AdmissionCheckpoint)>,
    next_attempt: Timestamp,
    retry_after: Timestamp,
}
impl SpaceMeter {
    pub fn advance(
        &mut self,
        config: &Config,
        catalog: &crate::catalog::Catalog,
        worker: &mut WorkerSupervisor,
        now: Timestamp,
    ) -> AppResult<()> {
        if let Some((ticket, checkpoint)) = self.active.take() {
            let mut succeeded = false;
            match worker.poll(ticket)? {
                WorkerState::Reaped => {
                    if self.reason.is_none() {
                        catalog.invalidate_admission()?;
                    }
                    self.active = Some((ticket, checkpoint));
                    self.reason=Some("space worker disappeared; process quiescence must be proved before replacement".into());
                    return Ok(());
                }
                WorkerState::Active | WorkerState::TimedOutStillAlive => {
                    self.active = Some((ticket, checkpoint));
                    return Ok(());
                }
                WorkerState::Completed(NasReply::LocalSpace(measurement)) => {
                    catalog.accept_admission_measurement(checkpoint, measurement.clone())?;
                    self.measurement = Some(measurement);
                    self.observed_at = Some(now);
                    self.reason = None;
                    succeeded = true;
                }
                WorkerState::Failed(error) => {
                    catalog.invalidate_admission()?;
                    self.reason = Some(error.to_string());
                }
                WorkerState::Completed(_) => {
                    catalog.invalidate_admission()?;
                    self.reason = Some("space worker returned an unexpected receipt".into())
                }
            }
            self.next_attempt = now + config.reconcile_seconds;
            self.retry_after = if succeeded { now } else { now + 5 };
            return Ok(());
        }
        if now >= self.retry_after
            && (catalog.needs_admission_measurement() || now >= self.next_attempt)
            && worker.available()?
        {
            let checkpoint = catalog.admission_checkpoint()?;
            self.active = Some((
                worker.submit(NasCommand::MeasureLocalSpace {
                    local: config.working_root.clone(),
                    state: config.state_root.clone(),
                })?,
                checkpoint,
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog::Catalog, fs::*};

    #[test]
    fn failed_reconciliation_blocks_admission_and_backs_off_retries() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.enable_async_admission();
        let checkpoint = catalog.admission_checkpoint().unwrap();
        catalog
            .accept_admission_measurement(checkpoint, measure_managed_space(&root, &root).unwrap())
            .unwrap();
        assert!(catalog.admission_measurement(&root, &root).is_ok());
        catalog.request_admission_measurement();
        let shell = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("bash"))
            .find(|path| path.is_file())
            .unwrap();
        let executable = temp.path().join("failing-worker");
        std::fs::write(&executable, format!("#!{}\nwhile IFS= read -r line; do\n IFS='\"' read -r a b c ticket rest <<< \"$line\"\n printf '{{\"ticket\":\"%s\",\"result\":{{\"Err\":{{\"code\":\"InvalidConfig\",\"message\":\"unknown managed object\",\"retry\":\"Correction\"}}}}}}\\n' \"$ticket\"\ndone\n", shell.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut worker = WorkerSupervisor::new_local(executable, vec![]).unwrap();
        let config = Config::new(
            temp.path().into(),
            temp.path().into(),
            temp.path().join("archive"),
        );
        let mut meter = SpaceMeter::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while meter.reason.is_none() {
            meter.advance(&config, &catalog, &mut worker, 100).unwrap();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            meter
                .reason
                .as_ref()
                .unwrap()
                .contains("unknown managed object")
        );
        assert!(catalog.admission_measurement(&root, &root).is_err());
        let previous = worker.latest_ticket();
        meter.advance(&config, &catalog, &mut worker, 101).unwrap();
        assert_eq!(worker.latest_ticket(), previous);
        meter.advance(&config, &catalog, &mut worker, 105).unwrap();
        assert_ne!(worker.latest_ticket(), previous);
    }

    #[test]
    fn disappeared_worker_invalidates_admission_once() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.enable_async_admission();
        let checkpoint = catalog.admission_checkpoint().unwrap();
        catalog
            .accept_admission_measurement(checkpoint, measure_managed_space(&root, &root).unwrap())
            .unwrap();
        let shell = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("bash"))
            .find(|path| path.is_file())
            .unwrap();
        let executable = temp.path().join("disappearing-worker");
        std::fs::write(
            &executable,
            format!("#!{}\nIFS= read -r line\nexit 0\n", shell.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut worker = WorkerSupervisor::new_local(executable, vec![]).unwrap();
        let config = Config::new(
            temp.path().into(),
            temp.path().into(),
            temp.path().join("archive"),
        );
        let mut meter = SpaceMeter::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while meter.reason.is_none() {
            meter.advance(&config, &catalog, &mut worker, 100).unwrap();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            meter
                .reason
                .as_ref()
                .unwrap()
                .contains("space worker disappeared")
        );
        assert!(catalog.admission_measurement(&root, &root).is_err());
        let generation: u64 = catalog
            .db
            .query_row("SELECT value FROM admission_generation", [], |row| {
                row.get(0)
            })
            .unwrap();
        for _ in 0..3 {
            meter.advance(&config, &catalog, &mut worker, 101).unwrap();
        }
        let repeated: u64 = catalog
            .db
            .query_row("SELECT value FROM admission_generation", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(generation, repeated);
    }
}
