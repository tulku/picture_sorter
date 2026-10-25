use super::*;
use crate::{archive::ArchiveRecord, domain::*};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConflictResolution {
    ChooseHead { version: VersionId },
    KeepAlternates,
    AdoptEqual { digest: Digest },
    ChooseDistinctPath { path: ArchivePath },
    AssociateGroups { groups: Vec<GroupId> },
    KeepIndependent { groups: Vec<GroupId> },
    SelectPrimaryRaw { member: ArchivePath },
    SelectRatingAuthority { asset: uuid::Uuid },
    AssociateSidecar { sidecar: SidecarId, group: GroupId },
    RetainUnassociated { sidecar: SidecarId },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolutionRequest {
    pub operation: OperationId,
    pub conflict: ConflictId,
    pub expected_revision: u64,
    pub expected_heads: Vec<VersionId>,
    pub expected_memberships: Vec<(GroupId, MembershipRevision)>,
    pub resolution: ConflictResolution,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictRecord {
    pub id: ConflictId,
    pub kind: String,
    pub group: Option<GroupId>,
    pub revision: u64,
    pub details: serde_json::Value,
    pub resolved: bool,
}
pub(crate) fn refresh_association_flags(db: &Connection) -> AppResult<()> {
    db.execute("UPDATE residency SET conflict=1 WHERE EXISTS(SELECT 1 FROM conflicts c,json_each(c.details,'$.groups') e WHERE c.kind='association' AND c.resolved=0 AND (e.value=residency.group_id OR EXISTS(SELECT 1 FROM aliases a WHERE a.alias=e.value AND a.canonical=residency.group_id)))",[])?;
    Ok(())
}
impl Catalog {
    pub fn record_ambiguity(
        &mut self,
        kind: &str,
        group: Option<GroupId>,
        details: serde_json::Value,
    ) -> AppResult<ConflictId> {
        let group = group.map(|g| self.canonical_group(g)).transpose()?;
        let previous:Option<String>=self.db.query_row("SELECT id FROM conflicts WHERE kind=? AND group_id IS ? AND details=? AND resolved=0 ORDER BY id LIMIT 1",params![kind,group.map(|g|g.to_string()),encode(&details)?],|r|r.get(0)).optional()?;
        if let Some(id) = previous {
            return Ok(serde_json::from_value(id.into())?);
        }
        let id = ConflictId::new();
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT INTO conflicts(id,kind,group_id,details) VALUES(?,?,?,?)",
            params![
                id.to_string(),
                kind,
                group.map(|g| g.to_string()),
                encode(&details)?
            ],
        )?;
        if let Some(group) = group {
            tx.execute(
                "UPDATE residency SET conflict=1,protected=1 WHERE group_id=?",
                [group.to_string()],
            )?;
        }
        refresh_association_flags(&tx)?;
        tx.commit()?;
        Ok(id)
    }
    pub fn conflicts(&self, include_resolved: bool) -> AppResult<Vec<ConflictRecord>> {
        let mut query=self.db.prepare("SELECT id,kind,group_id,revision,details,resolved FROM conflicts WHERE resolved=0 OR ? ORDER BY id")?;
        let mut rows = query.query([include_resolved])?;
        let mut result = vec![];
        while let Some(row) = rows.next()? {
            result.push(ConflictRecord {
                id: decode(format!("\"{}\"", row.get::<_, String>(0)?))?,
                kind: row.get(1)?,
                group: row
                    .get::<_, Option<String>>(2)?
                    .map(|s| decode(format!("\"{s}\"")))
                    .transpose()?,
                revision: row.get(3)?,
                details: decode(row.get(4)?)?,
                resolved: row.get(5)?,
            });
        }
        Ok(result)
    }
    pub fn conflicts_page(
        &self,
        after: Option<ConflictId>,
        limit: usize,
        include_resolved: bool,
    ) -> AppResult<Vec<ConflictRecord>> {
        if limit == 0 || limit > 128 {
            return Err(AppError::invalid("conflict page must be 1..128"));
        }
        let mut query=self.db.prepare("SELECT id,kind,group_id,revision,details,resolved FROM conflicts WHERE id>? AND (resolved=0 OR ?) ORDER BY id LIMIT ?")?;
        let mut rows = query.query(params![
            after.map(|id| id.to_string()).unwrap_or_default(),
            include_resolved,
            limit
        ])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            result.push(ConflictRecord {
                id: decode(format!("\"{}\"", row.get::<_, String>(0)?))?,
                kind: row.get(1)?,
                group: row
                    .get::<_, Option<String>>(2)?
                    .map(|id| decode(format!("\"{id}\"")))
                    .transpose()?,
                revision: row.get(3)?,
                details: decode(row.get(4)?)?,
                resolved: row.get(5)?,
            });
        }
        Ok(result)
    }
    pub fn conflict(&self, id: ConflictId) -> AppResult<ConflictRecord> {
        self.db
            .query_row(
                "SELECT kind,group_id,revision,details,resolved FROM conflicts WHERE id=?",
                [id.to_string()],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, u64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, bool>(4)?,
                    ))
                },
            )
            .map_err(Into::into)
            .and_then(|(kind, group, revision, details, resolved)| {
                Ok(ConflictRecord {
                    id,
                    kind,
                    group: group.map(|g| decode(format!("\"{g}\""))).transpose()?,
                    revision,
                    details: decode(details)?,
                    resolved,
                })
            })
    }
    pub fn resolution_applied(&self, request: &ResolutionRequest) -> AppResult<bool> {
        let prior: Option<String> = self
            .db
            .query_row(
                "SELECT request FROM resolution_receipts WHERE operation=?",
                [request.operation.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        match prior {
            Some(prior) if prior == encode(request)? => Ok(true),
            Some(_) => Err(failure()),
            None => Ok(false),
        }
    }
    pub fn resolution_records(&self, request: &ResolutionRequest) -> AppResult<Vec<ArchiveRecord>> {
        if self.resolution_applied(request)? {
            return Ok(vec![]);
        }
        validate(&self.db, request)?;
        let mut records = vec![ArchiveRecord::SemanticAmbiguityResolved {
            request: request.clone(),
            witness: semantic_witness(&self.db, request)?,
        }];
        if let ConflictResolution::ChooseHead { version } = request.resolution {
            let chosen = self.version(version)?;
            let (path, owner): (String, String) = self.db.query_row(
                "SELECT path,owner FROM sidecars WHERE id=?",
                [chosen.sidecar.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if owner == "rapidraw-base" || owner == "preserved-source-companion" {
                let conflict = self.conflict(request.conflict)?;
                let previous: Option<String> = self
                    .db
                    .query_row(
                        "SELECT committed FROM projections WHERE sidecar=?",
                        [chosen.sidecar.to_string()],
                        |r| r.get(0),
                    )
                    .optional()?
                    .flatten();
                let expected = if conflict.kind == "nas_projection" {
                    serde_json::from_value(conflict.details["observed"].clone())?
                } else {
                    previous
                        .map(|p| {
                            serde_json::from_str::<crate::archive::ProjectionPlan>(&p)
                                .map(|p| p.body)
                        })
                        .transpose()?
                        .unwrap_or(VersionBody::Reset)
                };
                records.push(ArchiveRecord::ProjectionPlanned {
                    plan: crate::archive::ProjectionPlan {
                        sidecar: chosen.sidecar,
                        path: ArchivePath::parse(&path)?,
                        version: VersionId(request.operation.0),
                        expected,
                        body: chosen.body,
                        owner,
                    },
                });
            }
        }
        Ok(records)
    }
}
pub(crate) fn semantic_witness(
    db: &Connection,
    request: &ResolutionRequest,
) -> AppResult<serde_json::Value> {
    let conflict: (String, Option<String>, String) = db.query_row(
        "SELECT kind,group_id,details FROM conflicts WHERE id=?",
        [request.conflict.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut groups = Vec::new();
    for (group, _) in &request.expected_memberships {
        let canonical = super::identity::canonical(db, *group)?;
        let primary: Option<String> = db.query_row(
            "SELECT primary_raw FROM groups WHERE id=?",
            [canonical.to_string()],
            |r| r.get(0),
        )?;
        let mut query=db.prepare("SELECT m.path,m.digest,m.role,b.size FROM members m JOIN blobs b ON b.digest=m.digest WHERE m.group_id=? ORDER BY m.path,m.digest,m.role")?;
        let mut members = query
            .query_map([canonical.to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, u64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if conflict.0 == "original_collision" {
            let disputed: OriginalMember = serde_json::from_str(&conflict.2)?;
            members.retain(|m| m.0 != disputed.path.as_str() || m.1 != disputed.digest.as_str());
        }
        let primary = primary.filter(|path| members.iter().any(|m| &m.0 == path));
        groups.push(serde_json::json!({"group":canonical,"primary":primary,"members":members}));
    }
    Ok(
        serde_json::json!({"kind":conflict.0,"group":conflict.1,"details":serde_json::from_str::<serde_json::Value>(&conflict.2)?,"memberships":groups}),
    )
}
pub(crate) fn validate_semantic(
    db: &Connection,
    request: &ResolutionRequest,
    witness: &serde_json::Value,
) -> AppResult<ResolutionRequest> {
    if semantic_witness(db, request)? != *witness {
        return Err(failure());
    }
    let mut localized = request.clone();
    localized.expected_revision = db.query_row(
        "SELECT revision FROM conflicts WHERE id=?",
        [request.conflict.to_string()],
        |r| r.get(0),
    )?;
    for (group, revision) in &mut localized.expected_memberships {
        *revision = MembershipRevision(db.query_row(
            "SELECT revision FROM groups WHERE id=?",
            [group.to_string()],
            |r| r.get(0),
        )?);
    }
    validate(db, &localized)?;
    Ok(localized)
}
impl Catalog {
    pub fn validate_shared_resolution(
        &self,
        request: &ResolutionRequest,
        witness: &serde_json::Value,
    ) -> AppResult<()> {
        validate_semantic(&self.db, request, witness).map(|_| ())
    }
}
pub(crate) fn apply_semantic_resolution(
    db: &Connection,
    request: ResolutionRequest,
    witness: serde_json::Value,
) -> AppResult<()> {
    let prior: Option<String> = db
        .query_row(
            "SELECT request FROM resolution_receipts WHERE operation=?",
            [request.operation.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(prior) = prior {
        return if prior == encode(&request)? {
            Ok(())
        } else {
            Err(failure())
        };
    }
    let localized = validate_semantic(db, &request, &witness)?;
    apply_resolution(db, localized)?;
    db.execute(
        "UPDATE associations SET evidence=? WHERE id=?",
        params![
            encode(&PairEvidence::UserSelected {
                reason: encode(&request)?
            })?,
            request.operation.to_string()
        ],
    )?;
    db.execute(
        "UPDATE resolution_receipts SET request=? WHERE operation=?",
        params![encode(&request)?, request.operation.to_string()],
    )?;
    Ok(())
}
struct Context {
    kind: String,
    group: Option<GroupId>,
    details: serde_json::Value,
}
fn failure() -> AppError {
    AppError::new(
        ErrorCode::Conflict,
        "resolution selectors, evidence or revision do not match the current ambiguity",
    )
}
fn validate(db: &Connection, request: &ResolutionRequest) -> AppResult<Context> {
    let row: (String, Option<String>, u64, String, bool) = db.query_row(
        "SELECT kind,group_id,revision,details,resolved FROM conflicts WHERE id=?",
        [request.conflict.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    if row.2 != request.expected_revision || row.4 {
        return Err(failure());
    }
    let context = Context {
        kind: row.0,
        group: row.1.map(|s| decode(format!("\"{s}\""))).transpose()?,
        details: decode(row.3)?,
    };
    let mut selected = std::collections::BTreeSet::new();
    for (group, revision) in &request.expected_memberships {
        if !selected.insert(*group) {
            return Err(failure());
        }
        let id = super::identity::canonical(db, *group)?;
        let actual: u64 = db.query_row(
            "SELECT revision FROM groups WHERE id=?",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if actual != revision.0 || id != *group {
            return Err(failure());
        }
    }
    if let Some(group) = context.group
        && !selected.contains(&group)
    {
        return Err(failure());
    }
    match &request.resolution {
        ConflictResolution::ChooseHead { version } => {
            let sidecar: String = db.query_row(
                "SELECT sidecar FROM versions WHERE id=?",
                [version.to_string()],
                |r| r.get(0),
            )?;
            let mut heads = super::replay::heads(db, decode(format!("\"{sidecar}\""))?)?;
            heads.sort();
            let mut expected = request.expected_heads.clone();
            expected.sort();
            let mut recorded: Vec<VersionId> = if context.kind == "nas_projection" {
                vec![
                    serde_json::from_value(context.details["version"].clone())
                        .map_err(|_| failure())?,
                ]
            } else {
                serde_json::from_value(context.details.clone()).map_err(|_| failure())?
            };
            recorded.sort();
            if expected != heads || recorded != heads || !heads.contains(version) {
                return Err(failure());
            }
        }
        ConflictResolution::KeepAlternates => {}
        ConflictResolution::AdoptEqual { digest } => {
            if context.kind != "original_collision" {
                return Err(failure());
            }
            let member: OriginalMember =
                serde_json::from_value(context.details.clone()).map_err(|_| failure())?;
            let matches: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM placements WHERE path=? AND digest=? AND archived=1)",
                params![member.path.as_str(), digest.as_str()],
                |r| r.get(0),
            )?;
            if member.digest != *digest || !matches {
                return Err(failure());
            }
        }
        ConflictResolution::ChooseDistinctPath { path } => {
            if context.kind != "original_collision" {
                return Err(failure());
            }
            let member: OriginalMember =
                serde_json::from_value(context.details.clone()).map_err(|_| failure())?;
            if MediaRole::for_filename(path.as_str()) != Some(member.role)
                || path.as_str().starts_with(".photo-sorter/")
            {
                return Err(failure());
            }
            let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM members WHERE path=? COLLATE NOCASE UNION ALL SELECT 1 FROM import_items WHERE destination=? COLLATE NOCASE AND state<>'conflict')",params![path.as_str(),path.as_str()],|r|r.get(0))?;
            if exists {
                return Err(failure());
            }
        }
        ConflictResolution::AssociateGroups { groups }
        | ConflictResolution::KeepIndependent { groups } => {
            if context.kind != "association" || groups.is_empty() || groups.len() > 128 {
                return Err(failure());
            }
            let allowed: Vec<GroupId> =
                serde_json::from_value(context.details.get("groups").cloned().ok_or_else(failure)?)
                    .map_err(|_| failure())?;
            let a = groups
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            let b = allowed
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>();
            if a.len() != groups.len() || a != b || !a.is_subset(&selected) {
                return Err(failure());
            }
        }
        ConflictResolution::SelectPrimaryRaw { member } => {
            if !matches!(context.kind.as_str(), "primary_raw" | "association") {
                return Err(failure());
            }
            let group = context.group.ok_or_else(failure)?;
            let matches: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM members WHERE group_id=? AND path=? AND role=?)",
                params![group.to_string(), member.as_str(), encode(&MediaRole::Raw)?],
                |r| r.get(0),
            )?;
            if !matches {
                return Err(failure());
            }
        }
        ConflictResolution::SelectRatingAuthority { asset } => {
            if context.kind != "rating_authority" {
                return Err(failure());
            }
            let server = context
                .details
                .get("server")
                .and_then(|v| v.as_str())
                .ok_or_else(failure)?;
            let group = context.group.ok_or_else(failure)?;
            let matches:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM mappings WHERE group_id=? AND server=? AND asset=? AND stale=0)",params![group.to_string(),server,asset.to_string()],|r|r.get(0))?;
            if !matches {
                return Err(failure());
            }
        }
        ConflictResolution::AssociateSidecar { sidecar, group } => {
            check_sidecar(&context, *sidecar)?;
            if !selected.contains(group) {
                return Err(failure());
            }
            super::identity::canonical(db, *group)?;
        }
        ConflictResolution::RetainUnassociated { sidecar } => {
            check_sidecar(&context, *sidecar)?;
        }
    }
    if !matches!(request.resolution, ConflictResolution::ChooseHead { .. })
        && !request.expected_heads.is_empty()
    {
        return Err(failure());
    }
    Ok(context)
}
fn check_sidecar(context: &Context, sidecar: SidecarId) -> AppResult<()> {
    if context.kind != "orphan_sidecar"
        || context.details.get("sidecar") != Some(&serde_json::to_value(sidecar)?)
    {
        return Err(failure());
    }
    Ok(())
}
pub(crate) fn apply_resolution(db: &Connection, request: ResolutionRequest) -> AppResult<()> {
    let prior: Option<String> = db
        .query_row(
            "SELECT request FROM resolution_receipts WHERE operation=?",
            [request.operation.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(prior) = prior {
        if prior == encode(&request)? {
            return Ok(());
        }
        return Err(failure());
    }
    let context = validate(db, &request)?;
    match &request.resolution {
        ConflictResolution::ChooseHead { version } => {
            let (sidecar, body): (String, String) = db.query_row(
                "SELECT sidecar,body FROM versions WHERE id=?",
                [version.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let derived = VersionId(request.operation.0);
            db.execute(
                "INSERT INTO versions(id,sidecar,body,origin,operation) VALUES(?,?,?,?,?)",
                params![
                    derived.to_string(),
                    sidecar,
                    body,
                    encode(&VersionOrigin::Derived)?,
                    request.operation.to_string()
                ],
            )?;
            for parent in &request.expected_heads {
                db.execute(
                    "INSERT INTO version_parents(version,parent) VALUES(?,?)",
                    params![derived.to_string(), parent.to_string()],
                )?;
            }
            db.execute("DELETE FROM heads WHERE sidecar=?", [&sidecar])?;
            db.execute(
                "INSERT INTO heads(sidecar,version) VALUES(?,?)",
                params![sidecar, derived.to_string()],
            )?;
            db.execute(
                "INSERT INTO archived_versions(version) VALUES(?)",
                [derived.to_string()],
            )?;
            db.execute("INSERT INTO projections(sidecar,pending) VALUES(?,1) ON CONFLICT(sidecar) DO UPDATE SET pending=1",[sidecar])?;
        }
        ConflictResolution::KeepAlternates => {}
        ConflictResolution::AdoptEqual { digest } => {
            let member: OriginalMember = serde_json::from_value(context.details.clone())?;
            db.execute("UPDATE import_items SET archived=1,state='archived' WHERE group_id=? AND destination=? AND digest=?",params![context.group.map(|g|g.to_string()),member.path.as_str(),digest.as_str()])?;
        }
        ConflictResolution::ChooseDistinctPath { path } => {
            let member: OriginalMember = serde_json::from_value(context.details.clone())?;
            let mut query = db.prepare("SELECT id FROM import_items WHERE group_id=? AND destination=? AND digest=? AND state='conflict' AND archived=0")?;
            let items = query
                .query_map(
                    params![
                        context.group.map(|g| g.to_string()),
                        member.path.as_str(),
                        member.digest.as_str()
                    ],
                    |r| r.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            for item in items {
                let replacement = JobId::new().to_string();
                db.execute("INSERT INTO retired_import_plans(item,replacement,operation,previous,plan) SELECT id,?,?,json_object('destination',destination,'local_ready',local_ready,'digest',digest,'size',size),(SELECT plan FROM upload_plans WHERE item=import_items.id) FROM import_items WHERE id=?",params![replacement,request.operation.to_string(),item])?;
                db.execute("DELETE FROM upload_plans WHERE item=?", [&item])?;
                db.execute("UPDATE import_items SET id=?,destination=?,state='queued',local_ready=0 WHERE id=?",params![replacement,path.as_str(),item])?;
                let attached=db.execute("INSERT INTO members(archive,path,group_id,digest,role) SELECT coalesce((SELECT id FROM archives ORDER BY id LIMIT 1),'unbound'),destination,group_id,digest,? FROM import_items WHERE id=? ON CONFLICT DO NOTHING",params![encode(&member.role)?,replacement])?;
                if attached > 0 {
                    db.execute(
                        "UPDATE groups SET revision=revision+1 WHERE id=?",
                        [context.group.map(|g| g.to_string())],
                    )?;
                }
            }
            let changed = db.execute("DELETE FROM members WHERE group_id=? AND path=? AND digest=? AND NOT EXISTS(SELECT 1 FROM placements p WHERE p.path=members.path AND p.digest=members.digest AND p.archived=1)",params![context.group.map(|g|g.to_string()),member.path.as_str(),member.digest.as_str()])?;
            db.execute(
                "DELETE FROM placements WHERE group_id=? AND path=? AND digest=? AND archived=0",
                params![
                    context.group.map(|g| g.to_string()),
                    member.path.as_str(),
                    member.digest.as_str()
                ],
            )?;
            if changed > 0 {
                db.execute("UPDATE groups SET revision=revision+1,primary_raw=CASE WHEN primary_raw=? THEN NULL ELSE primary_raw END WHERE id=?",params![member.path.as_str(),context.group.map(|g|g.to_string())])?;
                db.execute(
                    "UPDATE residency SET ready_revision=NULL,protected=1 WHERE group_id=?",
                    [context.group.map(|g| g.to_string())],
                )?;
            }
            if let Some(group) = context.group {
                super::identity::infer_primary_raw(db, group)?;
            }
        }
        ConflictResolution::AssociateGroups { groups } => {
            super::identity::merge(
                db,
                groups,
                &PairEvidence::UserSelected {
                    reason: format!("explicit association {}", request.operation),
                },
            )?;
        }
        ConflictResolution::KeepIndependent { groups } => {
            for group in groups {
                db.execute("INSERT INTO independent_groups(group_id,operation) VALUES(?,?) ON CONFLICT(group_id) DO UPDATE SET operation=excluded.operation",params![group.to_string(),request.operation.to_string()])?;
            }
        }
        ConflictResolution::SelectPrimaryRaw { member } => {
            db.execute("INSERT INTO primary_raw_choices(group_id,path,operation) VALUES(?,?,?) ON CONFLICT(group_id) DO UPDATE SET path=excluded.path,operation=excluded.operation",params![context.group.map(|g|g.to_string()),member.as_str(),request.operation.to_string()])?;
            db.execute(
                "UPDATE groups SET primary_raw=?,revision=revision+1 WHERE id=?",
                params![member.as_str(), context.group.map(|g| g.to_string())],
            )?;
        }
        ConflictResolution::SelectRatingAuthority { asset } => {
            let server = context
                .details
                .get("server")
                .and_then(|v| v.as_str())
                .ok_or_else(failure)?;
            db.execute("INSERT INTO rating_authorities(group_id,server,asset) VALUES(?,?,?) ON CONFLICT(group_id) DO UPDATE SET server=excluded.server,asset=excluded.asset",params![context.group.map(|g|g.to_string()),server,asset.to_string()])?;
        }
        ConflictResolution::AssociateSidecar { sidecar, group } => {
            db.execute(
                "UPDATE sidecars SET group_id=? WHERE id=?",
                params![group.to_string(), sidecar.to_string()],
            )?;
        }
        ConflictResolution::RetainUnassociated { sidecar } => {
            db.execute("INSERT INTO retained_companions(sidecar,operation) VALUES(?,?) ON CONFLICT(sidecar) DO UPDATE SET operation=excluded.operation",params![sidecar.to_string(),request.operation.to_string()])?;
        }
    }
    if !matches!(request.resolution, ConflictResolution::KeepAlternates) {
        db.execute(
            "UPDATE conflicts SET resolved=1,revision=revision+1 WHERE id=?",
            [request.conflict.to_string()],
        )?;
        if let Some(group) = context.group {
            let group = super::identity::canonical(db, group)?;
            db.execute("UPDATE residency SET conflict=EXISTS(SELECT 1 FROM conflicts WHERE group_id=? AND resolved=0) WHERE group_id=?",params![group.to_string(),group.to_string()])?;
        }
    }
    for (group, _) in &request.expected_memberships {
        let canonical = super::identity::canonical(db, *group)?;
        db.execute("UPDATE residency SET conflict=EXISTS(SELECT 1 FROM conflicts WHERE group_id=? AND resolved=0) WHERE group_id=?",params![canonical.to_string(),canonical.to_string()])?;
    }
    refresh_association_flags(db)?;
    for (group, _) in &request.expected_memberships {
        crate::storage::refresh_group_protection(db, super::identity::canonical(db, *group)?)?;
    }
    db.execute(
        "INSERT INTO associations(id,evidence) VALUES(?,?) ON CONFLICT DO NOTHING",
        params![
            request.operation.to_string(),
            encode(&PairEvidence::UserSelected {
                reason: encode(&request)?
            })?
        ],
    )?;
    db.execute(
        "INSERT INTO resolution_receipts(operation,request) VALUES(?,?)",
        params![request.operation.to_string(), encode(&request)?],
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{archive::CommitEnvelope, fs::RootPolicy};
    #[test]
    fn explicit_primary_raw_survives_repeated_member_observation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temporary.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let group = catalog.create_group(false).unwrap();
        let mut members = Vec::new();
        for extension in ["ORF", "ORI"] {
            let member = OriginalMember {
                path: ArchivePath::parse(&format!("a.{extension}")).unwrap(),
                digest: Digest::of(extension.as_bytes()),
                size: 3,
                role: MediaRole::Raw,
            };
            catalog
                .attach_member(
                    group,
                    member.clone(),
                    PairEvidence::ObservedPlacement {
                        path: member.path.clone(),
                        digest: member.digest.clone(),
                    },
                )
                .unwrap();
            members.push(member);
        }
        let conflict = catalog
            .record_ambiguity(
                "primary_raw",
                Some(group),
                serde_json::json!({"candidates":members}),
            )
            .unwrap();
        let request = ResolutionRequest {
            operation: OperationId::new(),
            conflict,
            expected_revision: 0,
            expected_heads: vec![],
            expected_memberships: vec![(group, catalog.group(group).unwrap().membership_revision)],
            resolution: ConflictResolution::SelectPrimaryRaw {
                member: members[1].path.clone(),
            },
        };
        apply_resolution(&catalog.db, request).unwrap();
        catalog
            .attach_member(
                group,
                members[0].clone(),
                PairEvidence::ObservedPlacement {
                    path: members[0].path.clone(),
                    digest: members[0].digest.clone(),
                },
            )
            .unwrap();
        assert_eq!(
            catalog.group(group).unwrap().primary_raw,
            Some(members[1].path.clone())
        );
    }
    #[test]
    fn shared_resolution_uses_semantic_evidence_across_private_revisions() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut cat = Catalog::open(&root).unwrap();
        let group = cat.create_group(false).unwrap();
        let conflict = cat
            .record_ambiguity(
                "association",
                Some(group),
                serde_json::json!({"groups":[group]}),
            )
            .unwrap();
        let request = ResolutionRequest {
            operation: OperationId::new(),
            conflict,
            expected_revision: 0,
            expected_heads: vec![],
            expected_memberships: vec![(group, cat.group(group).unwrap().membership_revision)],
            resolution: ConflictResolution::KeepIndependent {
                groups: vec![group],
            },
        };
        let records = cat.resolution_records(&request).unwrap();
        cat.db
            .execute("UPDATE groups SET revision=revision+17", [])
            .unwrap();
        cat.db
            .execute("UPDATE conflicts SET revision=revision+9", [])
            .unwrap();
        let commit =
            CommitEnvelope::new(ArchiveId::new(), WriterId::new(), 1, vec![], records).unwrap();
        cat.apply_commit(&commit).unwrap();
        assert!(cat.conflict(conflict).unwrap().resolved);
        let recorded: String = cat
            .db
            .query_row(
                "SELECT evidence FROM associations WHERE id=?",
                [request.operation.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        let provenance: PairEvidence = decode(recorded).unwrap();
        assert!(
            matches!(provenance,PairEvidence::UserSelected{reason} if reason==encode(&request).unwrap())
        );
        cat.apply_commit(&commit).unwrap();
        assert_eq!(
            cat.db
                .query_row("SELECT request FROM resolution_receipts", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            encode(&request).unwrap()
        );
    }
    #[test]
    fn distinct_path_retires_old_attempt_and_unpublished_membership() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut cat = Catalog::open(&root).unwrap();
        let group = cat.create_group(false).unwrap();
        let member = OriginalMember {
            path: ArchivePath::parse("old.ORF").unwrap(),
            digest: Digest::of(b"raw"),
            size: 3,
            role: MediaRole::Raw,
        };
        cat.attach_member(
            group,
            member.clone(),
            PairEvidence::ObservedPlacement {
                path: member.path.clone(),
                digest: member.digest.clone(),
            },
        )
        .unwrap();
        let parent = cat
            .enqueue(DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::Import {
                    source: temp.path().into(),
                    strict: true,
                },
            })
            .unwrap();
        let item = JobId::new();
        cat.db.execute("INSERT INTO import_items(id,parent,source_root,source_path,digest,size,role,group_id,destination,state,local_ready) VALUES(?,?,'/fixture','raw.ORF',?,3,?,?,'old.ORF','conflict',1)",params![item.to_string(),parent.to_string(),member.digest.as_str(),encode(&crate::import::SourceRole::Original(MediaRole::Raw)).unwrap(),group.to_string()]).unwrap();
        cat.db
            .execute(
                "INSERT INTO upload_plans VALUES(?,'{}')",
                [item.to_string()],
            )
            .unwrap();
        let conflict = cat
            .record_ambiguity(
                "original_collision",
                Some(group),
                serde_json::to_value(&member).unwrap(),
            )
            .unwrap();
        let request = ResolutionRequest {
            operation: OperationId::new(),
            conflict,
            expected_revision: 0,
            expected_heads: vec![],
            expected_memberships: vec![(group, cat.group(group).unwrap().membership_revision)],
            resolution: ConflictResolution::ChooseDistinctPath {
                path: ArchivePath::parse("new.ORF").unwrap(),
            },
        };
        apply_resolution(&cat.db, request).unwrap();
        assert_eq!(
            cat.db
                .query_row("SELECT count(*) FROM upload_plans", [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
        let (id, ready, path): (String, bool, String) = cat
            .db
            .query_row(
                "SELECT id,local_ready,destination FROM import_items",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_ne!(id, item.to_string());
        assert!(!ready);
        assert_eq!(path, "new.ORF");
        assert_eq!(
            cat.group(group).unwrap().members[0].path.as_str(),
            "new.ORF"
        );
        assert_eq!(
            cat.db
                .query_row("SELECT count(*) FROM retired_import_plans", [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn all_typed_resolutions_revalidate_and_preserve_history() {
        for case in 0..10 {
            let temp = tempfile::tempdir().unwrap();
            let state = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
            let mut cat = Catalog::open(&state).unwrap();
            let g = cat.create_group(false).unwrap();
            let h = cat.create_group(false).unwrap();
            let path = ArchivePath::parse("A.ORF").unwrap();
            let digest = Digest::of(b"raw");
            cat.attach_member(
                g,
                OriginalMember {
                    path: path.clone(),
                    digest: digest.clone(),
                    size: 3,
                    role: MediaRole::Raw,
                },
                PairEvidence::UserSelected {
                    reason: "test".into(),
                },
            )
            .unwrap();
            let sidecar = SidecarId::new();
            let v1 = VersionId::new();
            let v2 = VersionId::new();
            let asset = uuid::Uuid::new_v4();
            cat.register_sidecar(
                sidecar,
                None,
                ArchivePath::parse("A.ORF.rrdata").unwrap(),
                "unassociated",
            )
            .unwrap();
            for version in [v1, v2] {
                cat.db
                    .execute(
                        "INSERT INTO versions(id,sidecar,body,origin,operation) VALUES(?,?,?,?,?)",
                        params![
                            version.to_string(),
                            sidecar.to_string(),
                            encode(&VersionBody::Content {
                                digest: Digest::of(version.to_string().as_bytes()),
                                size: 36
                            })
                            .unwrap(),
                            encode(&VersionOrigin::Observed).unwrap(),
                            OperationId::new().to_string()
                        ],
                    )
                    .unwrap();
                cat.db
                    .execute(
                        "INSERT INTO heads(sidecar,version) VALUES(?,?)",
                        params![sidecar.to_string(), version.to_string()],
                    )
                    .unwrap();
            }
            cat.db
                .execute(
                    "INSERT INTO placements(id,group_id,path,digest,archived) VALUES(?,?,?,?,1)",
                    params![
                        OperationId::new().to_string(),
                        g.to_string(),
                        path.as_str(),
                        digest.as_str()
                    ],
                )
                .unwrap();
            cat.db.execute("INSERT INTO mappings(server,asset,path,group_id) VALUES('http://fixture',?,'A.JPG',?)",params![asset.to_string(),g.to_string()]).unwrap();
            let parent = cat
                .enqueue(DurableRequest {
                    operation_id: OperationId::new(),
                    kind: JobKind::Import {
                        source: temp.path().into(),
                        strict: true,
                    },
                })
                .unwrap();
            cat.db.execute("INSERT INTO import_items(id,parent,source_root,source_path,digest,size,role,group_id,destination,state) VALUES(?,?,'/fixture','A.ORF',?,3,?,?,'A.ORF','conflict')",params![JobId::new().to_string(),parent.to_string(),digest.as_str(),encode(&crate::import::SourceRole::Original(MediaRole::Raw)).unwrap(),g.to_string()]).unwrap();
            let member = OriginalMember {
                path: path.clone(),
                digest: digest.clone(),
                size: 3,
                role: MediaRole::Raw,
            };
            let (kind, details, resolution) = match case {
                0 => (
                    "sidecar",
                    serde_json::to_value(vec![v1, v2]).unwrap(),
                    ConflictResolution::ChooseHead { version: v1 },
                ),
                1 => (
                    "sidecar",
                    serde_json::to_value(vec![v1, v2]).unwrap(),
                    ConflictResolution::KeepAlternates,
                ),
                2 => (
                    "original_collision",
                    serde_json::to_value(&member).unwrap(),
                    ConflictResolution::AdoptEqual {
                        digest: digest.clone(),
                    },
                ),
                3 => (
                    "original_collision",
                    serde_json::to_value(&member).unwrap(),
                    ConflictResolution::ChooseDistinctPath {
                        path: ArchivePath::parse("B.ORF").unwrap(),
                    },
                ),
                4 => (
                    "association",
                    serde_json::json!({"groups":[g,h]}),
                    ConflictResolution::AssociateGroups { groups: vec![g, h] },
                ),
                5 => (
                    "association",
                    serde_json::json!({"groups":[g,h]}),
                    ConflictResolution::KeepIndependent { groups: vec![g, h] },
                ),
                6 => (
                    "primary_raw",
                    serde_json::json!({}),
                    ConflictResolution::SelectPrimaryRaw {
                        member: path.clone(),
                    },
                ),
                7 => (
                    "rating_authority",
                    serde_json::json!({"server":"http://fixture"}),
                    ConflictResolution::SelectRatingAuthority { asset },
                ),
                8 => (
                    "orphan_sidecar",
                    serde_json::json!({"sidecar":sidecar}),
                    ConflictResolution::AssociateSidecar { sidecar, group: g },
                ),
                _ => (
                    "orphan_sidecar",
                    serde_json::json!({"sidecar":sidecar}),
                    ConflictResolution::RetainUnassociated { sidecar },
                ),
            };
            let conflict = cat.record_ambiguity(kind, Some(g), details).unwrap();
            let operation = OperationId::new();
            let request = ResolutionRequest {
                operation,
                conflict,
                expected_revision: 0,
                expected_heads: if case == 0 { vec![v1, v2] } else { vec![] },
                expected_memberships: vec![
                    (g, cat.group(g).unwrap().membership_revision),
                    (h, cat.group(h).unwrap().membership_revision),
                ],
                resolution,
            };
            let mut stale = request.clone();
            stale.expected_revision = 1;
            assert!(cat.resolution_records(&stale).is_err());
            let records = cat.resolution_records(&request).unwrap();
            let commit =
                CommitEnvelope::new(ArchiveId::new(), WriterId::new(), 1, vec![], records).unwrap();
            cat.apply_commit(&commit).unwrap();
            cat.apply_commit(&commit).unwrap();
            assert_eq!(cat.conflicts(true).unwrap()[0].resolved, case != 1);
            assert_eq!(cat.version(v1).unwrap().parents, vec![]);
            assert_eq!(cat.version(v2).unwrap().parents, vec![]);
            match case {
                0 => {
                    let heads = cat.heads(sidecar).unwrap();
                    assert_eq!(heads, vec![VersionId(operation.0)]);
                    assert_eq!(cat.version(heads[0]).unwrap().parents.len(), 2);
                }
                1 => assert_eq!(cat.heads(sidecar).unwrap().len(), 2),
                2 => assert!(
                    cat.db
                        .query_row("SELECT archived FROM import_items", [], |r| r
                            .get::<_, bool>(0))
                        .unwrap()
                ),
                3 => assert_eq!(
                    cat.db
                        .query_row("SELECT destination FROM import_items", [], |r| r
                            .get::<_, String>(0))
                        .unwrap(),
                    "B.ORF"
                ),
                4 => assert_eq!(
                    cat.canonical_group(g).unwrap(),
                    cat.canonical_group(h).unwrap()
                ),
                5 => assert_ne!(
                    cat.canonical_group(g).unwrap(),
                    cat.canonical_group(h).unwrap()
                ),
                6 => assert_eq!(cat.group(g).unwrap().primary_raw, Some(path)),
                7 => assert_eq!(
                    cat.db
                        .query_row("SELECT asset FROM rating_authorities", [], |r| r
                            .get::<_, String>(0))
                        .unwrap(),
                    asset.to_string()
                ),
                8 => assert_eq!(
                    cat.db
                        .query_row("SELECT group_id FROM sidecars", [], |r| r
                            .get::<_, String>(0))
                        .unwrap(),
                    g.to_string()
                ),
                _ => assert_eq!(
                    cat.db
                        .query_row("SELECT count(*) FROM retained_companions", [], |r| r
                            .get::<_, u64>(0))
                        .unwrap(),
                    1
                ),
            }
        }
    }
}
