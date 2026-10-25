use photo_sorter::{
    domain::{FaultInjector, FaultPoint},
    error::{AppError, AppResult, ErrorCode},
};
use std::sync::Mutex;
#[derive(Default)]
pub struct Faults(pub Mutex<Option<FaultPoint>>);
impl FaultInjector for Faults {
    fn hit(&self, point: FaultPoint) -> AppResult<()> {
        let mut armed = self.0.lock().unwrap();
        if *armed == Some(point) {
            *armed = None;
            Err(AppError::new(ErrorCode::Io, "injected interruption"))
        } else {
            Ok(())
        }
    }
}
