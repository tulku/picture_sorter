use super::*;
use crate::{archive::ArchiveRecord, domain::*};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatingConvergence {
    pub operation: OperationId,
    pub group: GroupId,
    pub server: String,
    pub asset: uuid::Uuid,
    pub jpeg: ArchivePath,
    pub digest: Digest,
    pub checksum: String,
    pub external_path: String,
    pub library: Option<String>,
    pub local_version: VersionId,
    pub rating: Rating,
    pub conflicts: Vec<(ConflictId, Digest)>,
}
fn evidence(kind: &str, group: &str, details: &str) -> AppResult<Digest> {
    Ok(Digest::of(
        encode(&(
            kind,
            group,
            serde_json::from_str::<serde_json::Value>(details)?,
        ))?
        .as_bytes(),
    ))
}
impl Catalog {
    pub fn rating_conflict_witnesses(
        &self,
        group: GroupId,
    ) -> AppResult<Vec<(ConflictId, Digest)>> {
        let mut query=self.db.prepare("SELECT id,kind,details FROM conflicts WHERE group_id=? AND resolved=0 AND kind IN('rating','rating_metadata') ORDER BY id LIMIT 129")?;
        let rows = query
            .query_map([group.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() > 128 {
            return Err(AppError::new(
                ErrorCode::Unavailable,
                "rating conflict witnesses exceed bounded convergence batch",
            ));
        }
        rows.into_iter()
            .map(|(id, kind, details)| {
                Ok((
                    serde_json::from_value(id.into())?,
                    evidence(&kind, &group.to_string(), &details)?,
                ))
            })
            .collect()
    }
    pub fn queue_rating_convergence(
        &mut self,
        operation: OperationId,
        group: GroupId,
        server: &str,
        asset: uuid::Uuid,
        version: VersionId,
        rating: Rating,
    ) -> AppResult<()> {
        let conflicts = self.rating_conflict_witnesses(group)?;
        if conflicts.is_empty() {
            return Ok(());
        }
        let binding:(String,String,String,String,Option<String>)=self.db.query_row("SELECT path,digest,remote_checksum,external_path,library FROM mappings WHERE server=? AND asset=? AND group_id=? AND stale=0",params![server,asset.to_string(),group.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        let convergence = RatingConvergence {
            operation,
            group,
            server: server.into(),
            asset,
            jpeg: ArchivePath::parse(&binding.0)?,
            digest: Digest::parse(&binding.1)?,
            checksum: binding.2,
            external_path: binding.3,
            library: binding.4,
            local_version: version,
            rating,
            conflicts,
        };
        if validate(&self.db, &convergence, false).is_err() {
            return Ok(());
        }
        let records = vec![
            ArchiveRecord::RatingObserved {
                operation,
                group,
                local: RatingObservation::Stars(rating),
                remote: RatingObservation::Stars(rating),
            },
            ArchiveRecord::RatingConverged {
                evidence: convergence,
            },
        ];
        self.db.execute("INSERT INTO pending_shared_records(operation,records) VALUES(?,?) ON CONFLICT DO NOTHING",params![operation.to_string(),encode(&records)?])?;
        Ok(())
    }
    pub fn validate_rating_convergence(&self, evidence: &RatingConvergence) -> AppResult<()> {
        validate(&self.db, evidence, false)
    }
}
fn validate(db: &Connection, c: &RatingConvergence, require_observation: bool) -> AppResult<()> {
    if c.conflicts.is_empty()
        || c.conflicts.len() > 128
        || super::identity::canonical(db, c.group)? != c.group
    {
        return Err(AppError::integrity("invalid convergence witnesses"));
    }
    let binding:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM mappings WHERE server=? AND asset=? AND group_id=? AND path=? AND digest=? AND remote_checksum=? AND external_path=? AND library IS ? AND (stale=0 OR ?))",params![c.server,c.asset.to_string(),c.group.to_string(),c.jpeg.as_str(),c.digest.as_str(),c.checksum,c.external_path,c.library,require_observation],|r|r.get(0))?;
    let uncertain:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM rating_intents WHERE group_id=? AND state IN('prepared','sent'))",[c.group.to_string()],|r|r.get(0))?;
    let current:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM versions v JOIN sidecars s ON s.id=v.sidecar JOIN archived_versions a ON a.version=v.id WHERE v.id=? AND s.group_id=? AND s.owner='rapidraw-base' AND EXISTS(SELECT 1 FROM heads h WHERE h.sidecar=s.id AND h.version=v.id) AND (SELECT count(*) FROM heads h WHERE h.sidecar=s.id)=1)",params![c.local_version.to_string(),c.group.to_string()],|r|r.get(0))?;
    if !binding || (!require_observation && uncertain) || !current {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "rating convergence identity, immutable save or uncertainty changed",
        ));
    }
    if require_observation {
        let observed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM rating_observations WHERE id=? AND group_id=? AND local=? AND remote=?)",params![c.operation.to_string(),c.group.to_string(),encode(&RatingObservation::Stars(c.rating))?,encode(&RatingObservation::Stars(c.rating))?],|r|r.get(0))?;
        if !observed {
            return Err(AppError::integrity(
                "convergence has no matching observations",
            ));
        }
    }
    Ok(())
}
pub(crate) fn apply(db: &Connection, c: RatingConvergence) -> AppResult<()> {
    validate(db, &c, true)?;
    for (id, expected) in &c.conflicts {
        let row: Option<(String, Option<String>, String, bool)> = db
            .query_row(
                "SELECT kind,group_id,details,resolved FROM conflicts WHERE id=?",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((kind, group, details, resolved)) = row else {
            return Err(AppError::integrity("convergence conflict is missing"));
        };
        if resolved {
            continue;
        }
        if !matches!(kind.as_str(), "rating" | "rating_metadata")
            || group.as_deref() != Some(&c.group.to_string())
            || evidence(&kind, &c.group.to_string(), &details)? != *expected
        {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "convergence conflict evidence changed",
            ));
        }
        db.execute(
            "UPDATE conflicts SET resolved=1,revision=revision+1 WHERE id=?",
            [id.to_string()],
        )?;
    }
    db.execute("UPDATE residency SET conflict=EXISTS(SELECT 1 FROM conflicts WHERE group_id=? AND resolved=0) WHERE group_id=?",params![c.group.to_string(),c.group.to_string()])?;
    crate::storage::refresh_group_protection(db, c.group)?;
    Ok(())
}
