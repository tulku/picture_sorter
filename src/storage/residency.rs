use crate::{
    catalog::*,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FilesystemId(pub u64);
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Readiness {
    pub revision: MembershipRevision,
    pub originals_ready: bool,
    pub metadata_current: bool,
    pub waiting_reason: Option<String>,
}
impl Catalog {
    pub fn request_pin(&mut self, group: GroupId, operation: OperationId) -> AppResult<JobId> {
        let group = self.canonical_group(group)?;
        let request = DurableRequest {
            operation_id: operation,
            kind: JobKind::Pin { group },
        };
        let encoded = serde_json::to_string(&request)?;
        let tx = self.db.transaction()?;
        let previous: Option<(String, String)> = tx
            .query_row(
                "SELECT id,request FROM jobs WHERE operation=?",
                [operation.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let id = if let Some((id, old)) = previous {
            if old != encoded {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "operation reused with different pin",
                ));
            }
            return serde_json::from_str(&format!("\"{id}\"")).map_err(Into::into);
        } else {
            let id = JobId::new();
            tx.execute(
                "INSERT INTO jobs(id,operation,request,state) VALUES(?,?,?,?)",
                params![
                    id.to_string(),
                    operation.to_string(),
                    encoded,
                    serde_json::to_string(&JobState::Queued)?
                ],
            )?;
            id
        };
        tx.execute(
            "UPDATE residency SET pin=1 WHERE group_id=?",
            [group.to_string()],
        )?;
        tx.commit()?;
        Ok(id)
    }
    pub fn unpin(&mut self, group: GroupId) -> AppResult<()> {
        let group = self.canonical_group(group)?;
        self.db.execute(
            "UPDATE residency SET pin=0 WHERE group_id=?",
            [group.to_string()],
        )?;
        Ok(())
    }
    pub fn reserve(
        &mut self,
        job: JobId,
        filesystem: FilesystemId,
        remaining: u64,
    ) -> AppResult<ReservationId> {
        let record = self.job(job)?;
        if record.cancel_requested
            || matches!(record.state, JobState::Complete | JobState::Cancelled)
        {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "cannot reserve for cancelled or complete job",
            ));
        }
        let id = ReservationId::new();
        self.db.execute(
            "INSERT INTO reservations(id,job,filesystem,remaining) VALUES(?,?,?,?)",
            params![
                id.to_string(),
                job.to_string(),
                filesystem.0.to_string(),
                remaining
            ],
        )?;
        Ok(id)
    }
    pub fn remaining_reservations(&self, filesystem: FilesystemId) -> AppResult<u64> {
        let mut q = self
            .db
            .prepare("SELECT remaining FROM reservations WHERE filesystem=?")?;
        let mut sum = 0u64;
        for row in q.query_map([filesystem.0.to_string()], |r| r.get::<_, u64>(0))? {
            sum = sum
                .checked_add(row?)
                .ok_or_else(|| AppError::invalid("reservation overflow"))?;
        }
        Ok(sum)
    }
    pub fn update_reservation(&mut self, id: ReservationId, remaining: u64) -> AppResult<()> {
        if self.db.execute(
            "UPDATE reservations SET remaining=? WHERE id=?",
            params![remaining, id.to_string()],
        )? != 1
        {
            return Err(AppError::invalid("unknown reservation"));
        }
        Ok(())
    }
    pub fn record_materialization(
        &mut self,
        group: GroupId,
        member: &OriginalMember,
        archived: bool,
    ) -> AppResult<()> {
        let group = self.group(group)?;
        if !group.members.contains(member) {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "membership changed before materialization receipt",
            ));
        }
        self.db.execute("INSERT INTO placements(id,group_id,path,digest,local_ready,archived) VALUES(?,?,?,?,1,?) ON CONFLICT(path,group_id) DO UPDATE SET local_ready=1,archived=max(archived,excluded.archived)",params![OperationId::new().to_string(),group.id.to_string(),member.path.as_str(),member.digest.as_str(),archived])?;
        self.db.execute(
            "UPDATE residency SET last_activity=? WHERE group_id=?",
            params![SystemClock::default().now(), group.id.to_string()],
        )?;
        Ok(())
    }
    pub fn readiness(&self, group: GroupId) -> AppResult<Readiness> {
        let group = self.group(group)?;
        let missing:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM members m LEFT JOIN placements p ON p.group_id=m.group_id AND p.path=m.path AND p.digest=m.digest WHERE m.group_id=? AND (p.id IS NULL OR p.local_ready=0))",[group.id.to_string()],|r|r.get(0))?;
        let protection = self.protection(group.id)?;
        let originals_ready = !group.members.is_empty() && !missing;
        let no_sidecars: bool = self.db.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM sidecars WHERE group_id=?)",
            [group.id.to_string()],
            |r| r.get(0),
        )?;
        let metadata_current = (protection.metadata_current || no_sidecars) && !protection.conflict;
        let reason = if protection.conflict {
            Some("unresolved conflict".into())
        } else if !originals_ready {
            Some("originals are not resident".into())
        } else if !metadata_current {
            Some("metadata requires editor-closed refresh".into())
        } else {
            None
        };
        Ok(Readiness {
            revision: group.membership_revision,
            originals_ready,
            metadata_current,
            waiting_reason: reason,
        })
    }
}
