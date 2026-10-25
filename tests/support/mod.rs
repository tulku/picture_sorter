#![allow(dead_code)]
pub mod faults;
use std::path::PathBuf;
pub struct TestRig {
    pub _temp: tempfile::TempDir,
    pub source: PathBuf,
    pub clock: photo_sorter::domain::ManualClock,
    pub faults: faults::Faults,
    pub local: PathBuf,
    pub state: PathBuf,
    pub archive: PathBuf,
}
impl TestRig {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let local = temp.path().join("local");
        let state = temp.path().join("state");
        let archive = temp.path().join("archive");
        for root in [&local, &state, &archive] {
            std::fs::create_dir(root).unwrap();
        }
        Self {
            _temp: temp,
            source,
            clock: Default::default(),
            faults: Default::default(),
            local,
            state,
            archive,
        }
    }
}
pub mod http;
