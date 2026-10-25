use super::*;
use crate::error::AppResult;
#[derive(Debug, Clone)]
pub struct FavoriteWrite {
    pub asset: uuid::Uuid,
    pub expected: bool,
    pub desired: bool,
    pub jpeg_checksum: String,
}
#[derive(Debug)]
pub enum FavoriteOutcome {
    Confirmed,
    Conflict { observed: bool },
    Uncertain,
}
pub enum FavoriteProgress {
    None,
    Pending,
    Finished(AppResult<FavoriteOutcome>),
}
