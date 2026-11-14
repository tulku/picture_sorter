use super::*;
use crate::{
    domain::*,
    edit::{SidecarKind, SidecarOwner},
};
use rusqlite::{OptionalExtension, params};

fn equivalent_path_claim(
    db: &Connection,
    query: &str,
    values: impl rusqlite::Params,
    path: &ArchivePath,
) -> AppResult<bool> {
    let folded = path.as_str().to_lowercase().to_uppercase();
    let mut statement = db.prepare(query)?;
    let mut rows = statement.query(values)?;
    while let Some(row) = rows.next()? {
        let candidate: String = row.get(0)?;
        if candidate.to_lowercase().to_uppercase() == folded {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(crate) fn reject_original_claim(db: &Connection, path: &ArchivePath) -> AppResult<()> {
    let claimed = equivalent_path_claim(db,
        "SELECT path FROM members WHERE lower(path)=lower(?1) OR path GLOB '*[^ -~]*'
         UNION ALL SELECT path FROM placements WHERE lower(path)=lower(?1) OR path GLOB '*[^ -~]*'
         UNION ALL SELECT destination FROM import_items WHERE lower(destination)=lower(?1) OR destination GLOB '*[^ -~]*'",
        params![path.as_str().to_lowercase().to_uppercase()], path)?;
    if claimed {
        return Err(AppError::integrity(
            "generated output collides with camera original ownership",
        ));
    }
    Ok(())
}
pub(crate) fn reject_generated_claim(db: &Connection, path: &ArchivePath) -> AppResult<()> {
    let claimed = equivalent_path_claim(
        db,
        "SELECT path FROM sidecars WHERE owner=?2 AND (lower(path)=lower(?1) OR path GLOB '*[^ -~]*')",
        params![
            path.as_str().to_lowercase().to_uppercase(),
            GENERATED_OUTPUT_OWNER
        ],
        path,
    )?;
    if claimed {
        return Err(AppError::integrity(
            "camera original collides with generated output ownership",
        ));
    }
    Ok(())
}
pub(crate) fn validate_generated_identity(
    db: &Connection,
    id: SidecarId,
    group: Option<GroupId>,
    path: &ArchivePath,
) -> AppResult<()> {
    reject_original_claim(db, path)?;
    let group = group
        .ok_or_else(|| AppError::integrity("generated output requires an independent group"))?;
    let path_occupied = equivalent_path_claim(
        db,
        "SELECT path FROM sidecars WHERE id<>?2 AND (lower(path)=lower(?1) OR path GLOB '*[^ -~]*')",
        params![path.as_str().to_lowercase().to_uppercase(), id.to_string()],
        path,
    )?;
    let occupied: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM members WHERE group_id=?1) OR EXISTS(SELECT 1 FROM sidecars WHERE group_id=?1 AND owner=?2 AND id<>?3)", params![group.to_string(),GENERATED_OUTPUT_OWNER,id.to_string()],|r|r.get(0))?;
    if path_occupied || occupied {
        return Err(AppError::integrity(
            "generated output identity collides with existing ownership",
        ));
    }
    Ok(())
}
pub(crate) fn reject_generated_group(db: &Connection, group: GroupId) -> AppResult<()> {
    let generated: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sidecars WHERE group_id=? AND owner=?)",
        params![group.to_string(), GENERATED_OUTPUT_OWNER],
        |r| r.get(0),
    )?;
    if generated {
        return Err(AppError::integrity(
            "generated output groups cannot contain camera originals or merge",
        ));
    }
    Ok(())
}
impl Catalog {
    pub fn generated_owner(&self, path: &ArchivePath) -> AppResult<Option<SidecarOwner>> {
        let row: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT id,group_id FROM sidecars WHERE path=? AND owner=?",
                params![path.as_str(), GENERATED_OUTPUT_OWNER],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(id, group)| {
            Ok(SidecarOwner {
                id: id.parse()?,
                group: Some(self.canonical_group(group.parse()?)?),
                path: path.clone(),
                kind: SidecarKind::Generated,
                virtual_copy: None,
            })
        })
        .transpose()
    }
    pub fn register_generated_output(&mut self, path: &ArchivePath) -> AppResult<SidecarOwner> {
        validate_saved_path(path, GENERATED_OUTPUT_OWNER)?;
        if let Some(owner) = self.generated_owner(path)? {
            validate_generated_identity(&self.db, owner.id, owner.group, path)?;
            return Ok(owner);
        }
        let id = SidecarId::new();
        let group = GroupId::new();
        let tx = self.db.transaction()?;
        validate_generated_identity(&tx, id, Some(group), path)?;
        tx.execute(
            "INSERT INTO groups(id,published) VALUES(?,0)",
            [group.to_string()],
        )?;
        tx.execute(
            "INSERT INTO residency(group_id) VALUES(?)",
            [group.to_string()],
        )?;
        tx.execute(
            "INSERT INTO sidecars(id,group_id,path,owner) VALUES(?,?,?,?)",
            params![
                id.to_string(),
                group.to_string(),
                path.as_str(),
                GENERATED_OUTPUT_OWNER
            ],
        )?;
        tx.commit()?;
        Ok(SidecarOwner {
            id,
            group: Some(group),
            path: path.clone(),
            kind: SidecarKind::Generated,
            virtual_copy: None,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GeneratedProjectionAuthorization {
    archive: ArchiveId,
    sidecar: SidecarId,
    path: ArchivePath,
    version: VersionId,
}
impl GeneratedProjectionAuthorization {
    pub fn matches(&self, archive: ArchiveId, plan: &crate::archive::ProjectionPlan) -> bool {
        self.archive == archive
            && self.sidecar == plan.sidecar
            && self.path == plan.path
            && self.version == plan.version
            && plan.owner == GENERATED_OUTPUT_OWNER
    }
}
impl Catalog {
    pub fn authorize_generated_projection(
        &self,
        plan: &crate::archive::ProjectionPlan,
    ) -> AppResult<GeneratedProjectionAuthorization> {
        if !matches!(self.publication_readiness()?, PublicationReadiness::Ready) {
            return Err(AppError::integrity(
                "generated ownership requires complete journal replay",
            ));
        }
        validate_saved_path(&plan.path, &plan.owner)?;
        let owner = self
            .generated_owner(&plan.path)?
            .ok_or_else(|| AppError::integrity("missing generated ownership"))?;
        if owner.id != plan.sidecar || plan.owner != GENERATED_OUTPUT_OWNER {
            return Err(AppError::integrity("generated projection owner mismatch"));
        }
        validate_generated_identity(&self.db, owner.id, owner.group, &plan.path)?;
        let version = self.version(plan.version)?;
        if version.sidecar != plan.sidecar
            || version.body != plan.body
            || (self.heads(plan.sidecar)? != vec![plan.version]
                && self.local_head(plan.sidecar)? != Some(plan.version))
        {
            return Err(AppError::integrity(
                "generated projection must match the committed saved head",
            ));
        }
        let archive: String = self
            .db
            .query_row("SELECT id FROM archives LIMIT 1", [], |r| r.get(0))?;
        Ok(GeneratedProjectionAuthorization {
            archive: archive.parse()?,
            sidecar: plan.sidecar,
            path: plan.path.clone(),
            version: plan.version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_camera_claims_block_generated_outputs_without_nas_files() {
        for (camera, generated) in [
            ("Été.TIF", "été.tif"),
            ("straße.TIF", "STRASSE.tif"),
            ("STRASSE.TIF", "straße.tif"),
            ("I.TIF", "ı.tif"),
            ("ẞ.TIF", "ss.tif"),
            ("K.TIF", "k.tif"),
        ] {
            for claim in ["member", "placement", "import"] {
                let dir = tempfile::tempdir().unwrap();
                let root =
                    crate::fs::RootHandle::open(dir.path(), crate::fs::RootPolicy::Local).unwrap();
                let mut catalog = Catalog::open(&root).unwrap();
                let group = catalog.create_group(false).unwrap();
                let digest = Digest::of(b"camera");
                match claim {
                    "member" => {
                        catalog
                            .attach_member(
                                group,
                                OriginalMember {
                                    path: camera.parse().unwrap(),
                                    digest: digest.clone(),
                                    size: 6,
                                    role: MediaRole::OtherStill,
                                },
                                PairEvidence::UserSelected {
                                    reason: "test".into(),
                                },
                            )
                            .unwrap();
                    }
                    "placement" => {
                        catalog
                            .db
                            .execute(
                                "INSERT INTO placements(id,group_id,path,digest) VALUES(?,?,?,?)",
                                params![
                                    OperationId::new().to_string(),
                                    group.to_string(),
                                    camera,
                                    digest.as_str()
                                ],
                            )
                            .unwrap();
                    }
                    _ => {
                        let job = catalog
                            .enqueue(DurableRequest {
                                operation_id: OperationId::new(),
                                kind: JobKind::Clean,
                            })
                            .unwrap();
                        catalog.db.execute("INSERT INTO import_items(id,parent,source_root,source_path,digest,size,role,destination,state) VALUES(?,?,'/fixture','source',?,6,?,?,'pending')",params![OperationId::new().to_string(),job.to_string(),digest.as_str(),encode(&MediaRole::OtherStill).unwrap(),camera]).unwrap();
                    }
                }
                assert!(!dir.path().join(camera).exists());
                assert!(
                    catalog
                        .register_generated_output(&generated.parse().unwrap())
                        .is_err(),
                    "{claim}: {camera} vs {generated}"
                );
            }
        }
    }
    #[test]
    fn unicode_generated_claims_block_camera_members_and_duplicate_saved_identities() {
        for (generated, alias) in [
            ("été.tif", "ÉTÉ.TIF"),
            ("straße.tif", "STRASSE.TIF"),
            ("STRASSE.tif", "straße.TIF"),
            ("ı.tif", "I.TIF"),
            ("ss.tif", "ẞ.TIF"),
            ("k.tif", "K.TIF"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root =
                crate::fs::RootHandle::open(dir.path(), crate::fs::RootPolicy::Local).unwrap();
            let mut catalog = Catalog::open(&root).unwrap();
            catalog
                .register_generated_output(&generated.parse().unwrap())
                .unwrap();
            assert!(
                catalog
                    .register_generated_output(&alias.parse().unwrap())
                    .is_err(),
                "{generated} vs {alias}"
            );
            let group = catalog.create_group(false).unwrap();
            assert!(
                catalog
                    .attach_member(
                        group,
                        OriginalMember {
                            path: alias.parse().unwrap(),
                            digest: Digest::of(b"camera"),
                            size: 6,
                            role: MediaRole::OtherStill
                        },
                        PairEvidence::UserSelected {
                            reason: "test".into()
                        }
                    )
                    .is_err(),
                "{generated} vs {alias}"
            );
        }
    }
    #[test]
    fn generated_authorization_binds_archive_path_version_and_complete_replay() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::fs::RootHandle::open(dir.path(), crate::fs::RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let archive = ArchiveId::new();
        catalog
            .db
            .execute(
                "INSERT INTO archives(id,writer) VALUES(?,?)",
                params![archive.to_string(), WriterId::new().to_string()],
            )
            .unwrap();
        let owner = catalog
            .register_generated_output(&ArchivePath::parse("render.psd").unwrap())
            .unwrap();
        let version = crate::edit::VersionStore::new(&mut catalog, &root)
            .record(crate::edit::SnapshotIntent {
                sidecar: owner.id,
                parents: vec![],
                operation: OperationId::new(),
                payload: crate::edit::SnapshotPayload::Content(b"project".to_vec()),
                origin: VersionOrigin::Observed,
            })
            .unwrap();
        assert!(
            !catalog
                .readiness(owner.group.unwrap())
                .unwrap()
                .originals_ready
        );
        let mut plan = crate::archive::ProjectionPlan {
            sidecar: owner.id,
            path: owner.path.clone(),
            version,
            expected: VersionBody::Reset,
            body: catalog.version(version).unwrap().body,
            owner: GENERATED_OUTPUT_OWNER.into(),
        };
        let auth = catalog.authorize_generated_projection(&plan).unwrap();
        assert!(auth.matches(archive, &plan));
        assert!(!auth.matches(ArchiveId::new(), &plan));
        plan.path = ArchivePath::parse("different.psd").unwrap();
        assert!(!auth.matches(archive, &plan));
        assert!(catalog.authorize_generated_projection(&plan).is_err());
        plan.path = owner.path;
        plan.body = VersionBody::Reset;
        assert!(catalog.authorize_generated_projection(&plan).is_err());
        plan.body = catalog.version(version).unwrap().body;
        catalog
            .block_record("missing-journal", "incomplete replay")
            .unwrap();
        assert!(catalog.authorize_generated_projection(&plan).is_err());
    }
    #[test]
    fn generated_groups_reject_camera_members_merges_and_pending_import_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::fs::RootHandle::open(dir.path(), crate::fs::RootPolicy::Local).unwrap();
        let mut catalog = Catalog::open(&root).unwrap();
        let saved = catalog
            .register_generated_output(&ArchivePath::parse("render.psd").unwrap())
            .unwrap();
        let camera = catalog.create_group(false).unwrap();
        assert!(
            catalog
                .attach_member(
                    saved.group.unwrap(),
                    OriginalMember {
                        path: ArchivePath::parse("camera.ORF").unwrap(),
                        digest: Digest::of(b"raw"),
                        size: 3,
                        role: MediaRole::Raw
                    },
                    PairEvidence::UserSelected {
                        reason: "test".into()
                    }
                )
                .is_err()
        );
        assert!(
            catalog
                .merge_groups(
                    &[saved.group.unwrap(), camera],
                    PairEvidence::UserSelected {
                        reason: "test".into()
                    }
                )
                .is_err()
        );
        let job = catalog
            .enqueue(DurableRequest {
                operation_id: OperationId::new(),
                kind: JobKind::Clean,
            })
            .unwrap();
        catalog.db.execute("INSERT INTO import_items(id,parent,source_root,source_path,digest,size,role,destination,state) VALUES(?,?,'/fixture','source',?,3,?,'HDR/IMPORT.TIF','pending')", params![OperationId::new().to_string(),job.to_string(),Digest::of(b"raw").as_str(),encode(&MediaRole::OtherStill).unwrap()]).unwrap();
        assert!(
            catalog
                .register_generated_output(&ArchivePath::parse("hdr/import.tif").unwrap())
                .is_err()
        );
        let ready = catalog.readiness(saved.group.unwrap()).unwrap();
        assert!(!ready.originals_ready);
        assert!(ready.waiting_reason.unwrap().contains("generated"));
    }
}
