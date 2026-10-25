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
impl Catalog {
    pub fn record_ambiguity(
        &mut self,
        kind: &str,
        group: Option<GroupId>,
        details: serde_json::Value,
    ) -> AppResult<ConflictId> {
        let id = ConflictId::new();
        let group = group.map(|g| self.canonical_group(g)).transpose()?;
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
    pub fn resolution_records(&self, request: &ResolutionRequest) -> AppResult<Vec<ArchiveRecord>> {
        validate(&self.db, request)?;
        Ok(vec![ArchiveRecord::AmbiguityResolved {
            request: request.clone(),
        }])
    }
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
            let mut recorded: Vec<VersionId> =
                serde_json::from_value(context.details.clone()).map_err(|_| failure())?;
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
            db.execute("UPDATE import_items SET destination=?,state='queued' WHERE group_id=? AND destination=? AND digest=? AND state='conflict'",params![path.as_str(),context.group.map(|g|g.to_string()),member.path.as_str(),member.digest.as_str()])?;
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
            db.execute(
                "UPDATE groups SET primary_raw=? WHERE id=?",
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
