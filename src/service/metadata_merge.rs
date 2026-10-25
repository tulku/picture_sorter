use crate::{
    catalog::*,
    domain::*,
    edit::*,
    error::{AppError, AppResult, ErrorCode},
    fs::RootHandle,
};
use rusqlite::{OptionalExtension, params};
fn document(
    catalog: &Catalog,
    root: &RootHandle,
    version: VersionId,
    adapter: &RapidRawAdapter,
) -> AppResult<EditDocument> {
    let VersionBody::Content { digest, size } = catalog.version(version)?.body else {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "Reset cannot participate in automatic field merge",
        ));
    };
    if size > 64 * 1024 * 1024 {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "oversized metadata remains opaque",
        ));
    }
    let bytes = root.read(&snapshot_path(&digest)?, 64 * 1024 * 1024)?;
    if bytes.len() as u64 != size || Digest::of(&bytes) != digest {
        return Err(AppError::integrity(
            "merge metadata snapshot identity mismatch",
        ));
    }
    adapter.inspect(&bytes)
}
pub(crate) fn merge_sidecar(
    catalog: &mut Catalog,
    root: &RootHandle,
    sidecar: SidecarId,
    editor_version: &str,
) -> AppResult<Option<VersionId>> {
    let heads = catalog.heads(sidecar)?;
    if heads.len() != 2 {
        return Ok(None);
    }
    let encoded = serde_json::to_string(&heads)?;
    let signature = format!("{editor_version}:{encoded}");
    let attempted: bool = catalog.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM metadata_merge_attempts WHERE sidecar=? AND signature=?)",
        params![sidecar.to_string(), signature],
        |r| r.get(0),
    )?;
    if attempted {
        return Ok(None);
    }
    let adapter = RapidRawAdapter::for_version(editor_version)?;
    let Some(base) = catalog.common_version(heads[0], heads[1])? else {
        return Ok(None);
    };
    let base = document(catalog, root, base, &adapter)?;
    let left = document(catalog, root, heads[0], &adapter)?;
    let right = document(catalog, root, heads[1], &adapter)?;
    let merge = || -> AppResult<Vec<u8>> {
        let (
            RatingObservation::Stars(base_rating),
            RatingObservation::Stars(left_rating),
            RatingObservation::Stars(right_rating),
        ) = (&base.rating, &left.rating, &right.rating)
        else {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "unsupported/rejected rating remains unresolved",
            ));
        };
        let rating = if left_rating == right_rating || right_rating == base_rating {
            *left_rating
        } else if left_rating == base_rating {
            *right_rating
        } else {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "different concurrent rating changes remain unresolved",
            ));
        };
        adapter.merge_rating_adjustments(&base, &left, &right, rating)
    };
    let bytes = match merge() {
        Ok(bytes) => bytes,
        Err(error) => {
            catalog.db.execute("INSERT INTO metadata_merge_attempts(sidecar,signature,reason) VALUES(?,?,?) ON CONFLICT DO NOTHING",params![sidecar.to_string(),signature,error.to_string()])?;
            return Ok(None);
        }
    };
    let digest = blake3::hash(
        format!(
            "native-field-merge-v1:{sidecar}:{signature}:{}",
            Digest::of(&bytes)
        )
        .as_bytes(),
    );
    let mut id = [0; 16];
    id.copy_from_slice(&digest.as_bytes()[..16]);
    let operation = OperationId(uuid::Uuid::from_bytes(id));
    let version = VersionStore::new(catalog, root).record(SnapshotIntent {
        sidecar,
        parents: heads,
        operation,
        payload: SnapshotPayload::Content(bytes),
        origin: VersionOrigin::Derived,
    })?;
    catalog.db.execute(
        "INSERT INTO derived_heads(version,sidecar,expected) VALUES(?,?,?) ON CONFLICT DO NOTHING",
        params![version.to_string(), sidecar.to_string(), encoded],
    )?;
    catalog.db.execute("INSERT INTO metadata_merge_attempts(sidecar,signature,reason) VALUES(?,?,'derived version awaits publication') ON CONFLICT DO NOTHING",params![sidecar.to_string(),signature])?;
    Ok(Some(version))
}
pub(crate) fn advance(
    catalog: &mut Catalog,
    root: &RootHandle,
    version: &str,
    after: &mut Option<SidecarId>,
) -> AppResult<()> {
    let next:Option<(String,Option<String>)>=catalog.db.query_row("SELECT s.id,s.group_id FROM sidecars s WHERE s.owner='rapidraw-base' AND s.id>? AND (SELECT count(*) FROM heads h WHERE h.sidecar=s.id)=2 AND NOT EXISTS(SELECT 1 FROM sidecar_diagnostics d WHERE d.sidecar=s.id) ORDER BY s.id LIMIT 1",[after.map(|id|id.to_string()).unwrap_or_default()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((sidecar, group)) = next else {
        *after = None;
        return Ok(());
    };
    let sidecar: SidecarId = serde_json::from_value(sidecar.into())?;
    *after = Some(sidecar);
    match merge_sidecar(catalog, root, sidecar, version) {
        Ok(_) => Ok(()),
        Err(error) if error.errno == Some(libc::ENOENT) => {
            if let Some(group) = group {
                let group: GroupId = serde_json::from_value(group.into())?;
                let mut versions = catalog.heads(sidecar)?;
                if let Some(common) = catalog.common_version(versions[0], versions[1])? {
                    versions.push(common);
                }
                versions.sort();
                versions.dedup();
                let active:bool=catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE json_extract(request,'$.kind.type')='metadata_fetch' AND json_extract(request,'$.kind.group')=? AND json_extract(state,'$') NOT IN('Complete','Cancelled','Failed'))",[group.to_string()],|r|r.get(0))?;
                if !active {
                    catalog.enqueue(DurableRequest {
                        operation_id: OperationId::new(),
                        kind: JobKind::MetadataFetch { group, versions },
                    })?;
                }
            }
            Ok(())
        }
        Err(error) if matches!(error.code, ErrorCode::Conflict | ErrorCode::Unsupported) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use crate::{catalog::*, domain::*, edit::*, fs::*};
    #[test]
    fn coordinator_merges_rating_and_adjustments_with_both_parents_without_applying_live_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let group = catalog.create_group(false).unwrap();
        let sidecar = SidecarId::new();
        catalog
            .register_sidecar(
                sidecar,
                Some(group),
                ArchivePath::parse("RAW/a.orf.rrdata").unwrap(),
                "rapidraw-base",
            )
            .unwrap();
        let mut save = |parents, bytes: &[u8], detached| {
            let intent = SnapshotIntent {
                sidecar,
                parents,
                operation: OperationId::new(),
                payload: SnapshotPayload::Content(bytes.to_vec()),
                origin: VersionOrigin::Observed,
            };
            let mut store = VersionStore::new(&mut catalog, &root);
            if detached {
                store.record_detached(intent).unwrap()
            } else {
                store.record(intent).unwrap()
            }
        };
        let base = save(
            vec![],
            br#"{"version":1,"rating":3,"adjustments":{"exposure":1},"mask":"preserve"}"#,
            false,
        );
        let left = save(
            vec![base],
            br#"{"version":1,"rating":3,"adjustments":{"exposure":2},"mask":"preserve"}"#,
            false,
        );
        let right = save(
            vec![base],
            br#"{"version":1,"rating":5,"adjustments":{"exposure":1},"mask":"preserve"}"#,
            true,
        );
        for id in [base, left, right] {
            catalog
                .db
                .execute(
                    "INSERT INTO archived_versions(version) VALUES(?)",
                    [id.to_string()],
                )
                .unwrap();
        }
        for id in [left, right] {
            catalog
                .db
                .execute(
                    "INSERT INTO heads(sidecar,version) VALUES(?,?)",
                    rusqlite::params![sidecar.to_string(), id.to_string()],
                )
                .unwrap();
        }
        let version = super::merge_sidecar(&mut catalog, &root, sidecar, "1.6.6")
            .unwrap()
            .unwrap();
        let merged = catalog.version(version).unwrap();
        let mut parents = vec![left, right];
        parents.sort();
        assert_eq!(merged.parents, parents);
        assert_eq!(catalog.local_head(sidecar).unwrap(), Some(left));
        let VersionBody::Content { digest, .. } = merged.body else {
            panic!()
        };
        let bytes = root.read(&snapshot_path(&digest).unwrap(), 1024).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["rating"], 5);
        assert_eq!(json["adjustments"]["exposure"], 2);
        assert_eq!(json["mask"], "preserve");
        assert!(!temp.path().join("RAW/a.orf.rrdata").exists());
        let pin = catalog.request_pin(group, OperationId::new()).unwrap();
        let bodies = catalog
            .pin_metadata_bodies(
                pin,
                group,
                catalog.group(group).unwrap().membership_revision,
            )
            .unwrap();
        assert_eq!(
            bodies.len(),
            2,
            "ordinary pins fetch canonical heads rather than complete editing history"
        );
    }
}
