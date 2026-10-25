use crate::{
    catalog::*,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
};
use rusqlite::params;
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvictionIntent {
    pub group: GroupId,
    pub revision: MembershipRevision,
    pub members: Vec<OriginalMember>,
    pub operation: OperationId,
    pub eligibility_activity: Timestamp,
}
#[derive(Default, Debug)]
pub struct EvictionReport {
    pub removed: u64,
    pub retained: u64,
}
pub struct EvictionPlanner;
impl EvictionPlanner {
    pub fn candidates(catalog: &Catalog, now: Timestamp) -> AppResult<Vec<EvictionIntent>> {
        Self::candidates_with_hold(catalog, now, 600)
    }
    pub fn candidates_with_hold(
        catalog: &Catalog,
        now: Timestamp,
        hold: u64,
    ) -> AppResult<Vec<EvictionIntent>> {
        let mut result = Vec::new();
        let mut query=catalog.db.prepare("SELECT g.id FROM groups g JOIN residency r ON r.group_id=g.id WHERE g.id NOT IN(SELECT alias FROM aliases) ORDER BY r.last_activity,g.id")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let id: GroupId = serde_json::from_str(&format!("\"{id}\""))?;
            let group = catalog.group(id)?;
            if eligible(catalog, &group, now, hold)? {
                let activity: u64 = catalog.db.query_row(
                    "SELECT last_activity FROM residency WHERE group_id=?",
                    [group.id.to_string()],
                    |r| r.get(0),
                )?;
                result.push(EvictionIntent {
                    group: group.id,
                    revision: group.membership_revision,
                    members: group.members,
                    operation: OperationId::new(),
                    eligibility_activity: activity,
                });
                if result.len() == 128 {
                    break;
                }
            }
        }

        Ok(result)
    }
}
fn eligible(catalog: &Catalog, group: &Group, now: Timestamp, hold: u64) -> AppResult<bool> {
    eligible_with_residency(catalog, group, now, hold, true)
}
fn eligible_with_residency(
    catalog: &Catalog,
    group: &Group,
    now: Timestamp,
    hold: u64,
    require_resident: bool,
) -> AppResult<bool> {
    if !matches!(
        catalog.publication_readiness()?,
        PublicationReadiness::Ready
    ) {
        return Ok(false);
    }
    let resident: bool = catalog.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM placements WHERE group_id=? AND local_ready=1)",
        [group.id.to_string()],
        |r| r.get(0),
    )?;
    if require_resident && !resident {
        return Ok(false);
    }
    let rating_pending:bool=catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM rating_intents WHERE group_id=? AND state IN('prepared','sent'))",[group.id.to_string()],|r|r.get(0))?;
    if rating_pending {
        return Ok(false);
    }
    let p = catalog.protection(group.id)?;
    if p.pinned || p.pending || p.conflict || group.members.is_empty() {
        return Ok(false);
    }
    let last: u64 = catalog.db.query_row(
        "SELECT last_activity FROM residency WHERE group_id=?",
        [group.id.to_string()],
        |r| r.get(0),
    )?;
    if now.saturating_sub(last) < hold {
        return Ok(false);
    }
    let bad:bool=catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM members m LEFT JOIN placements p ON p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest WHERE m.group_id=? AND (p.archived IS NULL OR p.archived=0))",[group.id.to_string()],|r|r.get(0))?;
    let dirty:bool=catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM sidecars s LEFT JOIN local_heads l ON s.id=l.sidecar LEFT JOIN projections p ON s.id=p.sidecar WHERE s.group_id=? AND (s.protected=1 OR l.dirty=1 OR p.pending=1))",[group.id.to_string()],|r|r.get(0))?;
    Ok(!bad && !dirty)
}
pub fn eviction_still_allowed(catalog: &Catalog, intent: &EvictionIntent) -> AppResult<bool> {
    let group = catalog.group(intent.group)?;
    let activity: u64 = catalog.db.query_row(
        "SELECT last_activity FROM residency WHERE group_id=?",
        [group.id.to_string()],
        |r| r.get(0),
    )?;
    Ok(group.id == intent.group
        && group.membership_revision == intent.revision
        && group.members == intent.members
        && activity == intent.eligibility_activity
        && eligible_with_residency(catalog, &group, u64::MAX, 0, false)?)
}
pub struct Evictor<'a> {
    pub catalog: &'a mut Catalog,
    pub local: &'a RootHandle,
}
impl Evictor<'_> {
    pub fn execute(
        &mut self,
        intent: EvictionIntent,
        faults: &dyn FaultInjector,
    ) -> AppResult<EvictionReport> {
        self.execute_inner(intent, faults, None)
    }
    pub fn execute_verified(
        &mut self,
        intent: EvictionIntent,
        proof: &IntegrityReport,
    ) -> AppResult<EvictionReport> {
        self.execute_inner(intent, &NoFaults, Some(proof))
    }
    fn execute_inner(
        &mut self,
        intent: EvictionIntent,
        faults: &dyn FaultInjector,
        proof: Option<&IntegrityReport>,
    ) -> AppResult<EvictionReport> {
        self.local.require_local()?;
        let group = self.catalog.group(intent.group)?;
        if group.membership_revision != intent.revision
            || group.members != intent.members
            || !eligible_with_residency(self.catalog, &group, u64::MAX, 0, proof.is_none())?
        {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "eviction invalidated by pin, membership or protection",
            ));
        }
        let job = self.catalog.enqueue(DurableRequest {
            operation_id: intent.operation,
            kind: JobKind::Evict {
                group: intent.group,
                revision: intent.revision,
                members: intent.members.clone(),
                activity: intent.eligibility_activity,
            },
        })?;
        let mut report = EvictionReport::default();
        for member in intent.members {
            if !matches!(
                crate::import::source_role(member.path.as_str()),
                Some(crate::import::SourceRole::Original(_))
            ) {
                return Err(AppError::integrity(
                    "only immutable media members are evictable",
                ));
            }
            let current = self.catalog.group(group.id)?;
            if current.membership_revision != intent.revision
                || !eligible_with_residency(self.catalog, &current, u64::MAX, 0, proof.is_none())?
            {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "eviction eligibility changed",
                ));
            }
            faults.hit(FaultPoint::BeforeUnlink)?;
            let activity: u64 = self.catalog.db.query_row(
                "SELECT last_activity FROM residency WHERE group_id=?",
                [group.id.to_string()],
                |r| r.get(0),
            )?;
            if self.catalog.group(group.id)?.membership_revision != intent.revision
                || activity != intent.eligibility_activity
                || !eligible_with_residency(
                    self.catalog,
                    &self.catalog.group(group.id)?,
                    u64::MAX,
                    0,
                    proof.is_none(),
                )?
            {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "eviction invalidated immediately before unlink",
                ));
            }
            let file =
                match self
                    .local
                    .open_relative(&member.path, libc::O_RDONLY | libc::O_NONBLOCK, 0)
                {
                    Ok(file) => file,
                    Err(e) if e.errno == Some(libc::ENOENT) => {
                        self.catalog.db.execute(
                            "UPDATE placements SET local_ready=0 WHERE group_id=? AND path=?",
                            params![group.id.to_string(), member.path.as_str()],
                        )?;
                        continue;
                    }
                    Err(e) => return Err(e),
                };
            let mut file = file;
            let current_stamp = crate::edit::SnapshotStamp::of(&file.metadata()?);
            let verified = match proof {
                Some(proof) => proof
                    .verified
                    .iter()
                    .any(|(path, stamp)| path == &member.path && stamp == &current_stamp),
                None => crate::transfer::digest_file(&mut file)? == (member.digest, member.size),
            };
            if !verified {
                return Err(AppError::integrity("changed local original retained"));
            }
            self.local.unlink(&member.path, false)?;
            self.catalog.db.execute(
                "UPDATE placements SET local_ready=0 WHERE group_id=? AND path=?",
                params![group.id.to_string(), member.path.as_str()],
            )?;
            report.removed += 1;
            faults.hit(FaultPoint::UnlinkMember)?;
        }
        let generation = self.catalog.job(job)?.generation;
        self.catalog
            .transition(job, generation, JobTransition::Complete)?;
        Ok(report)
    }
}
impl Catalog {
    pub fn refresh_protection(&mut self, group: GroupId) -> AppResult<()> {
        let group = self.canonical_group(group)?;
        refresh_group_protection(&self.db, group)
    }
}
pub(crate) fn refresh_group_protection(db: &rusqlite::Connection, group: GroupId) -> AppResult<()> {
    let originals:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM members m LEFT JOIN placements p ON p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest WHERE m.group_id=? AND (p.archived IS NULL OR p.archived=0))",[group.to_string()],|r|r.get(0))?;
    let edits: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sidecars s LEFT JOIN local_heads l ON l.sidecar=s.id LEFT JOIN projections p ON p.sidecar=s.id WHERE s.group_id=? AND (s.protected=1 OR l.dirty=1 OR p.pending=1 OR EXISTS(SELECT 1 FROM versions v WHERE v.sidecar=s.id AND NOT EXISTS(SELECT 1 FROM archived_versions a WHERE a.version=v.id)) OR EXISTS(SELECT 1 FROM sidecar_diagnostics d WHERE d.sidecar=s.id)))",
            [group.to_string()],
            |r| r.get(0),
        )?;
    db.execute("UPDATE residency SET metadata_current=NOT EXISTS(SELECT 1 FROM sidecars s LEFT JOIN projections p ON p.sidecar=s.id LEFT JOIN local_heads l ON l.sidecar=s.id WHERE s.group_id=? AND s.owner<>'jpeg-xmp-external' AND (p.pending=1 OR p.sidecar IS NULL OR l.version IS NULL OR l.dirty=1 OR (SELECT count(*) FROM heads h WHERE h.sidecar=s.id)<>1 OR NOT EXISTS(SELECT 1 FROM heads h WHERE h.sidecar=s.id AND h.version=l.version))) WHERE group_id=?",params![group.to_string(),group.to_string()])?;
    db.execute(
            "UPDATE residency SET protected=? WHERE group_id=?",
            params![originals || edits || db.query_row("SELECT EXISTS(SELECT 1 FROM physical_findings WHERE group_id=? AND status<>'Missing') OR EXISTS(SELECT 1 FROM startup_checks WHERE group_id=?)",params![group.to_string(),group.to_string()],|r|r.get::<_,bool>(0))?, group.to_string()],
        )?;
    Ok(())
}
