use super::*;
use crate::domain::*;
use rusqlite::{OptionalExtension, params};
impl Catalog {
    pub fn enqueue(&mut self, request: DurableRequest) -> AppResult<JobId> {
        let tx = self.db.transaction()?;
        let operation = request.operation_id.to_string();
        let encoded = encode(&request)?;
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT id,request FROM jobs WHERE operation=?",
                [&operation],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, old)) = existing {
            if old != encoded {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "operation ID reused with different request",
                ));
            }
            return decode(format!("\"{id}\""));
        }
        let id = JobId::new();
        tx.execute(
            "INSERT INTO jobs(id,operation,request,state) VALUES(?,?,?,?)",
            params![
                id.to_string(),
                operation,
                encoded,
                encode(&JobState::Queued)?
            ],
        )?;
        tx.commit()?;
        Ok(id)
    }
    pub fn job(&self, id: JobId) -> AppResult<JobRecord> {
        let row=self.db.query_row("SELECT request,state,generation,local_ready,archive_committed,reason,cancel_requested FROM jobs WHERE id=?",[id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,u64>(2)?,r.get::<_,bool>(3)?,r.get::<_,bool>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,bool>(6)?)))?;
        Ok(JobRecord {
            id,
            request: decode(row.0)?,
            state: decode(row.1)?,
            generation: row.2,
            local_ready: row.3,
            archive_committed: row.4,
            reason: row.5,
            cancel_requested: row.6,
        })
    }
    pub fn jobs(&self) -> AppResult<Vec<JobRecord>> {
        let mut query = self.db.prepare("SELECT id FROM jobs ORDER BY rowid")?;
        let ids = query
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|s| self.job(decode(format!("\"{s}\""))?))
            .collect()
    }
    pub fn transition(
        &mut self,
        id: JobId,
        expected_generation: u64,
        change: JobTransition,
    ) -> AppResult<()> {
        let old = self.job(id)?;
        if old.generation != expected_generation
            || matches!(old.state, JobState::Complete | JobState::Cancelled)
        {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "obsolete attempt or terminal job",
            ));
        }
        if old.cancel_requested
            && !matches!(
                change,
                JobTransition::ConfirmCancelled | JobTransition::RequestCancel
            )
        {
            return Err(AppError::new(
                ErrorCode::Cancelled,
                "cancellation pending; retain reservations until stopped",
            ));
        }
        let mut generation = old.generation;
        let mut state = old.state;
        let mut local = old.local_ready;
        let mut archived = old.archive_committed;
        let mut reason = None;
        let mut cancel = old.cancel_requested;
        match change {
            JobTransition::Start => {
                generation = generation
                    .checked_add(1)
                    .ok_or_else(|| AppError::invalid("generation overflow"))?;
                state = JobState::Running;
            }
            JobTransition::Wait(next, why) => {
                if !matches!(
                    next,
                    JobState::WaitingSource
                        | JobState::WaitingNas
                        | JobState::WaitingCapacity
                        | JobState::WaitingApplication
                        | JobState::Conflict
                        | JobState::Failed
                ) {
                    return Err(AppError::invalid("invalid waiting transition"));
                }
                state = next;
                reason = Some(why);
            }
            JobTransition::Ingested => local = true,
            JobTransition::Archived => {
                archived = true;
                local = true;
            }
            JobTransition::Complete => state = JobState::Complete,
            JobTransition::RequestCancel => {
                cancel = true;
                state = JobState::CancelRequested;
            }
            JobTransition::ConfirmCancelled => {
                if !cancel {
                    return Err(AppError::invalid("cancellation not requested"));
                }
                state = JobState::Cancelled;
            }
        }
        let tx = self.db.transaction()?;
        let changed=tx.execute("UPDATE jobs SET state=?,generation=?,local_ready=?,archive_committed=?,reason=?,cancel_requested=? WHERE id=? AND generation=?",params![encode(&state)?,generation,local,archived,reason,cancel,id.to_string(),expected_generation])?;
        if changed != 1 {
            return Err(AppError::new(ErrorCode::Conflict, "obsolete generation"));
        }
        if matches!(state, JobState::Running) {
            tx.execute(
                "INSERT INTO attempts(job,generation,state) VALUES(?,?,?)",
                params![id.to_string(), generation, encode(&state)?],
            )?;
        }
        if matches!(state, JobState::Cancelled) {
            tx.execute("DELETE FROM reservations WHERE job=?", [id.to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }
}
