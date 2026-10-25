use super::*;
use crate::{
    catalog::*,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
};
use rusqlite::{OptionalExtension, params};
use std::path::{Component, Path, PathBuf};
use url::Url;
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefixMapping {
    pub source: PathBuf,
    pub target: Option<ArchivePath>,
    pub library: Option<uuid::Uuid>,
}
impl PrefixMapping {
    pub fn translate(&self, value: &str) -> AppResult<ArchivePath> {
        let path = Path::new(value);
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || value.contains(['\\', '\0'])
        {
            return Err(AppError::invalid("invalid external asset path"));
        }
        let relative = path
            .strip_prefix(&self.source)
            .map_err(|_| AppError::invalid("asset does not match configured component prefix"))?
            .to_str()
            .ok_or_else(|| AppError::invalid("UTF-8 asset path required"))?;
        ArchivePath::parse(&match &self.target {
            Some(prefix) => format!("{}/{relative}", prefix.as_str()),
            None => relative.to_owned(),
        })
    }
}
pub fn resolve_asset(
    asset: &AssetInfo,
    mappings: &[PrefixMapping],
    catalog: &Catalog,
) -> AppResult<GroupId> {
    if asset.kind != "IMAGE"
        || asset.original_mime_type.as_deref() != Some("image/jpeg")
        || !matches!(
            asset
                .original_file_name
                .rsplit('.')
                .next()
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("jpg" | "jpeg")
        )
    {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "only verified JPEG image assets support photograph pins",
        ));
    }
    let mut groups = std::collections::BTreeSet::new();
    for mapping in mappings {
        if mapping.library.is_some() && mapping.library != asset.library_id {
            continue;
        }
        if let Ok(path) = mapping.translate(&asset.original_path) {
            let id: Option<String> = catalog
                .db
                .query_row(
                    "SELECT group_id FROM members WHERE path=? AND role=?",
                    params![path.as_str(), serde_json::to_string(&MediaRole::Jpeg)?],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = id {
                let id: GroupId = serde_json::from_str(&format!("\"{id}\""))?;
                groups.insert(catalog.canonical_group(id)?);
            }
        }
    }
    if groups.len() != 1 {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "asset has no unique exact archived JPEG placement",
        ));
    }
    groups
        .into_iter()
        .next()
        .ok_or_else(|| AppError::integrity("resolved group disappeared"))
}
#[derive(Debug)]
pub struct PinResult {
    pub group: Option<GroupId>,
    pub job: Option<JobId>,
    pub reason: Option<String>,
}
pub struct PinCoordinator<'a> {
    pub catalog: &'a mut Catalog,
    pub client: Option<&'a dyn ImmichAccess>,
    pub origin: Url,
    pub mappings: &'a [PrefixMapping],
}
impl Catalog {
    pub fn bind_asset(&mut self, origin: &Url, asset: &AssetInfo, group: GroupId) -> AppResult<()> {
        let group = self.canonical_group(group)?;
        let old: Option<(String, String, String)> = self
            .db
            .query_row(
                "SELECT group_id,external_path,remote_checksum FROM mappings WHERE server=? AND asset=?",
                params![origin.origin().ascii_serialization(), asset.id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if old.is_some_and(|(g, p, checksum)| {
            g != group.to_string() || p != asset.original_path || checksum != asset.checksum
        }) {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "asset identity changed; old mapping preserved pending verification",
            ));
        }
        let candidates = self
            .group(group)?
            .members
            .into_iter()
            .filter(|m| {
                m.role == MediaRole::Jpeg
                    && m.path.as_str().rsplit('/').next() == Some(asset.original_file_name.as_str())
            })
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "mapping requires one exact JPEG placement",
            ));
        }
        let member = &candidates[0];
        self.db.execute("INSERT INTO mappings(server,asset,library,path,digest,group_id,remote_checksum,external_path) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(server,asset) DO NOTHING",params![origin.origin().ascii_serialization(),asset.id.to_string(),asset.library_id.map(|id|id.to_string()),member.path.as_str(),member.digest.as_str(),group.to_string(),asset.checksum,asset.original_path])?;

        Ok(())
    }
}
impl PinCoordinator<'_> {
    pub fn request(&mut self, urls: &[Url], operation: OperationId) -> AppResult<Vec<PinResult>> {
        let mut results = Vec::new();
        for url in urls {
            let id = match parse_photo_url(url.as_str(), &self.origin) {
                Ok(id) => id,
                Err(e) => {
                    results.push(PinResult {
                        group: None,
                        job: None,
                        reason: Some(e.to_string()),
                    });
                    continue;
                }
            };
            let server = self.origin.origin().ascii_serialization();
            let previous: Option<String> = self
                .catalog
                .db
                .query_row(
                    "SELECT child_operation FROM pin_batches WHERE operation=? AND asset=?",
                    params![operation.to_string(), id.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            let child = match previous {
                Some(id) => serde_json::from_str(&format!("\"{id}\""))?,
                None => {
                    let child = OperationId::new();
                    self.catalog.db.execute(
                        "INSERT INTO pin_batches(operation,asset,child_operation) VALUES(?,?,?)",
                        params![operation.to_string(), id.to_string(), child.to_string()],
                    )?;
                    child
                }
            };
            let job = self.catalog.enqueue(DurableRequest {
                operation_id: child,
                kind: JobKind::PinUrl {
                    server: server.clone(),
                    asset: id,
                },
            })?;
            let cached: Option<String> = self
                .catalog
                .db
                .query_row(
                    "SELECT group_id FROM mappings WHERE server=? AND asset=? AND stale=0",
                    params![server, id.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            let resolved = if let Some(group) = cached {
                let group: GroupId = serde_json::from_str(&format!("\"{group}\""))?;
                Ok(self.catalog.canonical_group(group)?)
            } else if let Some(client) = self.client {
                client.asset(id).and_then(|asset| {
                    let group = resolve_asset(&asset, self.mappings, self.catalog)?;
                    self.catalog.bind_asset(&self.origin, &asset, group)?;
                    Ok(group)
                })
            } else {
                Err(AppError::new(
                    ErrorCode::Unavailable,
                    "Immich unavailable; URL resolution remains pending",
                ))
            };
            match resolved {
                Ok(group) => {
                    self.catalog.pin_url_target(job, group)?;
                    results.push(PinResult {
                        group: Some(group),
                        job: Some(job),
                        reason: None,
                    });
                }
                Err(error) => {
                    let generation = self.catalog.job(job)?.generation;
                    self.catalog.transition(
                        job,
                        generation,
                        JobTransition::Wait(JobState::WaitingImmich, error.to_string()),
                    )?;
                    results.push(PinResult {
                        group: None,
                        job: Some(job),
                        reason: Some(error.to_string()),
                    });
                }
            }
        }
        Ok(results)
    }
}

impl Catalog {
    pub fn pin_url_target(&mut self, job: JobId, group: GroupId) -> AppResult<()> {
        let group = self.canonical_group(group)?;
        let record = self.job(job)?;
        let JobKind::PinUrl { server, asset } = record.request.kind else {
            return Err(AppError::invalid("job is not a URL pin"));
        };
        let mapped: String = self.db.query_row(
            "SELECT group_id FROM mappings WHERE server=? AND asset=? AND stale=0",
            params![server, asset.to_string()],
            |r| r.get(0),
        )?;
        let mapped: GroupId = serde_json::from_str(&format!("\"{mapped}\""))?;
        if self.canonical_group(mapped)? != group {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "URL mapping does not authorize this pin target",
            ));
        }
        let tx = self.db.transaction()?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT group_id FROM pin_targets WHERE job=?",
                [job.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(prior) = prior {
            if prior != group.to_string() {
                return Err(AppError::new(ErrorCode::Conflict, "pin target changed"));
            }
            return Ok(());
        }
        tx.execute(
            "INSERT INTO pin_targets(job,group_id) VALUES(?,?)",
            params![job.to_string(), group.to_string()],
        )?;
        tx.execute(
            "UPDATE residency SET pin=1 WHERE group_id=?",
            [group.to_string()],
        )?;
        if record.state == JobState::WaitingImmich {
            tx.execute("UPDATE jobs SET state=?,reason=NULL,generation=generation+1 WHERE id=? AND generation=?",params![serde_json::to_string(&JobState::Queued)?,job.to_string(),record.generation])?;
        }
        tx.commit()?;
        Ok(())
    }
}
