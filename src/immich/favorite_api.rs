use crate::error::AppResult;
#[derive(Debug, Clone)]
pub struct FavoriteWrite {
    pub asset: uuid::Uuid,
    pub expected: bool,
    pub desired: bool,
    pub jpeg_checksum: String,
    pub external_path: String,
    pub library_id: Option<uuid::Uuid>,
}
#[derive(Debug)]
pub enum FavoriteOutcome {
    IdentityChanged,
    Confirmed,
    Conflict { observed: bool },
    Uncertain,
}
pub enum FavoriteProgress {
    None,
    Pending,
    Finished(AppResult<FavoriteOutcome>),
}

impl FavoriteWrite {
    pub(crate) fn matches_asset(&self, asset: &super::AssetInfo) -> bool {
        asset.id == self.asset
            && asset.checksum == self.jpeg_checksum
            && asset.original_path == self.external_path
            && asset.library_id == self.library_id
            && asset.kind == "IMAGE"
            && asset.original_mime_type.as_deref() == Some("image/jpeg")
    }
}
