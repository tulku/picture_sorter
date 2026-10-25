use std::path::PathBuf;
pub struct TestRig {
    pub _temp: tempfile::TempDir,
    pub local: PathBuf,
    pub state: PathBuf,
    pub archive: PathBuf,
}
impl TestRig {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("local");
        let state = temp.path().join("state");
        let archive = temp.path().join("archive");
        for root in [&local, &state, &archive] {
            std::fs::create_dir(root).unwrap();
        }
        Self {
            _temp: temp,
            local,
            state,
            archive,
        }
    }
}
