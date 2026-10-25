mod client;
mod mapping;
pub use client::*;
pub use mapping::*;
mod fetch;
pub use fetch::*;
mod ratings;
pub use ratings::*;
pub enum WriteProgress {
    None,
    Pending,
    Finished(crate::error::AppResult<WriteOutcome>),
}
pub trait ImmichAccess {
    fn origin(&self) -> &url::Url;
    fn asset(&self, id: uuid::Uuid) -> crate::error::AppResult<AssetInfo>;
    fn rating(&self, id: uuid::Uuid) -> crate::error::AppResult<crate::domain::RatingObservation> {
        ratings::remote_rating(&self.asset(id)?)
    }
    fn outbound_enabled(&self) -> bool;
    fn set_rating(&self, write: RatingWrite) -> crate::error::AppResult<WriteOutcome>;
    fn set_favorite(&self, _write: FavoriteWrite) -> crate::error::AppResult<FavoriteOutcome> {
        Err(crate::error::AppError::new(
            crate::error::ErrorCode::Unsupported,
            "favorite writes unavailable",
        ))
    }
    fn favorite_progress(&self, _asset: uuid::Uuid) -> FavoriteProgress {
        FavoriteProgress::None
    }
    fn write_progress(&self, _asset: uuid::Uuid) -> WriteProgress {
        WriteProgress::None
    }
}
impl ImmichAccess for ImmichClient {
    fn set_favorite(&self, write: FavoriteWrite) -> crate::error::AppResult<FavoriteOutcome> {
        self.set_favorite(write)
    }
    fn origin(&self) -> &url::Url {
        self.origin()
    }
    fn asset(&self, id: uuid::Uuid) -> crate::error::AppResult<AssetInfo> {
        self.asset(id)
    }
    fn outbound_enabled(&self) -> bool {
        self.outbound_enabled()
    }
    fn set_rating(&self, write: RatingWrite) -> crate::error::AppResult<WriteOutcome> {
        self.set_rating(write)
    }
}

mod favorite_api;
pub use favorite_api::*;

mod favorites;
pub use favorites::*;
