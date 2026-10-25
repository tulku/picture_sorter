use super::*;
impl Catalog {
    pub(crate) fn enable_async_admission(&mut self) {
        self.async_admission = true;
        self.admission_requested.set(true);
    }
    pub(crate) fn admission_epoch(&self) -> AppResult<u64> {
        Ok(self
            .db
            .query_row("SELECT value FROM admission_epoch", [], |r| r.get(0))?)
    }
    pub(crate) fn needs_admission_measurement(&self) -> bool {
        self.admission_requested.get()
    }
    pub(crate) fn accept_admission_measurement(
        &self,
        epoch: u64,
        measurement: crate::fs::ManagedSpaceMeasurement,
    ) -> AppResult<()> {
        if self.admission_epoch()? == epoch {
            *self.admission_proof.borrow_mut() = Some((epoch, measurement));
            self.admission_requested.set(false);
            self.admission_observed.set(Some(std::time::Instant::now()));
        }
        Ok(())
    }
    pub(crate) fn admission_measurement(
        &self,
        local: &RootHandle,
        state: &RootHandle,
    ) -> AppResult<crate::fs::ManagedSpaceMeasurement> {
        if !self.async_admission {
            return crate::fs::measure_managed_space(local, state);
        }
        if let Some((epoch, measurement)) = self.admission_proof.borrow().as_ref()
            && *epoch == self.admission_epoch()?
            && self
                .admission_observed
                .get()
                .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(1))
        {
            return Ok(measurement.clone());
        }
        self.admission_requested.set(true);
        Err(AppError::new(
            ErrorCode::Capacity,
            "fresh worker capacity measurement pending; bytes retained",
        ))
    }
    pub(crate) fn set_admission_limits(&mut self, budget: u64, reserve: u64) {
        self.admission_limits = Some((budget, reserve));
    }
    pub(crate) fn admit_inline_snapshot(&self, root: &RootHandle, size: u64) -> AppResult<()> {
        let Some((budget, reserve)) = self.admission_limits else {
            return Ok(());
        };
        use crate::storage::*;
        let measurement = self.admission_measurement(root, root)?;
        let required = size
            .checked_add(64 * 1024)
            .ok_or_else(|| AppError::invalid("snapshot reservation overflow"))?;
        if !matches!(
            admit_with_budget_reservations(
                &AdmissionInput {
                    budget,
                    occupied: measurement
                        .local
                        .total
                        .checked_add(measurement.state.total)
                        .ok_or_else(|| AppError::invalid("occupancy overflow"))?,
                    reserved: self.remaining_reservations(FilesystemId(root.device()))?,
                    available: measurement.state_filesystem_free,
                    reserve,
                    required,
                    existing: 0,
                    partial: 0,
                    overhead: 0,
                    nas_available: false,
                    allow_direct: false
                },
                self.total_remaining_reservations()?
            ),
            AdmissionDecision::Local { .. }
        ) {
            return Err(AppError::new(
                ErrorCode::Capacity,
                "derived metadata waits for capacity; live bytes retained",
            ));
        }
        self.db
            .execute("UPDATE admission_epoch SET value=value+1", [])?;
        self.admission_requested.set(true);
        Ok(())
    }
    pub(crate) fn invalidate_admission(&self) -> AppResult<()> {
        self.db
            .execute("UPDATE admission_epoch SET value=value+1", [])?;
        self.admission_requested.set(true);
        Ok(())
    }
    pub(crate) fn reservation_charge(&self, id: crate::domain::ReservationId) -> AppResult<u64> {
        Ok(self.db.query_row("SELECT CASE WHEN ? AND r.remaining>0 THEN c.bytes ELSE r.remaining END FROM reservations r JOIN reservation_ceilings c ON c.id=r.id WHERE r.id=?",rusqlite::params![self.async_admission,id.to_string()],|r|r.get(0))?)
    }
    pub(crate) fn conservative_reservations(
        &self,
        filesystem: Option<crate::storage::FilesystemId>,
    ) -> AppResult<u64> {
        let mut q=self.db.prepare("SELECT c.bytes FROM reservations r JOIN reservation_ceilings c ON c.id=r.id WHERE r.remaining>0 AND (? IS NULL OR r.filesystem=?)")?;
        let fs = filesystem.map(|fs| fs.0.to_string());
        let mut sum = 0u64;
        for bytes in q.query_map(rusqlite::params![fs, fs], |r| r.get::<_, u64>(0))? {
            sum = sum
                .checked_add(bytes?)
                .ok_or_else(|| AppError::invalid("reservation overflow"))?;
        }
        Ok(sum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{domain::*, fs::*, storage::FilesystemId};
    #[test]
    fn asynchronous_admission_reuses_nonallocating_checks_and_rejects_reservation_stale_measurements()
     {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.enable_async_admission();
        assert!(catalog.admission_measurement(&root, &root).is_err());
        let epoch = catalog.admission_epoch().unwrap();
        let measured = measure_managed_space(&root, &root).unwrap();
        catalog
            .accept_admission_measurement(epoch, measured.clone())
            .unwrap();
        assert!(catalog.admission_measurement(&root, &root).is_ok());
        assert!(catalog.admission_measurement(&root, &root).is_ok());
        catalog
            .accept_admission_measurement(epoch, measured.clone())
            .unwrap();
        let job = catalog
            .enqueue(crate::catalog::DurableRequest {
                operation_id: OperationId::new(),
                kind: crate::catalog::JobKind::Clean,
            })
            .unwrap();
        let reservation = catalog
            .reserve(job, FilesystemId(root.device()), 10)
            .unwrap();
        catalog.update_reservation(reservation, 6).unwrap();
        assert_eq!(catalog.total_remaining_reservations().unwrap(), 10);
        assert_eq!(catalog.reservation_charge(reservation).unwrap(), 10);
        assert!(catalog.admission_measurement(&root, &root).is_err());
        catalog
            .accept_admission_measurement(epoch, measured)
            .unwrap();
        assert!(catalog.admission_measurement(&root, &root).is_err());
    }
}
