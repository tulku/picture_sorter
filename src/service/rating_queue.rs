use crate::{catalog::Catalog, domain::RatingIntentId, error::AppResult};
use rusqlite::OptionalExtension;
pub(crate) fn next_intent(
    catalog: &Catalog,
    after: i64,
) -> AppResult<Option<(i64, RatingIntentId)>> {
    let row:Option<(i64,String)>=catalog.db.query_row("SELECT r.rowid,r.id FROM rating_intents r WHERE r.state IN('prepared','sent') AND NOT EXISTS(SELECT 1 FROM rating_intents prior WHERE prior.group_id=r.group_id AND prior.rowid<r.rowid AND prior.state IN('prepared','sent')) ORDER BY r.rowid<=?,r.rowid LIMIT 1",[after],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    row.map(|(row, id)| Ok((row, serde_json::from_value(id.into())?)))
        .transpose()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{domain::*, fs::*};
    #[test]
    fn blocked_group_does_not_starve_later_groups_or_reorder_its_own_intents() {
        let temp = tempfile::tempdir().unwrap();
        let catalog =
            Catalog::open(&RootHandle::open(temp.path(), RootPolicy::Local).unwrap()).unwrap();
        let first = GroupId::new();
        let second = GroupId::new();
        let mut ids = vec![];
        for group in [first, first, second] {
            let id = RatingIntentId::new();
            ids.push(id);
            catalog.db.execute("INSERT INTO rating_intents(id,operation,group_id,server,asset,jpeg,expected,desired,state) VALUES(?,?,?,'http://fixture','asset','{}','{}',3,'prepared')",rusqlite::params![id.to_string(),OperationId::new().to_string(),group.to_string()]).unwrap();
        }
        let (row, id) = next_intent(&catalog, 0).unwrap().unwrap();
        assert_eq!(id, ids[0]);
        let (row, id) = next_intent(&catalog, row).unwrap().unwrap();
        assert_eq!(id, ids[2]);
        let (_, id) = next_intent(&catalog, row).unwrap().unwrap();
        assert_eq!(id, ids[0]);
    }
}
