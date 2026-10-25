use crate::{
    catalog::Catalog,
    domain::*,
    error::{AppError, AppResult},
    fs::RootHandle,
};
#[derive(Default, Debug)]
pub struct GcReport {
    pub removed: u64,
    pub bytes: u64,
}
pub struct SnapshotGc<'a> {
    pub state: &'a RootHandle,
}
impl SnapshotGc<'_> {
    pub fn collect(&self, catalog: &mut Catalog) -> AppResult<GcReport> {
        let mut result = GcReport::default();
        let mut q=catalog.db.prepare("SELECT digest,path,size FROM snapshots s WHERE archived=1 AND refs=0 AND NOT EXISTS(SELECT 1 FROM jobs WHERE json_extract(state,'$') NOT IN('Complete','Cancelled')) AND NOT EXISTS(SELECT 1 FROM conflicts WHERE resolved=0) AND NOT EXISTS(SELECT 1 FROM projections WHERE pending=1) AND NOT EXISTS(SELECT 1 FROM local_heads l JOIN versions v ON v.id=l.version WHERE json_extract(v.body,'$.Content.digest')=s.digest) LIMIT 128")?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);
        for (digest, path, size) in rows {
            let path = ArchivePath::parse(&path)?;
            if !path.as_str().starts_with(".photo-sorter/snapshots/") {
                return Err(AppError::integrity("invalid local snapshot ownership"));
            }
            let mut file = self.state.open_relative(&path, libc::O_RDONLY, 0)?;
            if crate::transfer::digest_file(&mut file)? != (Digest::parse(&digest)?, size) {
                return Err(AppError::integrity("unknown snapshot bytes retained"));
            }
            catalog.db.execute(
                "UPDATE snapshots SET cleanup_intent=1 WHERE digest=?",
                [&digest],
            )?;
            self.state.unlink(&path, false)?;
            catalog
                .db
                .execute("DELETE FROM snapshots WHERE digest=?", [&digest])?;
            result.removed += 1;
            result.bytes += size;
        }
        Ok(result)
    }
}
