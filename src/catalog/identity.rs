use super::*;
use crate::domain::*;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairFacts {
    pub context: String,
    pub stem: String,
    pub camera: String,
    pub capture: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PairEvidence {
    ObservedPlacement { path: ArchivePath, digest: Digest },
    Compared { left: PairFacts, right: PairFacts },
    UserSelected { reason: String },
}
impl PairEvidence {
    pub fn validate(&self) -> AppResult<()> {
        let valid = match self {
            Self::ObservedPlacement { .. } => true,
            Self::Compared { left, right } => {
                left == right
                    && !left.context.is_empty()
                    && !left.stem.is_empty()
                    && !left.camera.is_empty()
                    && !left.capture.is_empty()
            }
            Self::UserSelected { reason } => !reason.trim().is_empty(),
        };
        if valid {
            Ok(())
        } else {
            Err(AppError::new(
                ErrorCode::Conflict,
                "insufficient or contradictory pairing evidence",
            ))
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Protection {
    pub pinned: bool,
    pub pending: bool,
    pub conflict: bool,
    pub metadata_current: bool,
}
pub(crate) fn canonical(db: &Connection, id: GroupId) -> AppResult<GroupId> {
    let mut id = id;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(id) {
            return Err(AppError::integrity("group alias cycle"));
        }
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM groups WHERE id=?)",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(AppError::invalid("unknown group"));
        }
        let next: Option<String> = db
            .query_row(
                "SELECT canonical FROM aliases WHERE alias=?",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(next) = next {
            id = decode(format!("\"{next}\""))?;
        } else {
            return Ok(id);
        }
    }
}
pub(crate) fn merge(
    db: &Connection,
    ids: &[GroupId],
    evidence: &PairEvidence,
) -> AppResult<GroupId> {
    evidence.validate()?;
    let canonical_ids = ids
        .iter()
        .map(|id| canonical(db, *id))
        .collect::<AppResult<BTreeSet<_>>>()?;
    if canonical_ids.is_empty() {
        return Err(AppError::invalid("merge needs groups"));
    }
    if let PairEvidence::ObservedPlacement { path, digest } = evidence {
        for id in &canonical_ids {
            let count: u64 = db.query_row(
                "SELECT count(*) FROM members WHERE group_id=?",
                [id.to_string()],
                |r| r.get(0),
            )?;
            let owns: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM members WHERE group_id=? AND path=? AND digest=?)",
                params![id.to_string(), path.as_str(), digest.as_str()],
                |r| r.get(0),
            )?;
            if count != 0 && !owns {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "placement identity cannot associate an unrelated nonempty group",
                ));
            }
        }
    }
    let mut published = Vec::new();
    for id in &canonical_ids {
        if db.query_row(
            "SELECT published FROM groups WHERE id=?",
            [id.to_string()],
            |r| r.get::<_, bool>(0),
        )? {
            published.push(*id);
        }
    }
    let survivor = published.into_iter().min().unwrap_or(
        *canonical_ids
            .first()
            .ok_or_else(|| AppError::invalid("missing group"))?,
    );
    let winner = survivor.to_string();
    for id in canonical_ids {
        if id == survivor {
            continue;
        }
        let old = id.to_string();
        db.execute(
            "UPDATE members SET group_id=? WHERE group_id=?",
            params![winner, old],
        )?;
        for table in [
            "placements",
            "pin_targets",
            "sidecars",
            "mappings",
            "conflicts",
            "rating_observations",
            "rating_intents",
            "import_items",
            "group_facts",
            "startup_checks",
        ] {
            db.execute(
                &format!("UPDATE {table} SET group_id=? WHERE group_id=?"),
                params![winner, old],
            )?;
        }
        for table in ["rating_baselines", "rating_authorities"] {
            let old_row:Option<String>=db.query_row(&format!("SELECT json_object({}) FROM {table} WHERE group_id=?",if table=="rating_baselines" {"'common',common,'applied_local',applied_local,'pending',pending,'pending_version',pending_version,'source_version',source_version,'remote_observation',remote_observation"} else {"'server',server,'asset',asset"}),[&old],|r|r.get(0)).optional()?;
            if let Some(payload) = old_row {
                let exists: bool = db.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE group_id=?)"),
                    [&winner],
                    |r| r.get(0),
                )?;
                if !exists {
                    db.execute(
                        &format!("UPDATE {table} SET group_id=? WHERE group_id=?"),
                        params![winner, old],
                    )?;
                } else {
                    db.execute("INSERT INTO merged_rating_evidence(group_id,original_group,kind,payload) VALUES(?,?,?,?) ON CONFLICT DO NOTHING",params![winner,old,table,payload])?;
                    if table == "rating_baselines" {
                        let current:String=db.query_row("SELECT json_object('common',common,'applied_local',applied_local,'pending',pending,'pending_version',pending_version,'source_version',source_version,'remote_observation',remote_observation) FROM rating_baselines WHERE group_id=?",[&winner],|r|r.get(0))?;
                        if current != payload {
                            db.execute("INSERT INTO merged_rating_evidence(group_id,original_group,kind,payload) VALUES(?,?,?,?) ON CONFLICT DO NOTHING",params![winner,winner,table,current])?;
                            db.execute("INSERT INTO conflicts(id,kind,group_id,details) VALUES(?,'rating',?,?)",params![ConflictId::new().to_string(),winner,encode(&serde_json::json!({"reason":"associated groups have different rating baselines","retained":[winner,old]}))?])?;
                            db.execute(
                                "UPDATE residency SET conflict=1,protected=1 WHERE group_id=?",
                                [&winner],
                            )?;
                            db.execute("DELETE FROM rating_baselines WHERE group_id=?", [&winner])?;
                        }
                    }
                    if table == "rating_authorities" {
                        let previous: serde_json::Value = serde_json::from_str(&payload)?;
                        let current: (String, String) = db.query_row(
                            "SELECT server,asset FROM rating_authorities WHERE group_id=?",
                            [&winner],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )?;
                        if previous["server"] != current.0 || previous["asset"] != current.1 {
                            let details = serde_json::json!({"server":current.0,"assets":[current.1,previous["asset"]],"other_server":previous["server"]});
                            db.execute("INSERT INTO conflicts(id,kind,group_id,details) VALUES(?,'rating_authority',?,?)",params![ConflictId::new().to_string(),winner,encode(&details)?])?;
                            db.execute(
                                "UPDATE residency SET conflict=1,protected=1 WHERE group_id=?",
                                [&winner],
                            )?;
                        }
                    }
                }
            }
        }
        db.execute("UPDATE residency SET pin=max(pin,(SELECT pin FROM residency WHERE group_id=?)),protected=max(protected,(SELECT protected FROM residency WHERE group_id=?)),conflict=max(conflict,(SELECT conflict FROM residency WHERE group_id=?)),metadata_current=0,ready_revision=NULL WHERE group_id=?",params![old,old,old,winner])?;
        let left: Option<String> = db.query_row(
            "SELECT primary_raw FROM groups WHERE id=?",
            [&winner],
            |r| r.get(0),
        )?;
        let right: Option<String> =
            db.query_row("SELECT primary_raw FROM groups WHERE id=?", [&old], |r| {
                r.get(0)
            })?;
        if left.is_some() && right.is_some() && left != right {
            db.execute(
                "UPDATE residency SET conflict=1 WHERE group_id=?",
                [&winner],
            )?;
            db.execute(
                "INSERT INTO conflicts(id,kind,group_id,details) VALUES(?,'primary_raw',?,?)",
                params![
                    ConflictId::new().to_string(),
                    winner,
                    encode(&serde_json::json!({"candidates":[left,right]}))?
                ],
            )?;
            db.execute("UPDATE groups SET primary_raw=NULL WHERE id=?", [&winner])?;
        } else if left.is_none() {
            db.execute(
                "UPDATE groups SET primary_raw=? WHERE id=?",
                params![right, winner],
            )?;
        }
        db.execute(
            "INSERT INTO aliases(alias,canonical) VALUES(?,?)",
            params![old, winner],
        )?;
        db.execute(
            "UPDATE groups SET revision=revision+1 WHERE id=?",
            [&winner],
        )?;
    }
    let mut query = db.prepare("SELECT id,request FROM jobs")?;
    let jobs = query
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(query);
    for (id, request) in jobs {
        let mut request: DurableRequest = decode(request)?;
        match &mut request.kind {
            JobKind::Pin { group } => *group = canonical(db, *group)?,
            JobKind::Refresh { groups } => {
                for group in groups.iter_mut() {
                    *group = canonical(db, *group)?;
                }
                groups.sort();
                groups.dedup();
            }
            _ => {}
        }
        db.execute(
            "UPDATE jobs SET request=? WHERE id=?",
            params![encode(&request)?, id],
        )?;
    }
    db.execute(
        "INSERT INTO associations(id,evidence) VALUES(?,?)",
        params![OperationId::new().to_string(), encode(evidence)?],
    )?;
    Ok(survivor)
}
impl Catalog {
    pub fn create_group(&mut self, published: bool) -> AppResult<GroupId> {
        let id = GroupId::new();
        self.insert_group(id, published)?;
        Ok(id)
    }
    pub fn insert_group(&mut self, id: GroupId, published: bool) -> AppResult<()> {
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT INTO groups(id,published) VALUES(?,?)",
            params![id.to_string(), published],
        )?;
        tx.execute(
            "INSERT INTO residency(group_id) VALUES(?)",
            [id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn canonical_group(&self, id: GroupId) -> AppResult<GroupId> {
        canonical(&self.db, id)
    }
    pub fn group(&self, id: GroupId) -> AppResult<Group> {
        let id = self.canonical_group(id)?;
        let (revision, primary): (u64, Option<String>) = self.db.query_row(
            "SELECT revision,primary_raw FROM groups WHERE id=?",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut q=self.db.prepare("SELECT m.path,m.digest,b.size,m.role FROM members m JOIN blobs b ON b.digest=m.digest WHERE m.group_id=? ORDER BY m.path")?;
        let rows = q
            .query_map([id.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let members = rows
            .into_iter()
            .map(|(p, d, size, role)| {
                Ok(OriginalMember {
                    path: ArchivePath::parse(&p)?,
                    digest: Digest::parse(&d)?,
                    size,
                    role: decode(role)?,
                })
            })
            .collect::<AppResult<Vec<_>>>()?;
        Ok(Group {
            id,
            membership_revision: MembershipRevision(revision),
            members,
            primary_raw: primary.map(|s| ArchivePath::parse(&s)).transpose()?,
        })
    }
    pub fn attach_member(
        &mut self,
        id: GroupId,
        member: OriginalMember,
        evidence: PairEvidence,
    ) -> AppResult<MembershipRevision> {
        evidence.validate()?;
        member.validate()?;
        let tx = self.db.transaction()?;
        let id = canonical(&tx, id)?;
        let archive: String = tx
            .query_row("SELECT id FROM archives ORDER BY id LIMIT 1", [], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or_else(|| "unbound".into());
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT group_id,digest FROM members WHERE archive=? AND path=?",
                params![archive, member.path.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let target = if let Some((group, digest)) = existing {
            if digest != member.digest.as_str() {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "placement has different content",
                ));
            }
            let previous = decode(format!("\"{group}\""))?;
            if canonical(&tx, previous)? != id {
                merge(&tx, &[id, previous], &evidence)?
            } else {
                id
            }
        } else {
            tx.execute(
                "INSERT INTO blobs(digest,size) VALUES(?,?) ON CONFLICT(digest) DO NOTHING",
                params![member.digest.as_str(), member.size],
            )?;
            let size: u64 = tx.query_row(
                "SELECT size FROM blobs WHERE digest=?",
                [member.digest.as_str()],
                |r| r.get(0),
            )?;
            if size != member.size {
                return Err(AppError::integrity("digest size mismatch"));
            }
            tx.execute(
                "INSERT INTO members(archive,path,group_id,digest,role) VALUES(?,?,?,?,?)",
                params![
                    archive,
                    member.path.as_str(),
                    id.to_string(),
                    member.digest.as_str(),
                    encode(&member.role)?
                ],
            )?;
            tx.execute(
                "UPDATE groups SET revision=revision+1 WHERE id=?",
                [id.to_string()],
            )?;
            tx.execute("UPDATE residency SET ready_revision=NULL,metadata_current=0,protected=1 WHERE group_id=?",[id.to_string()])?;
            id
        };
        let revision = tx.query_row(
            "SELECT revision FROM groups WHERE id=?",
            [target.to_string()],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(MembershipRevision(revision))
    }
    pub fn merge_groups(&mut self, ids: &[GroupId], evidence: PairEvidence) -> AppResult<GroupId> {
        let tx = self.db.transaction()?;
        let id = merge(&tx, ids, &evidence)?;
        tx.commit()?;
        Ok(id)
    }
    pub fn protection(&self, id: GroupId) -> AppResult<Protection> {
        let id = self.canonical_group(id)?;
        Ok(self.db.query_row(
            "SELECT pin,protected,conflict,metadata_current FROM residency WHERE group_id=?",
            [id.to_string()],
            |r| {
                Ok(Protection {
                    pinned: r.get(0)?,
                    pending: r.get(1)?,
                    conflict: r.get(2)?,
                    metadata_current: r.get(3)?,
                })
            },
        )?)
    }
    pub fn set_protection(&mut self, id: GroupId, pinned: bool, conflict: bool) -> AppResult<()> {
        let id = self.canonical_group(id)?;
        self.db.execute(
            "UPDATE residency SET pin=?,conflict=? WHERE group_id=?",
            params![pinned, conflict, id.to_string()],
        )?;
        Ok(())
    }
}
impl Catalog {
    pub fn group_batch(&self, after: Option<GroupId>, limit: usize) -> AppResult<Vec<Group>> {
        if limit == 0 || limit > 128 {
            return Err(AppError::invalid("group page must be 1..128"));
        }
        let mut query=self.db.prepare("SELECT id FROM groups WHERE id>? AND id NOT IN(SELECT alias FROM aliases) ORDER BY id LIMIT ?")?;
        let ids = query
            .query_map(
                params![after.map(|id| id.to_string()).unwrap_or_default(), limit],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| self.group(decode(format!("\"{id}\""))?))
            .collect()
    }
}
impl Catalog {
    pub fn is_ancestor(&self, ancestor: VersionId, descendant: VersionId) -> AppResult<bool> {
        Ok(self.db.query_row("WITH RECURSIVE ancestors(id) AS (VALUES(?) UNION SELECT p.parent FROM version_parents p JOIN ancestors a ON p.version=a.id) SELECT EXISTS(SELECT 1 FROM ancestors WHERE id=?)",params![descendant.to_string(),ancestor.to_string()],|r|r.get(0))?)
    }
    pub fn common_version(
        &self,
        left: VersionId,
        right: VersionId,
    ) -> AppResult<Option<VersionId>> {
        let mut query=self.db.prepare("WITH RECURSIVE lhs(id) AS (VALUES(?) UNION SELECT p.parent FROM version_parents p JOIN lhs ON p.version=lhs.id),rhs(id) AS (VALUES(?) UNION SELECT p.parent FROM version_parents p JOIN rhs ON p.version=rhs.id),common(id) AS (SELECT id FROM lhs INTERSECT SELECT id FROM rhs) SELECT c.id FROM common c WHERE NOT EXISTS(SELECT 1 FROM version_parents p JOIN common child ON child.id=p.version WHERE p.parent=c.id) LIMIT 2")?;
        let ids = query
            .query_map(params![left.to_string(), right.to_string()], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if ids.len() > 1 {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "multiple common version ancestors require resolution",
            ));
        }
        ids.into_iter()
            .next()
            .map(|id| decode(format!("\"{id}\"")))
            .transpose()
    }
}
#[cfg(test)]
mod rating_merge_tests {
    use super::*;
    #[test]
    fn merge_transfers_losing_rating_baseline_and_authority() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::fs::RootHandle::open(dir.path(), crate::fs::RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let a = catalog.create_group(true).unwrap();
        let b = catalog.create_group(true).unwrap();
        let winner = a.min(b);
        let loser = a.max(b);
        let asset = uuid::Uuid::new_v4().to_string();
        catalog.db.execute("INSERT INTO mappings(server,asset,path,group_id) VALUES('https://fixture.example',?,'a.JPG',?)",params![asset,loser.to_string()]).unwrap();
        catalog.db.execute("INSERT INTO rating_authorities(group_id,server,asset) VALUES(?,'https://fixture.example',?)",params![loser.to_string(),asset]).unwrap();
        catalog.db.execute("INSERT INTO rating_baselines(group_id,common,applied_local,pending) VALUES(?,'3','3',5)",[loser.to_string()]).unwrap();
        assert_eq!(
            catalog
                .merge_groups(
                    &[a, b],
                    PairEvidence::UserSelected {
                        reason: "isolated merge".into()
                    }
                )
                .unwrap(),
            winner
        );
        let pending: Option<u8> = catalog
            .db
            .query_row(
                "SELECT pending FROM rating_baselines WHERE group_id=?",
                [winner.to_string()],
                |r| r.get(0),
            )
            .optional()
            .unwrap()
            .flatten();
        assert_eq!(pending, Some(5));
        let chosen: Option<String> = catalog
            .db
            .query_row(
                "SELECT asset FROM rating_authorities WHERE group_id=?",
                [winner.to_string()],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(chosen, Some(asset));
    }
}
