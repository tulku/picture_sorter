use super::*;
use crate::{
    archive::{ArchiveRecord, CommitEnvelope, ProjectionPlan},
    domain::*,
};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeSet;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GeneratedRetentionPlan {
    pub commit: CommitEnvelope,
    pub authorization: GeneratedRetentionAuthorization,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GeneratedRetentionReceipt {
    pub commit: CommitEnvelope,
    pub removed_digests: Vec<Digest>,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GeneratedRetentionAuthorization {
    current: ProjectionPlan,
    versions: Vec<Version>,
    removable: Vec<Digest>,
    projections: Vec<ProjectionPlan>,
    archive: ArchiveId,
    publication: CommitEnvelope,
}
impl GeneratedRetentionAuthorization {
    pub fn current(&self) -> &ProjectionPlan {
        &self.current
    }
    pub fn versions(&self) -> &[Version] {
        &self.versions
    }
    pub fn removable(&self) -> &[Digest] {
        &self.removable
    }
    pub fn projections(&self) -> &[ProjectionPlan] {
        &self.projections
    }
    pub fn archive(&self) -> ArchiveId {
        self.archive
    }
    pub fn publication(&self) -> &CommitEnvelope {
        &self.publication
    }
}

fn validate_current(db: &Connection, current: &ProjectionPlan) -> AppResult<()> {
    validate_saved_path(&current.path, &current.owner)?;
    let identity: Option<(Option<String>, String, String)> = db
        .query_row(
            "SELECT group_id,path,owner FROM sidecars WHERE id=?",
            [current.sidecar.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((group, path, owner)) = identity else {
        return Err(AppError::integrity("missing generated retention owner"));
    };
    if owner != GENERATED_OUTPUT_OWNER
        || current.owner != owner
        || current.path.as_str() != path
        || super::replay::heads(db, current.sidecar)? != vec![current.version]
    {
        return Err(AppError::integrity(
            "retention requires the singleton generated head",
        ));
    }
    super::generated::validate_generated_identity(
        db,
        current.sidecar,
        group.map(|g| g.parse()).transpose()?,
        &current.path,
    )?;
    let body: String = db.query_row(
        "SELECT body FROM versions WHERE id=? AND sidecar=?",
        params![current.version.to_string(), current.sidecar.to_string()],
        |r| r.get(0),
    )?;
    let committed: Option<String> = db
        .query_row(
            "SELECT committed FROM projections WHERE sidecar=?",
            [current.sidecar.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if decode::<VersionBody>(body)? != current.body
        || committed
            .map(decode::<ProjectionPlan>)
            .transpose()?
            .as_ref()
            != Some(current)
        || !matches!(current.body, VersionBody::Content { .. })
    {
        return Err(AppError::integrity(
            "retention current projection does not match saved bytes",
        ));
    }
    Ok(())
}
fn validate_versions(
    db: &Connection,
    current: &ProjectionPlan,
    versions: &[VersionId],
) -> AppResult<()> {
    if versions.is_empty()
        || versions.len() > 128
        || versions.iter().collect::<BTreeSet<_>>().len() != versions.len()
    {
        return Err(AppError::integrity(
            "retirement batch must name 1..128 distinct versions",
        ));
    }
    for id in versions {
        let eligible:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM versions v JOIN archived_versions a ON a.version=v.id WHERE v.id=?1 AND v.sidecar=?2 AND NOT EXISTS(SELECT 1 FROM heads h WHERE h.version=v.id))",params![id.to_string(),current.sidecar.to_string()],|r|r.get(0))?;
        if !eligible || *id == current.version {
            return Err(AppError::integrity(
                "retirement references current, foreign or unarchived version",
            ));
        }
    }
    Ok(())
}
pub(crate) fn apply_retirement(
    db: &Connection,
    commit_id: &str,
    current: &ProjectionPlan,
    versions: &[VersionId],
) -> AppResult<()> {
    validate_current(db, current)?;
    validate_versions(db, current, versions)?;
    let envelope: Vec<u8> = db.query_row(
        "SELECT envelope FROM commits WHERE id=?",
        [commit_id],
        |r| r.get(0),
    )?;
    db.execute("INSERT INTO generated_retirement_batches(commit_id,current,versions,envelope) VALUES(?,?,?,?) ON CONFLICT DO NOTHING",params![commit_id,encode(current)?,encode(&versions)?,envelope])?;
    for id in versions {
        db.execute(
            "INSERT INTO generated_retired_versions(version) VALUES(?) ON CONFLICT DO NOTHING",
            [id.to_string()],
        )?;
    }
    Ok(())
}

impl Catalog {
    pub fn generated_retention_candidate(&self) -> AppResult<Option<(SidecarId, VersionId)>> {
        if !matches!(self.publication_readiness()?, PublicationReadiness::Ready) {
            return Ok(None);
        }
        let row:Option<(String,String)>=self.db.query_row("SELECT n.sidecar,n.version FROM nas_projections n JOIN sidecars s ON s.id=n.sidecar WHERE s.owner=?1 AND n.applied=1 AND (SELECT count(*) FROM heads h WHERE h.sidecar=n.sidecar)=1 AND EXISTS(SELECT 1 FROM heads h WHERE h.sidecar=n.sidecar AND h.version=n.version) AND NOT EXISTS(SELECT 1 FROM local_heads l WHERE l.sidecar=n.sidecar AND l.version<>n.version) AND NOT EXISTS(SELECT 1 FROM publication_failures f WHERE f.source='retention:'||n.sidecar||':'||n.version AND f.next_attempt>?2) AND (EXISTS(SELECT 1 FROM versions v JOIN archived_versions a ON a.version=v.id WHERE v.sidecar=n.sidecar AND v.id<>n.version AND NOT EXISTS(SELECT 1 FROM generated_retired_versions r WHERE r.version=v.id)) OR EXISTS(SELECT 1 FROM generated_retirement_batches b WHERE b.cleaned=0 AND json_extract(b.current,'$.sidecar')=n.sidecar)) ORDER BY n.rowid LIMIT 1",params![GENERATED_OUTPUT_OWNER,SystemClock::default().now()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        row.map(|(s, v)| Ok((s.parse()?, v.parse()?))).transpose()
    }
    pub fn prepare_generated_retention(
        &mut self,
        archive: ArchiveId,
        writer: WriterId,
        sidecar: SidecarId,
        current: VersionId,
    ) -> AppResult<GeneratedRetentionPlan> {
        if !matches!(self.publication_readiness()?, PublicationReadiness::Ready) {
            return Err(AppError::integrity(
                "retention requires complete fresh journal replay",
            ));
        }
        let bound: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM archives WHERE id=?)",
            [archive.to_string()],
            |r| r.get(0),
        )?;
        let applied:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM nas_projections WHERE sidecar=? AND version=? AND applied=1)",params![sidecar.to_string(),current.to_string()],|r|r.get(0))?;
        if !bound || !applied || self.local_head(sidecar)?.is_some_and(|id| id != current) {
            return Err(AppError::integrity(
                "retention requires the current applied generated projection",
            ));
        }
        let projection: String = self.db.query_row(
            "SELECT committed FROM projections WHERE sidecar=?",
            [sidecar.to_string()],
            |r| r.get(0),
        )?;
        let projection: ProjectionPlan = decode(projection)?;
        if projection.version != current {
            return Err(AppError::integrity("retention current changed"));
        }
        validate_current(&self.db, &projection)?;
        let publication:Vec<u8>=self.db.query_row("SELECT c.envelope FROM projection_commits p JOIN commits c ON c.id=p.commit_id WHERE p.sidecar=? AND p.version=? AND c.applied=1",params![sidecar.to_string(),current.to_string()],|r|r.get(0))?;
        let publication = CommitEnvelope::decode(&publication, archive)?;
        let pending:Option<Vec<u8>>=self.db.query_row("SELECT envelope FROM generated_retirement_batches WHERE cleaned=0 AND current=? ORDER BY rowid LIMIT 1",[encode(&projection)?],|r|r.get(0)).optional()?;
        let commit = if let Some(bytes) = pending {
            CommitEnvelope::decode(&bytes, archive)?
        } else {
            let mut q=self.db.prepare("SELECT v.id FROM versions v JOIN archived_versions a ON a.version=v.id WHERE v.sidecar=?1 AND v.id<>?2 AND (NOT EXISTS(SELECT 1 FROM generated_retired_versions r WHERE r.version=v.id) OR EXISTS(SELECT 1 FROM generated_retirement_batches b,json_each(b.versions) j WHERE b.cleaned=0 AND j.value=v.id AND NOT EXISTS(SELECT 1 FROM generated_retirement_batches done,json_each(done.versions) completed WHERE done.cleaned=1 AND completed.value=v.id))) ORDER BY EXISTS(SELECT 1 FROM generated_retired_versions r WHERE r.version=v.id),v.rowid LIMIT 128")?;
            let ids = q
                .query_map(params![sidecar.to_string(), current.to_string()], |r| {
                    r.get::<_, String>(0)
                })?
                .map(|r| r?.parse())
                .collect::<AppResult<Vec<VersionId>>>()?;
            drop(q);
            validate_versions(&self.db, &projection, &ids)?;
            let commit = self
                .prepare_publication(
                    archive,
                    writer,
                    OperationId::new(),
                    vec![ArchiveRecord::GeneratedPayloadsRetired {
                        current: projection.clone(),
                        versions: ids.clone(),
                    }],
                )?
                .commit;
            self.db.execute("INSERT INTO generated_retirement_batches(commit_id,current,versions,envelope) VALUES(?,?,?,?)",params![commit.commit_id.to_string(),encode(&projection)?,encode(&ids)?,commit.encode()?])?;
            commit
        };
        let records = commit.records()?;
        let [
            ArchiveRecord::GeneratedPayloadsRetired {
                current: record_current,
                versions: ids,
            },
        ] = records.as_slice()
        else {
            return Err(AppError::integrity("invalid pending retirement record"));
        };
        if record_current != &projection {
            return Err(AppError::integrity("pending retention current changed"));
        }
        validate_versions(&self.db, &projection, ids)?;
        let versions = ids
            .iter()
            .map(|id| self.version(*id))
            .collect::<AppResult<Vec<_>>>()?;
        let mut removable = BTreeSet::new();
        for version in &versions {
            if let VersionBody::Content { digest, .. } = &version.body {
                let mut q=self.db.prepare("SELECT v.id,s.owner,EXISTS(SELECT 1 FROM heads h WHERE h.version=v.id) OR EXISTS(SELECT 1 FROM local_heads l WHERE l.version=v.id) OR EXISTS(SELECT 1 FROM pending_resolutions p WHERE p.version=v.id),EXISTS(SELECT 1 FROM generated_retired_versions r WHERE r.version=v.id) FROM versions v JOIN sidecars s ON s.id=v.sidecar WHERE json_extract(v.body,'$.Content.digest')=?")?;
                let mut rows = q.query([digest.as_str()])?;
                let mut protected = false;
                while let Some(row) = rows.next()? {
                    let id: String = row.get(0)?;
                    let owner: String = row.get(1)?;
                    let live: bool = row.get(2)?;
                    let retired: bool = row.get(3)?;
                    if live
                        || owner != GENERATED_OUTPUT_OWNER
                        || (!retired && !ids.iter().any(|v| v.to_string() == id))
                    {
                        protected = true;
                        break;
                    }
                }
                let original:bool=self.db.query_row("SELECT EXISTS(SELECT 1 FROM members WHERE digest=?1) OR EXISTS(SELECT 1 FROM placements WHERE digest=?1) OR EXISTS(SELECT 1 FROM import_items WHERE digest=?1)",[digest.as_str()],|r|r.get(0))?;
                if !protected && !original {
                    removable.insert(digest.clone());
                }
            }
        }
        let mut q=self.db.prepare("SELECT c.envelope FROM commits c WHERE c.applied=1 AND c.id IN (SELECT pc.commit_id FROM projection_commits pc WHERE pc.sidecar=?1 AND pc.version IN (SELECT value FROM json_each(?2) UNION ALL SELECT ?3)) ORDER BY c.rowid")?;
        let envelopes = q
            .query_map(
                params![sidecar.to_string(), encode(ids)?, current.to_string()],
                |r| r.get::<_, Vec<u8>>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);
        let mut projections = Vec::new();
        for bytes in envelopes {
            for record in CommitEnvelope::decode(&bytes, archive)?.records()? {
                if let ArchiveRecord::ProjectionPlanned { plan } = record
                    && plan.sidecar == sidecar
                    && (plan.version == current || ids.contains(&plan.version))
                    && !projections.contains(&plan)
                {
                    projections.push(plan);
                }
            }
        }
        let removable = removable.into_iter().collect::<Vec<_>>();
        let tx = self.db.transaction()?;
        for digest in &removable {
            tx.execute(
                "DELETE FROM history_receipts WHERE archive=? AND digest=?",
                params![archive.to_string(), digest.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(GeneratedRetentionPlan {
            commit,
            authorization: GeneratedRetentionAuthorization {
                current: projection,
                versions,
                removable,
                projections,
                archive,
                publication,
            },
        })
    }
    pub fn acknowledge_generated_retention(
        &mut self,
        receipt: &GeneratedRetentionReceipt,
    ) -> AppResult<()> {
        if !self.commit_applied(receipt.commit.commit_id)? {
            return Err(AppError::integrity(
                "retention acknowledgement requires applied commit",
            ));
        }
        let expected: Vec<u8> = self.db.query_row(
            "SELECT envelope FROM generated_retirement_batches WHERE commit_id=?",
            [receipt.commit.commit_id.to_string()],
            |r| r.get(0),
        )?;
        if expected != receipt.commit.encode()? {
            return Err(AppError::integrity(
                "retention acknowledgement envelope mismatch",
            ));
        }
        let tx = self.db.transaction()?;
        tx.execute(
            "UPDATE generated_retirement_batches SET cleaned=1 WHERE commit_id=?",
            [receipt.commit.commit_id.to_string()],
        )?;
        tx.execute("UPDATE generated_retirement_batches AS b SET cleaned=1 WHERE cleaned=0 AND NOT EXISTS(SELECT 1 FROM json_each(b.versions) old WHERE NOT EXISTS(SELECT 1 FROM generated_retirement_batches done,json_each(done.versions) completed WHERE done.cleaned=1 AND completed.value=old.value))",[])?;
        for digest in &receipt.removed_digests {
            tx.execute(
                "DELETE FROM history_receipts WHERE archive=? AND digest=?",
                params![receipt.commit.archive_id.to_string(), digest.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
