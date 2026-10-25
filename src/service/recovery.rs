use crate::{catalog::Catalog, error::AppResult};
use serde::Serialize;
#[derive(Debug, Default, Serialize)]
pub struct RecoveryReport {
    pub retained_reservations: u64,
    pub unfinished_jobs: u64,
    pub uncertain_attempts: u64,
}
pub struct ServiceRecovery;
impl ServiceRecovery {
    pub fn restore(catalog: &mut Catalog) -> AppResult<RecoveryReport> {
        let retained_reservations =
            catalog
                .db
                .query_row("SELECT count(*) FROM reservations", [], |r| r.get(0))?;
        let unfinished_jobs=catalog.db.query_row("SELECT count(*) FROM jobs WHERE json_extract(state,'$') NOT IN('Complete','Cancelled')",[],|r|r.get(0))?;
        let uncertain_attempts = catalog.db.query_row(
            "SELECT count(*) FROM attempts WHERE json_extract(state,'$')='Running'",
            [],
            |r| r.get(0),
        )?;
        catalog.db.execute("UPDATE residency SET ready_revision=NULL,metadata_current=0,protected=1 WHERE group_id IN(SELECT group_id FROM placements WHERE local_ready=1)",[])?;
        Ok(RecoveryReport {
            retained_reservations,
            unfinished_jobs,
            uncertain_attempts,
        })
    }
}
