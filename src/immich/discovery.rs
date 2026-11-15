use super::*;
use crate::{catalog::Catalog, error::AppResult};
use rusqlite::{OptionalExtension, params};

#[derive(Default, Debug, Clone, serde::Serialize)]
pub struct DiscoveryCounts {
    pub matched: u64,
    pub skipped: u64,
    pub conflicts: u64,
}

pub fn discover_assets(
    catalog: &mut Catalog,
    origin: &url::Url,
    mappings: &[PrefixMapping],
    items: &[AssetInfo],
) -> AppResult<DiscoveryCounts> {
    let mut counts = DiscoveryCounts::default();
    for asset in items {
        let group = match resolve_asset(asset, mappings, catalog) {
            Ok(group) => group,
            Err(_) => {
                counts.skipped += 1;
                continue;
            }
        };
        let previous: Option<(String,String,String,Option<String>,bool)> = catalog.db.query_row(
            "SELECT group_id,external_path,remote_checksum,library,stale FROM mappings WHERE server=? AND asset=?",
            params![origin.origin().ascii_serialization(),asset.id.to_string()],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).optional()?;
        if let Some((old_group, path, checksum, library, stale)) = previous {
            if old_group == group.to_string()
                && path == asset.original_path
                && checksum == asset.checksum
                && library == asset.library_id.map(|id| id.to_string())
                && !stale
            {
                counts.matched += 1;
            } else {
                counts.conflicts += 1;
            }
            continue;
        }
        match catalog.bind_asset(origin, asset, group) {
            Ok(()) => counts.matched += 1,
            Err(error) if error.code == crate::error::ErrorCode::Conflict => counts.conflicts += 1,
            Err(error) => return Err(error),
        }
    }
    Ok(counts)
}
