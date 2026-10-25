use super::*;
use crate::{archive::*, domain::*};
use rusqlite::{OptionalExtension, params};
impl Catalog {
    pub fn prepare_publication(
        &mut self,
        archive: ArchiveId,
        writer: WriterId,
        operation: OperationId,
        records: Vec<ArchiveRecord>,
    ) -> AppResult<PublicationPlan> {
        let old: Option<String> = self
            .db
            .query_row(
                "SELECT plan FROM publication_plans WHERE operation=?",
                [operation.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            return decode(old);
        }
        let tx = self.db.transaction()?;
        let seen:u64=tx.query_row("SELECT coalesce(max(json_extract(envelope,'$.writer_sequence')),0) FROM commits WHERE json_extract(envelope,'$.writer_id')=?",[writer.to_string()],|r|r.get(0))?;
        let sequence:u64=tx.query_row("INSERT INTO writer_sequences(writer,sequence) VALUES(?,?) ON CONFLICT(writer) DO UPDATE SET sequence=max(sequence+1,excluded.sequence) RETURNING sequence",params![writer.to_string(),seen+1],|r|r.get(0))?;
        let mut query=tx.prepare("SELECT id FROM commits WHERE applied=1 AND NOT EXISTS(SELECT 1 FROM dependencies d JOIN commits child ON child.id=d.commit_id WHERE d.dependency=commits.id AND child.applied=1) ORDER BY id LIMIT 129")?;
        let dependencies = query
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| decode(format!("\"{}\"", r?)))
            .collect::<AppResult<Vec<CommitId>>>()?;
        drop(query);
        if dependencies.len() > 128 {
            return Err(AppError::new(
                ErrorCode::Unavailable,
                "journal frontier exceeds one bounded publication; compact causal dependencies before proceeding",
            ));
        }
        let plan = PublicationPlan {
            commit: CommitEnvelope::new(archive, writer, sequence, dependencies, records)?,
            attempt: operation,
            staged: vec![],
        };
        tx.execute(
            "INSERT INTO publication_plans(operation,plan) VALUES(?,?)",
            params![operation.to_string(), encode(&plan)?],
        )?;
        tx.commit()?;
        Ok(plan)
    }
}
