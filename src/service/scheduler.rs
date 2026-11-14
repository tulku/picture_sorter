use crate::{config::Config, domain::*, error::AppResult};
use serde::Serialize;
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
        }
    }
    pub(crate) fn next_delay(
        &self,
        now: Timestamp,
        operations: &[Timestamp],
    ) -> std::time::Duration {
        let seconds = [
            (self.last_observe, self.debounce),
            (self.last_poll, self.poll),
            (self.last_reconcile, self.reconcile),
        ]
        .into_iter()
        .map(|(last, interval)| {
            last.map_or(0, |last| {
                last.saturating_add(interval.max(1)).saturating_sub(now)
            })
        })
        .chain(
            operations
                .iter()
                .filter(|deadline| **deadline > now)
                .map(|deadline| deadline.saturating_sub(now)),
        )
        .min()
        .unwrap_or(0);
        std::time::Duration::from_secs(seconds)
    }
    pub fn tick(&mut self, now: Timestamp) -> AppResult<TickReport> {
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
}
fn due(last: Option<Timestamp>, now: Timestamp, interval: u64) -> bool {
    last.is_none_or(|last| now < last || now.saturating_sub(last) >= interval)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operational_retry_precedes_periodic_observation() {
        let config = Config {
            debounce_seconds: 30,
            poll_seconds: 60,
            reconcile_seconds: 300,
            ..Config::default()
        };
        let mut scheduler = Scheduler::new(&config);
        scheduler.tick(100).unwrap();
        assert_eq!(
            scheduler.next_delay(100, &[102, 105]),
            std::time::Duration::from_secs(2)
        );
    }
}
