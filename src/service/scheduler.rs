use crate::{config::Config, domain::*, error::AppResult};
use serde::Serialize;
use std::collections::HashMap;
#[derive(Debug, Clone)]
pub enum ServiceEvent {
    Command(OperationId),
    LocalNotification,
    WorkerCompletion,
    Connectivity(bool),
    Failed(JobId),
    Progress(JobId),
}
#[derive(Debug, Default, Serialize)]
pub struct TickReport {
    pub reconcile: bool,
    pub poll_ratings: bool,
    pub observe_edits: bool,
    pub activity_hold_seconds: u64,
}
pub struct Scheduler {
    debounce: u64,
    poll: u64,
    reconcile: u64,
    hold: u64,
    last_observe: Option<Timestamp>,
    last_poll: Option<Timestamp>,
    last_reconcile: Option<Timestamp>,
    now: Timestamp,
    retries: HashMap<JobId, (u32, Timestamp)>,
}
impl Scheduler {
    pub fn new(config: &Config) -> Self {
        Self {
            debounce: config.debounce_seconds,
            poll: config.poll_seconds,
            reconcile: config.reconcile_seconds,
            hold: config.activity_hold_seconds,
            last_observe: None,
            last_poll: None,
            last_reconcile: None,
            now: 0,
            retries: HashMap::new(),
        }
    }
    pub fn tick(&mut self, now: Timestamp) -> AppResult<TickReport> {
        self.now = now;
        let mut report = TickReport {
            activity_hold_seconds: self.hold,
            ..Default::default()
        };
        if due(self.last_observe, now, self.debounce) {
            report.observe_edits = true;
            self.last_observe = Some(now);
        }
        if due(self.last_poll, now, self.poll) {
            report.poll_ratings = true;
            self.last_poll = Some(now);
        }
        if due(self.last_reconcile, now, self.reconcile) {
            report.reconcile = true;
            self.last_reconcile = Some(now);
        }
        Ok(report)
    }
    pub fn handle(&mut self, event: ServiceEvent) -> AppResult<()> {
        match event {
            ServiceEvent::Failed(job) => {
                let failures = self.retries.get(&job).map_or(0, |v| v.0).saturating_add(1);
                let delay = 1u64
                    .checked_shl(failures.saturating_sub(1).min(8))
                    .unwrap_or(256)
                    .min(300);
                self.retries
                    .insert(job, (failures, self.now.saturating_add(delay)));
            }
            ServiceEvent::Progress(job) => {
                self.retries.remove(&job);
            }
            ServiceEvent::LocalNotification => self.last_observe = None,
            ServiceEvent::Connectivity(true) => {
                self.last_poll = None;
            }
            _ => {}
        }
        Ok(())
    }
    pub fn may_retry(&self, job: JobId) -> bool {
        self.retries
            .get(&job)
            .is_none_or(|(_, when)| self.now >= *when)
    }
    pub fn retry_state(&self, job: JobId) -> Option<(u32, Timestamp)> {
        self.retries.get(&job).copied()
    }
    pub fn restore_retry(&mut self, job: JobId, failures: u32, at: Timestamp) {
        self.retries.insert(job, (failures, at));
    }
}
fn due(last: Option<Timestamp>, now: Timestamp, interval: u64) -> bool {
    last.is_none_or(|last| now < last || now.saturating_sub(last) >= interval)
}
