use serde::{Deserialize, Serialize};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};
static COUNTERS: [AtomicU64; 9] = [const { AtomicU64::new(0) }; 9];
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub streamed_hash_bytes: u64,
    pub exif_invocations: u64,
    pub transfer_bytes: u64,
    pub journal_reads: u64,
    pub hash_micros: u64,
    pub exif_micros: u64,
    pub transfer_micros: u64,
    pub journal_micros: u64,
    #[serde(default)]
    pub refresh_applications: u64,
}
impl Metrics {
    fn values(&self) -> [u64; 9] {
        [
            self.streamed_hash_bytes,
            self.exif_invocations,
            self.transfer_bytes,
            self.journal_reads,
            self.hash_micros,
            self.exif_micros,
            self.transfer_micros,
            self.journal_micros,
            self.refresh_applications,
        ]
    }
    fn from_values(v: [u64; 9]) -> Self {
        Self {
            streamed_hash_bytes: v[0],
            exif_invocations: v[1],
            transfer_bytes: v[2],
            journal_reads: v[3],
            hash_micros: v[4],
            exif_micros: v[5],
            transfer_micros: v[6],
            journal_micros: v[7],
            refresh_applications: v[8],
        }
    }
    pub fn difference(&self, before: &Self) -> Self {
        let a = self.values();
        let b = before.values();
        Self::from_values(std::array::from_fn(|n| a[n].saturating_sub(b[n])))
    }
    pub fn add(&mut self, other: &Self) {
        let a = self.values();
        let b = other.values();
        *self = Self::from_values(std::array::from_fn(|n| a[n].saturating_add(b[n])));
    }
}
pub fn snapshot() -> Metrics {
    Metrics::from_values(std::array::from_fn(|n| COUNTERS[n].load(Ordering::Relaxed)))
}
pub(crate) fn hash_bytes(bytes: u64) {
    COUNTERS[0].fetch_add(bytes, Ordering::Relaxed);
}
pub(crate) fn exif_invocation() {
    COUNTERS[1].fetch_add(1, Ordering::Relaxed);
}
pub(crate) fn transferred(bytes: u64) {
    COUNTERS[2].fetch_add(bytes, Ordering::Relaxed);
}
pub(crate) fn journal_read() {
    COUNTERS[3].fetch_add(1, Ordering::Relaxed);
}
pub(crate) enum Stage {
    Hash,
    Exif,
    Transfer,
    Journal,
}
pub(crate) struct Timer {
    index: usize,
    start: Instant,
}
impl Timer {
    pub fn start(stage: Stage) -> Self {
        Self {
            index: match stage {
                Stage::Hash => 4,
                Stage::Exif => 5,
                Stage::Transfer => 6,
                Stage::Journal => 7,
            },
            start: Instant::now(),
        }
    }
}
impl Drop for Timer {
    fn drop(&mut self) {
        COUNTERS[self.index].fetch_add(
            u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

pub(crate) fn refresh_application() {
    COUNTERS[8].fetch_add(1, Ordering::Relaxed);
}
