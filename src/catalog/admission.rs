use super::*;
use std::collections::BTreeMap;

pub(crate) const ADMISSION_PENDING: &str = "worker capacity reconciliation pending; bytes retained";

#[derive(Clone)]
pub(crate) struct AdmissionCheckpoint {
    epoch: u64,
    allocations: BTreeMap<u64, u64>,
    outstanding: BTreeMap<u64, u64>,
}

impl Catalog {
    pub(crate) fn enable_async_admission(&mut self) {
        self.async_admission = true;
        self.admission_requested.set(true);
    }
    pub(crate) fn admission_checkpoint(&self) -> AppResult<AdmissionCheckpoint> {
        Ok(AdmissionCheckpoint {
            epoch: self
                .db
                .query_row("SELECT value FROM admission_generation", [], |r| r.get(0))?,
            allocations: self.admission_allocations()?,
            outstanding: self.admission_outstanding()?,
        })
    }
    fn admission_allocations(&self) -> AppResult<BTreeMap<u64, u64>> {
        let mut query = self
            .db
            .prepare("SELECT filesystem,bytes FROM admission_allocations")?;
        let rows = query.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })?;
        let mut allocations = BTreeMap::new();
        for row in rows {
            let (filesystem, bytes) = row?;
            allocations.insert(
                filesystem
                    .parse()
                    .map_err(|_| AppError::integrity("invalid reservation filesystem"))?,
                bytes,
            );
        }
        Ok(allocations)
    }
    fn admission_outstanding(&self) -> AppResult<BTreeMap<u64, u64>> {
        let mut query = self
            .db
            .prepare("SELECT filesystem,remaining FROM reservations WHERE remaining>0")?;
        let rows = query.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })?;
        let mut outstanding = BTreeMap::<u64, u64>::new();
        for row in rows {
            let (filesystem, bytes) = row?;
            let bytes_so_far = outstanding
                .entry(
                    filesystem
                        .parse()
                        .map_err(|_| AppError::integrity("invalid reservation filesystem"))?,
                )
                .or_default();
            *bytes_so_far = bytes_so_far
                .checked_add(bytes)
                .ok_or_else(|| AppError::invalid("reservation overflow"))?;
        }
        Ok(outstanding)
    }
    fn admission_catalog_bytes(&self) -> AppResult<u64> {
        let mut bytes = 0u64;
        for name in ["catalog.sqlite", "catalog.sqlite-wal", "catalog.sqlite-shm"] {
            match std::fs::symlink_metadata(format!(
                "/proc/self/fd/{}/{name}",
                self._root.as_raw_fd()
            )) {
                Ok(metadata) if metadata.is_file() => {
                    bytes = bytes
                        .checked_add(metadata.len())
                        .ok_or_else(|| AppError::invalid("catalog size overflow"))?;
                }
                Ok(_) => return Err(AppError::integrity("catalog file changed type")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(bytes)
    }
    pub(crate) fn needs_admission_measurement(&self) -> bool {
        self.admission_requested.get()
    }
    pub(crate) fn request_admission_measurement(&self) {
        self.admission_requested.set(true);
    }
    pub(crate) fn accept_admission_measurement(
        &self,
        checkpoint: AdmissionCheckpoint,
        measurement: crate::fs::ManagedSpaceMeasurement,
    ) -> AppResult<()> {
        let epoch: u64 = self
            .db
            .query_row("SELECT value FROM admission_generation", [], |r| r.get(0))?;
        if epoch == checkpoint.epoch {
            *self.admission_proof.borrow_mut() = Some((checkpoint, measurement));
            self.admission_requested.set(false);
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
        if let Some((checkpoint, baseline)) = self.admission_proof.borrow().as_ref() {
            let allocations = self.admission_allocations()?;
            let outstanding = self.admission_outstanding()?;
            let mut measurement = baseline.clone();
            for filesystem in allocations
                .keys()
                .chain(checkpoint.outstanding.keys())
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
            {
                let grants = allocations
                    .get(&filesystem)
                    .copied()
                    .unwrap_or(0)
                    .checked_sub(
                        checkpoint
                            .allocations
                            .get(&filesystem)
                            .copied()
                            .unwrap_or(0),
                    )
                    .and_then(|n| {
                        n.checked_add(
                            checkpoint
                                .outstanding
                                .get(&filesystem)
                                .copied()
                                .unwrap_or(0),
                        )
                    })
                    .ok_or_else(|| AppError::integrity("allocation accounting overflow"))?;
                // Live reservations charge unwritten bytes separately; consumed grants remain occupied.
                let consumed = grants
                    .checked_sub(outstanding.get(&filesystem).copied().unwrap_or(0))
                    .ok_or_else(|| AppError::integrity("allocation accounting mismatch"))?;
                let usage = if filesystem == state.device() {
                    &mut measurement.state
                } else {
                    &mut measurement.local
                };
                usage.total = usage
                    .total
                    .checked_add(consumed)
                    .ok_or_else(|| AppError::invalid("occupancy overflow"))?;
                usage.other = usage
                    .other
                    .checked_add(consumed)
                    .ok_or_else(|| AppError::invalid("occupancy overflow"))?;
            }
            let catalog_bytes = self.admission_catalog_bytes()?;
            measurement.state.total = measurement
                .state
                .total
                .checked_sub(measurement.state.catalog)
                .and_then(|n| n.checked_add(catalog_bytes))
                .ok_or_else(|| AppError::invalid("catalog size overflow"))?;
            measurement.state.catalog = catalog_bytes;
            measurement.filesystem_free = crate::fs::available_bytes(local)?;
            measurement.state_filesystem_free = crate::fs::available_bytes(state)?;
            return Ok(measurement);
        }
        self.admission_requested.set(true);
        Err(AppError::new(ErrorCode::Capacity, ADMISSION_PENDING))
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
                    allow_direct: false,
                },
                self.total_remaining_reservations()?
            ),
            AdmissionDecision::Local { .. }
        ) {
            self.request_admission_measurement();
            return Err(AppError::new(
                ErrorCode::Capacity,
                "derived metadata waits for capacity; live bytes retained",
            ));
        }
        self.db.execute("INSERT INTO admission_allocations VALUES(?,?) ON CONFLICT(filesystem) DO UPDATE SET bytes=bytes+excluded.bytes", rusqlite::params![root.device().to_string(), required])?;
        Ok(())
    }
    pub(crate) fn invalidate_admission(&self) -> AppResult<()> {
        self.db
            .execute("UPDATE admission_generation SET value=value+1", [])?;
        *self.admission_proof.borrow_mut() = None;
        self.admission_requested.set(true);
        Ok(())
    }
    pub(crate) fn reservation_charge(&self, id: crate::domain::ReservationId) -> AppResult<u64> {
        Ok(self.db.query_row(
            "SELECT remaining FROM reservations WHERE id=?",
            rusqlite::params![id.to_string()],
            |r| r.get(0),
        )?)
    }
    pub(crate) fn conservative_reservations(
        &self,
        filesystem: Option<crate::storage::FilesystemId>,
    ) -> AppResult<u64> {
        let mut q = self.db.prepare(
            "SELECT remaining FROM reservations WHERE remaining>0 AND (? IS NULL OR filesystem=?)",
        )?;
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
    use crate::{domain::*, fs::*, storage::*};

    fn job(catalog: &mut Catalog) -> JobId {
        catalog
            .enqueue(DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::Clean,
            })
            .unwrap()
    }

    #[test]
    fn asynchronous_admission_reuses_measurement_past_one_second_without_rescanning() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.enable_async_admission();
        assert!(catalog.admission_measurement(&root, &root).is_err());
        let checkpoint = catalog.admission_checkpoint().unwrap();
        let measured = measure_managed_space(&root, &root).unwrap();
        catalog
            .accept_admission_measurement(checkpoint, measured)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::os::unix::fs::symlink("missing", temp.path().join("not-scanned")).unwrap();
        assert!(catalog.admission_measurement(&root, &root).is_ok());
        assert!(!catalog.needs_admission_measurement());
    }

    #[test]
    fn allocations_during_reconciliation_remain_charged_after_release() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.enable_async_admission();
        let checkpoint = catalog.admission_checkpoint().unwrap();
        let measured = measure_managed_space(&root, &root).unwrap();
        let baseline = measured.local.total + measured.state.total;
        let job = job(&mut catalog);
        let reservation = catalog
            .reserve(job, FilesystemId(root.device()), 1_000_000)
            .unwrap();
        catalog.update_reservation(reservation, 500_000).unwrap();
        catalog
            .accept_admission_measurement(checkpoint, measured)
            .unwrap();
        let measurement = catalog.admission_measurement(&root, &root).unwrap();
        assert!(
            measurement.local.total
                + measurement.state.total
                + catalog.total_remaining_reservations().unwrap()
                >= baseline + 1_000_000
        );
        catalog.update_reservation(reservation, 0).unwrap();
        let measurement = catalog.admission_measurement(&root, &root).unwrap();
        assert!(measurement.local.total + measurement.state.total >= baseline + 1_000_000);
        catalog
            .db
            .execute(
                "DELETE FROM reservations WHERE id=?",
                [reservation.to_string()],
            )
            .unwrap();
        let measurement = catalog.admission_measurement(&root, &root).unwrap();
        assert!(measurement.local.total + measurement.state.total >= baseline + 1_000_000);
    }

    #[test]
    fn tracked_consumption_enforces_budget_until_reconciliation() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.enable_async_admission();
        let checkpoint = catalog.admission_checkpoint().unwrap();
        let measured = measure_managed_space(&root, &root).unwrap();
        let baseline = measured.local.total + measured.state.total;
        catalog
            .accept_admission_measurement(checkpoint, measured)
            .unwrap();
        let job = job(&mut catalog);
        let reservation = catalog
            .reserve(job, FilesystemId(root.device()), 1_000_000)
            .unwrap();
        catalog.update_reservation(reservation, 0).unwrap();
        let measurement = catalog.admission_measurement(&root, &root).unwrap();
        assert_eq!(
            admit(&AdmissionInput {
                budget: baseline + 1_000_000,
                occupied: measurement.local.total + measurement.state.total,
                reserved: 0,
                available: u64::MAX,
                reserve: 0,
                required: 1,
                existing: 0,
                partial: 0,
                overhead: 0,
                nas_available: false,
                allow_direct: false
            }),
            AdmissionDecision::WaitingCapacity
        );
        let checkpoint = catalog.admission_checkpoint().unwrap();
        catalog
            .accept_admission_measurement(checkpoint, measure_managed_space(&root, &root).unwrap())
            .unwrap();
        let measurement = catalog.admission_measurement(&root, &root).unwrap();
        assert!(measurement.local.total + measurement.state.total < baseline + 1_000_000);
    }

    #[test]
    fn reservation_growth_and_reactivation_charge_only_new_allocations() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.enable_async_admission();
        let checkpoint = catalog.admission_checkpoint().unwrap();
        let baseline = measure_managed_space(&root, &root).unwrap();
        let original = baseline.local.total + baseline.state.total - baseline.state.catalog;
        catalog
            .accept_admission_measurement(checkpoint, baseline)
            .unwrap();
        let job = job(&mut catalog);
        let reservation = catalog
            .reserve(job, FilesystemId(root.device()), 1_000_000)
            .unwrap();
        catalog.update_reservation(reservation, 500_000).unwrap();
        assert_eq!(catalog.reservation_charge(reservation).unwrap(), 500_000);
        catalog.update_reservation(reservation, 750_000).unwrap();
        catalog.update_reservation(reservation, 0).unwrap();
        let measurement = catalog.admission_measurement(&root, &root).unwrap();
        assert_eq!(
            measurement.local.total + measurement.state.total - measurement.state.catalog,
            original + 1_250_000
        );
        let checkpoint = catalog.admission_checkpoint().unwrap();
        let baseline = measure_managed_space(&root, &root).unwrap();
        let original = baseline.local.total + baseline.state.total - baseline.state.catalog;
        catalog
            .accept_admission_measurement(checkpoint, baseline)
            .unwrap();
        catalog.update_reservation(reservation, 300_000).unwrap();
        let measurement = catalog.admission_measurement(&root, &root).unwrap();
        assert_eq!(
            measurement.local.total + measurement.state.total - measurement.state.catalog
                + catalog.total_remaining_reservations().unwrap(),
            original + 300_000
        );
        catalog.update_reservation(reservation, 0).unwrap();
        let measurement = catalog.admission_measurement(&root, &root).unwrap();
        assert_eq!(
            measurement.local.total + measurement.state.total - measurement.state.catalog,
            original + 300_000
        );
    }

    #[test]
    fn reconciliation_preserves_allocations_outstanding_when_scan_started() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.enable_async_admission();
        let job = job(&mut catalog);
        let reservation = catalog
            .reserve(job, FilesystemId(root.device()), 1_000_000)
            .unwrap();
        let checkpoint = catalog.admission_checkpoint().unwrap();
        let baseline = measure_managed_space(&root, &root).unwrap();
        let original = baseline.local.total + baseline.state.total - baseline.state.catalog;
        catalog.update_reservation(reservation, 0).unwrap();
        catalog
            .accept_admission_measurement(checkpoint, baseline)
            .unwrap();
        let measurement = catalog.admission_measurement(&root, &root).unwrap();
        assert_eq!(
            measurement.local.total + measurement.state.total - measurement.state.catalog,
            original + 1_000_000
        );
    }

    #[test]
    fn admission_refreshes_filesystem_free_bytes_without_tree_reconciliation() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.enable_async_admission();
        let checkpoint = catalog.admission_checkpoint().unwrap();
        let mut baseline = measure_managed_space(&root, &root).unwrap();
        baseline.filesystem_free = 0;
        baseline.state_filesystem_free = 0;
        catalog
            .accept_admission_measurement(checkpoint, baseline)
            .unwrap();
        let measurement = catalog.admission_measurement(&root, &root).unwrap();
        assert!(measurement.filesystem_free > 0);
        assert!(measurement.state_filesystem_free > 0);
    }

    #[test]
    fn inline_metadata_accounts_working_grants_on_a_separate_filesystem() {
        let working = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir_in("/dev/shm").unwrap();
        let local_root = RootHandle::open(working.path(), RootPolicy::Local).unwrap();
        let state_root = RootHandle::open(state.path(), RootPolicy::Local).unwrap();
        assert_ne!(local_root.device(), state_root.device());
        let mut catalog = Catalog::open(&state_root).unwrap();
        catalog.enable_async_admission();
        catalog.set_admission_limits(10_000_000, 0);
        let checkpoint = catalog.admission_checkpoint().unwrap();
        catalog
            .accept_admission_measurement(
                checkpoint,
                measure_managed_space(&local_root, &state_root).unwrap(),
            )
            .unwrap();
        let job = job(&mut catalog);
        let reservation = catalog
            .reserve(job, FilesystemId(local_root.device()), 1_000_000)
            .unwrap();
        catalog.update_reservation(reservation, 0).unwrap();
        catalog.admit_inline_snapshot(&state_root, 100).unwrap();
        let measurement = catalog
            .admission_measurement(&local_root, &state_root)
            .unwrap();
        assert!(measurement.local.total >= 1_000_000);
        assert!(measurement.state.total >= 65_636);
    }

    #[test]
    fn inline_capacity_wait_requests_reconciliation_without_discarding_proof() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.enable_async_admission();
        catalog.set_admission_limits(1, 0);
        let checkpoint = catalog.admission_checkpoint().unwrap();
        catalog
            .accept_admission_measurement(checkpoint, measure_managed_space(&root, &root).unwrap())
            .unwrap();
        assert!(catalog.admit_inline_snapshot(&root, 100).is_err());
        assert!(catalog.needs_admission_measurement());
        assert!(catalog.admission_measurement(&root, &root).is_ok());
    }

    #[test]
    fn invalidation_rejects_inflight_measurement_and_restart_needs_new_proof() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.enable_async_admission();
        let checkpoint = catalog.admission_checkpoint().unwrap();
        let baseline = measure_managed_space(&root, &root).unwrap();
        catalog.invalidate_admission().unwrap();
        catalog
            .accept_admission_measurement(checkpoint, baseline)
            .unwrap();
        assert!(catalog.admission_measurement(&root, &root).is_err());
        let checkpoint = catalog.admission_checkpoint().unwrap();
        catalog
            .accept_admission_measurement(checkpoint, measure_managed_space(&root, &root).unwrap())
            .unwrap();
        assert!(catalog.admission_measurement(&root, &root).is_ok());
        drop(catalog);
        let mut catalog = Catalog::open(&root).unwrap();
        catalog.enable_async_admission();
        assert!(catalog.admission_measurement(&root, &root).is_err());
    }

    #[test]
    fn catalog_wal_checkpoint_and_regrowth_are_measured_without_rescanning() {
        let working = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let local_root = RootHandle::open(working.path(), RootPolicy::Local).unwrap();
        let state_root = RootHandle::open(state.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&state_root).unwrap();
        catalog.enable_async_admission();
        catalog
            .db
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        for _ in 0..20 {
            job(&mut catalog);
        }
        let checkpoint = catalog.admission_checkpoint().unwrap();
        catalog
            .db
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        let baseline = measure_managed_space(&local_root, &state_root).unwrap();
        let baseline_catalog = baseline.state.catalog;
        catalog
            .accept_admission_measurement(checkpoint, baseline)
            .unwrap();
        job(&mut catalog);
        std::os::unix::fs::symlink("missing", state.path().join("not-scanned")).unwrap();
        let measurement = catalog
            .admission_measurement(&local_root, &state_root)
            .unwrap();
        let actual_bytes: u64 = ["catalog.sqlite", "catalog.sqlite-wal", "catalog.sqlite-shm"]
            .into_iter()
            .map(|name| std::fs::metadata(state.path().join(name)).unwrap().len())
            .sum();
        assert!(actual_bytes > baseline_catalog);
        assert_eq!(measurement.state.catalog, actual_bytes);
        assert_eq!(measurement.state.total, actual_bytes);
    }

    #[test]
    fn fresh_inline_version_books_one_allocation_and_replay_books_none() {
        use crate::edit::{SnapshotIntent, SnapshotPayload, VersionStore};
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let group = catalog.create_group(false).unwrap();
        let sidecar = SidecarId::new();
        catalog
            .register_sidecar(
                sidecar,
                Some(group),
                ArchivePath::parse("test.ORF.rrdata").unwrap(),
                "rapidraw-base",
            )
            .unwrap();
        catalog.enable_async_admission();
        catalog.set_admission_limits(10_000_000, 0);
        let checkpoint = catalog.admission_checkpoint().unwrap();
        catalog
            .accept_admission_measurement(checkpoint, measure_managed_space(&root, &root).unwrap())
            .unwrap();
        let intent = SnapshotIntent {
            sidecar,
            parents: vec![],
            operation: OperationId::new(),
            origin: VersionOrigin::Observed,
            payload: SnapshotPayload::Content(vec![1, 2, 3]),
        };
        let version = VersionStore::new(&mut catalog, &root)
            .record(intent.clone())
            .unwrap();
        assert_eq!(
            catalog.admission_allocations().unwrap().get(&root.device()),
            Some(&65_539)
        );
        assert_eq!(
            VersionStore::new(&mut catalog, &root)
                .record(intent)
                .unwrap(),
            version
        );
        assert_eq!(
            catalog.admission_allocations().unwrap().get(&root.device()),
            Some(&65_539)
        );
    }
}
