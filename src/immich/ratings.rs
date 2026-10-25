use super::*;
use crate::{
    catalog::*,
    domain::*,
    error::{AppError, AppResult, ErrorCode},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use url::Url;
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum RatingWriteMode {
    #[default]
    Disabled,
    Conditional,
    BestEffort,
}
#[derive(Debug, Clone)]
pub struct RatingWrite {
    pub asset: uuid::Uuid,
    pub expected: RatingObservation,
    pub desired: Rating,
    pub jpeg_checksum: String,
}
#[derive(Debug)]
pub enum WriteOutcome {
    Confirmed,
    Conflict { observed: RatingObservation },
    Uncertain,
}
#[derive(Default, Debug)]
pub struct RatingSyncReport {
    pub confirmed: bool,
    pub conflict: bool,
    pub waiting: bool,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct JpegBinding {
    path: ArchivePath,
    digest: Digest,
    remote_checksum: String,
}
#[derive(Debug, Clone)]
struct RatingIntent {
    id: RatingIntentId,
    group: GroupId,
    server: String,
    asset: uuid::Uuid,
    jpeg: JpegBinding,
    expected: RatingObservation,
    desired: Rating,
    state: String,
}
pub struct RatingSynchronizer<'a> {
    pub catalog: &'a mut Catalog,
    pub client: &'a dyn ImmichAccess,
}
impl Catalog {
    pub fn prepare_rating_write(
        &mut self,
        group: GroupId,
        asset: uuid::Uuid,
        expected: RatingObservation,
        desired: Rating,
        operation: OperationId,
        origin: &Url,
    ) -> AppResult<RatingIntentId> {
        let group = self.canonical_group(group)?;
        let server = origin.origin().ascii_serialization();
        let(path,digest,remote_checksum,mapped):(String,String,String,String)=self.db.query_row("SELECT path,digest,remote_checksum,group_id FROM mappings WHERE server=? AND asset=? AND stale=0",params![server,asset.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        let mapped: GroupId = serde_json::from_str(&format!("\"{mapped}\""))?;
        if self.canonical_group(mapped)? != group {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "rating asset maps to another group",
            ));
        }
        let jpeg = JpegBinding {
            path: ArchivePath::parse(&path)?,
            digest: Digest::parse(&digest)?,
            remote_checksum,
        };
        let old: Option<String> = self
            .db
            .query_row(
                "SELECT id FROM rating_intents WHERE operation=?",
                [operation.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = old {
            let id = serde_json::from_str(&format!("\"{id}\""))?;
            let old = self.rating_intent(id)?;
            if old.group != group
                || old.server != server
                || old.asset != asset
                || old.expected != expected
                || old.desired != desired
                || old.jpeg != jpeg
            {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "rating operation reused with different intent",
                ));
            }
            return Ok(id);
        }
        let id = RatingIntentId::new();
        let tx = self.db.transaction()?;
        tx.execute(
            "UPDATE rating_intents SET state='superseded' WHERE group_id=? AND state='prepared'",
            [group.to_string()],
        )?;
        tx.execute("INSERT INTO rating_intents(id,operation,group_id,server,asset,jpeg,expected,desired,state) VALUES(?,?,?,?,?,?,?,?,'prepared')",params![id.to_string(),operation.to_string(),group.to_string(),server,asset.to_string(),serde_json::to_string(&jpeg)?,serde_json::to_string(&expected)?,u8::from(desired)])?;
        tx.execute(
            "UPDATE residency SET protected=1 WHERE group_id=?",
            [group.to_string()],
        )?;
        tx.commit()?;
        Ok(id)
    }
    fn rating_intent(&self, id: RatingIntentId) -> AppResult<RatingIntent> {
        let(group,server,asset,jpeg,expected,desired,state):(String,String,String,String,String,u8,String)=self.db.query_row("SELECT group_id,server,asset,jpeg,expected,desired,state FROM rating_intents WHERE id=?",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
        Ok(RatingIntent {
            id,
            group: serde_json::from_str(&format!("\"{group}\""))?,
            server,
            asset: uuid::Uuid::parse_str(&asset)
                .map_err(|_| AppError::integrity("invalid rating asset"))?,
            jpeg: serde_json::from_str(&jpeg)?,
            expected: serde_json::from_str(&expected)?,
            desired: Rating::try_from(desired)?,
            state,
        })
    }
}
impl RatingSynchronizer<'_> {
    pub fn advance(&mut self, id: RatingIntentId) -> AppResult<RatingSyncReport> {
        self.advance_with_faults(id, &NoFaults)
    }
    pub fn advance_with_faults(
        &mut self,
        id: RatingIntentId,
        faults: &dyn FaultInjector,
    ) -> AppResult<RatingSyncReport> {
        let intent = self.catalog.rating_intent(id)?;
        if intent.state == "superseded" {
            return Ok(RatingSyncReport {
                reason: Some("superseded by a newer unsent local intent".into()),
                ..Default::default()
            });
        }
        if intent.state == "confirmed" {
            return Ok(RatingSyncReport {
                confirmed: true,
                ..Default::default()
            });
        }
        if intent.state == "conflict" {
            return Ok(RatingSyncReport {
                conflict: true,
                ..Default::default()
            });
        }
        if intent.server != self.client.origin().origin().ascii_serialization() {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "rating client origin changed",
            ));
        }
        let prior:bool=self.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM rating_intents WHERE group_id=? AND rowid<(SELECT rowid FROM rating_intents WHERE id=?) AND state IN('prepared','sent'))",params![intent.group.to_string(),id.to_string()],|r|r.get(0))?;
        if prior {
            return Ok(RatingSyncReport {
                waiting: true,
                reason: Some("earlier rating intent must settle".into()),
                ..Default::default()
            });
        }
        let binding:Option<(String,String,String)>=self.catalog.db.query_row("SELECT path,digest,remote_checksum FROM mappings WHERE server=? AND asset=? AND stale=0",params![intent.server,intent.asset.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if binding
            != Some((
                intent.jpeg.path.as_str().into(),
                intent.jpeg.digest.to_string(),
                intent.jpeg.remote_checksum.clone(),
            ))
        {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "mapped JPEG changed; rating intent retained",
            ));
        }
        if intent.state == "sent" {
            match self.client.write_progress(intent.asset) {
                WriteProgress::Pending => {
                    return Ok(RatingSyncReport {
                        waiting: true,
                        reason: Some("rating request remains in flight".into()),
                        ..Default::default()
                    });
                }
                WriteProgress::Finished(Ok(WriteOutcome::Confirmed)) => {
                    return self.finish(&intent, true, RatingObservation::Stars(intent.desired));
                }
                WriteProgress::Finished(Ok(WriteOutcome::Conflict { observed })) => {
                    return self.finish(&intent, false, observed);
                }
                WriteProgress::Finished(Err(error)) => {
                    self.catalog.db.execute(
                        "UPDATE rating_intents SET state='prepared' WHERE id=?",
                        [id.to_string()],
                    )?;
                    return Ok(RatingSyncReport {
                        waiting: true,
                        reason: Some(error.to_string()),
                        ..Default::default()
                    });
                }
                WriteProgress::Finished(Ok(WriteOutcome::Uncertain)) | WriteProgress::None => {}
            }
            let asset = self.client.asset(intent.asset)?;
            let observed = remote_rating(&asset)?;
            if asset.checksum == intent.jpeg.remote_checksum
                && observed == RatingObservation::Stars(intent.desired)
            {
                return self.finish(&intent, true, observed);
            }
            return self.finish(&intent, false, observed);
        }
        if self.catalog.protection(intent.group)?.conflict {
            return Ok(RatingSyncReport {
                waiting: true,
                reason: Some(
                    "unresolved photograph conflict blocks a prepared rating write".into(),
                ),
                ..Default::default()
            });
        }
        let authority: Option<(String, String)> = self
            .catalog
            .db
            .query_row(
                "SELECT server,asset FROM rating_authorities WHERE group_id=?",
                [intent.group.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if authority.is_some_and(|(server, asset)| {
            server != intent.server || asset != intent.asset.to_string()
        }) {
            return Ok(RatingSyncReport {
                waiting: true,
                reason: Some("rating authority changed before send".into()),
                ..Default::default()
            });
        }
        if !self.client.outbound_enabled() {
            return Ok(RatingSyncReport {
                waiting: true,
                reason: Some("outbound ratings disabled".into()),
                ..Default::default()
            });
        }
        self.catalog.db.execute(
            "UPDATE rating_intents SET state='sent' WHERE id=? AND state='prepared'",
            [id.to_string()],
        )?;
        let outcome = self.client.set_rating(RatingWrite {
            asset: intent.asset,
            expected: intent.expected.clone(),
            desired: intent.desired,
            jpeg_checksum: intent.jpeg.remote_checksum.clone(),
        });
        faults.hit(FaultPoint::ApiSent)?;
        faults.hit(FaultPoint::ApiReceived)?;
        match outcome {
            Ok(WriteOutcome::Confirmed) => {
                self.finish(&intent, true, RatingObservation::Stars(intent.desired))
            }
            Ok(WriteOutcome::Conflict { observed }) => self.finish(&intent, false, observed),
            Ok(WriteOutcome::Uncertain) => Ok(RatingSyncReport {
                waiting: true,
                reason: Some(
                    "API outcome uncertain; next attempt will read, never blindly resend".into(),
                ),
                ..Default::default()
            }),
            Err(error) => {
                self.catalog.db.execute(
                    "UPDATE rating_intents SET state='prepared' WHERE id=?",
                    [id.to_string()],
                )?;
                Ok(RatingSyncReport {
                    waiting: true,
                    reason: Some(error.to_string()),
                    ..Default::default()
                })
            }
        }
    }
    fn finish(
        &mut self,
        intent: &RatingIntent,
        confirmed: bool,
        observed: RatingObservation,
    ) -> AppResult<RatingSyncReport> {
        let tx = self.catalog.db.transaction()?;
        tx.execute(
            "UPDATE rating_intents SET state=? WHERE id=?",
            params![
                if confirmed { "confirmed" } else { "conflict" },
                intent.id.to_string()
            ],
        )?;
        if confirmed {
            tx.execute("INSERT INTO rating_observations(id,group_id,local,remote,common,pending) VALUES(?,?,?,?,?,?)",params![OperationId::new().to_string(),intent.group.to_string(),serde_json::to_string(&RatingObservation::Stars(intent.desired))?,serde_json::to_string(&observed)?,serde_json::to_string(&observed)?,serde_json::to_string(&observed)?])?;
        } else {
            tx.execute(
                "UPDATE residency SET conflict=1,protected=1 WHERE group_id=?",
                [intent.group.to_string()],
            )?;
            tx.execute("INSERT INTO conflicts(id,kind,group_id,details) VALUES(?,'rating',?,?) ON CONFLICT DO NOTHING",params![ConflictId(intent.id.0).to_string(),intent.group.to_string(),serde_json::to_string(&(intent.expected.clone(),intent.desired,observed.clone()))?])?;
        }
        let shared = vec![crate::archive::ArchiveRecord::RatingObserved {
            operation: OperationId(intent.id.0),
            group: intent.group,
            local: RatingObservation::Stars(intent.desired),
            remote: observed.clone(),
        }];
        tx.execute("INSERT INTO pending_shared_records(operation,records) VALUES(?,?) ON CONFLICT DO NOTHING",params![intent.id.to_string(),serde_json::to_string(&shared)?])?;
        tx.commit()?;
        Ok(RatingSyncReport {
            confirmed,
            conflict: !confirmed,
            ..Default::default()
        })
    }
}
pub(crate) fn remote_rating(asset: &AssetInfo) -> AppResult<RatingObservation> {
    let Some(exif) = &asset.exif_info else {
        return Ok(RatingObservation::Unobserved);
    };
    if !exif.rating_observed {
        return Ok(RatingObservation::Unobserved);
    }
    Ok(match exif.rating {
        None => RatingObservation::Stars(Rating::try_from(0)?),
        Some(n @ 1..=5) => RatingObservation::Stars(Rating::try_from(n as u8)?),
        Some(-1) => RatingObservation::Rejected,
        Some(n) => RatingObservation::Unsupported(format!("unsupported Immich rating {n}")),
    })
}
#[derive(Debug, Clone)]
pub struct RatingDerivation {
    pub sidecar: SidecarId,
    pub local: Option<VersionId>,
    pub remote: Option<VersionId>,
    pub previous: Option<VersionId>,
    pub rating: Rating,
    pub operation: OperationId,
}
type RatingBaselineRow = (
    String,
    String,
    Option<u8>,
    Option<String>,
    Option<String>,
    Option<String>,
);
#[derive(Debug, Clone)]
pub struct RatingScope {
    pub state_root: std::path::PathBuf,
    pub editor_version: String,
    pub after: Option<uuid::Uuid>,
    pub limit: usize,
}
#[derive(Default, Debug)]
pub struct RatingPollReport {
    pub observed: u64,
    pub pending: u64,
    pub conflicts: u64,
    pub next: Option<uuid::Uuid>,
}
#[derive(Debug)]
struct LocalRating {
    value: RatingObservation,
    sidecar: Option<SidecarId>,
    version: Option<VersionId>,
}
impl Catalog {
    fn rating_operation(&mut self, key: &str) -> AppResult<OperationId> {
        let old: Option<String> = self
            .db
            .query_row(
                "SELECT operation FROM rating_operations WHERE key=?",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            return Ok(serde_json::from_str(&format!("\"{old}\""))?);
        }
        let id = OperationId::new();
        self.db.execute(
            "INSERT INTO rating_operations(key,operation) VALUES(?,?)",
            params![key, id.to_string()],
        )?;
        Ok(id)
    }
    fn observe_remote_rating(
        &mut self,
        server: &str,
        asset: uuid::Uuid,
        value: &RatingObservation,
    ) -> AppResult<OperationId> {
        let encoded = serde_json::to_string(value)?;
        let previous: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT value,observation FROM rating_remote_state WHERE server=? AND asset=?",
                params![server, asset.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((previous, id)) = previous
            && previous == encoded
        {
            return Ok(serde_json::from_str(&format!("\"{id}\""))?);
        }
        let operation = OperationId::new();
        self.db.execute("INSERT INTO rating_remote_state(server,asset,value,observation) VALUES(?,?,?,?) ON CONFLICT(server,asset) DO UPDATE SET value=excluded.value,observation=excluded.observation",params![server,asset.to_string(),encoded,operation.to_string()])?;
        Ok(operation)
    }
}
impl RatingSynchronizer<'_> {
    pub fn poll(&mut self, scope: RatingScope) -> AppResult<RatingPollReport> {
        use crate::{edit::RapidRawAdapter, ratings::*};
        if scope.limit == 0 || scope.limit > 128 {
            return Err(AppError::invalid("rating poll batch must be 1..128"));
        }
        let root = crate::fs::RootHandle::open(&scope.state_root, crate::fs::RootPolicy::Local)?;
        let adapter = RapidRawAdapter::for_version(&scope.editor_version)?;
        let server = self.client.origin().origin().ascii_serialization();
        let mut q=self.catalog.db.prepare("SELECT asset,group_id FROM mappings WHERE server=? AND stale=0 AND asset>? ORDER BY asset LIMIT ?")?;
        let rows = q
            .query_map(
                params![
                    server,
                    scope.after.map(|id| id.to_string()).unwrap_or_default(),
                    scope.limit
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);
        let mut report = RatingPollReport::default();
        for (asset, group) in rows {
            let asset = uuid::Uuid::parse_str(&asset)
                .map_err(|_| AppError::integrity("invalid mapped asset"))?;
            report.next = Some(asset);
            let group = self
                .catalog
                .canonical_group(serde_json::from_str(&format!("\"{group}\""))?)?;
            let count: u64 = self.catalog.db.query_row(
                "SELECT count(*) FROM mappings WHERE server=? AND group_id=? AND stale=0",
                params![server, group.to_string()],
                |r| r.get(0),
            )?;
            if count > 1 {
                let authority: Option<String> = self
                    .catalog
                    .db
                    .query_row(
                        "SELECT asset FROM rating_authorities WHERE group_id=? AND server=?",
                        params![group.to_string(), server],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(authority) = authority {
                    if authority != asset.to_string() {
                        continue;
                    }
                } else {
                    self.catalog.record_rating_conflict(group,"rating_authority",serde_json::json!({"server":server,"required":"select an authoritative JPEG asset"}))?;
                    report.conflicts += 1;
                    continue;
                }
            }
            let asset_info = match self.client.asset(asset) {
                Ok(asset) => asset,
                Err(_) => {
                    report.pending += 1;
                    continue;
                }
            };
            let binding:(String,String,Option<String>)=self.catalog.db.query_row("SELECT external_path,remote_checksum,library FROM mappings WHERE server=? AND asset=? AND stale=0",params![server,asset.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if asset_info.original_path != binding.0
                || asset_info.checksum != binding.1
                || asset_info.library_id.map(|id| id.to_string()) != binding.2
                || asset_info.kind != "IMAGE"
                || asset_info.original_mime_type.as_deref() != Some("image/jpeg")
            {
                self.catalog.db.execute(
                    "UPDATE mappings SET stale=1 WHERE server=? AND asset=?",
                    params![server, asset.to_string()],
                )?;
                self.catalog.record_rating_conflict(group,"mapping_identity",serde_json::json!({"asset":asset,"reason":"upstream asset path, checksum, library or media identity changed"}))?;
                report.conflicts += 1;
                continue;
            }
            let remote = remote_rating(&asset_info)?;
            let observation = self
                .catalog
                .observe_remote_rating(&server, asset, &remote)?;
            let local = match local_rating(self.catalog, &root, group, &adapter) {
                Ok(local) => local,
                Err(error) => {
                    self.catalog.record_rating_conflict(
                        group,
                        "rating_metadata",
                        serde_json::json!({"reason":error.to_string()}),
                    )?;
                    report.conflicts += 1;
                    continue;
                }
            };
            let previous:Option<RatingBaselineRow>=self.catalog.db.query_row("SELECT common,applied_local,pending,pending_version,source_version,remote_observation FROM rating_baselines WHERE group_id=?",[group.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
            let (common, applied, pending, prior_version, previous_source, previous_observation) =
                if let Some((common, applied, pending, version, source, observation)) = previous {
                    (
                        serde_json::from_str(&common)?,
                        serde_json::from_str(&applied)?,
                        pending.map(Rating::try_from).transpose()?,
                        version
                            .map(|v| serde_json::from_str::<VersionId>(&format!("\"{v}\"")))
                            .transpose()?,
                        source,
                        observation,
                    )
                } else {
                    (
                        RatingObservation::Unobserved,
                        RatingObservation::Unobserved,
                        None,
                        None,
                        None,
                        None,
                    )
                };
            let input = RatingInputs {
                last_common: common.clone(),
                last_applied_local: applied.clone(),
                local: local.value.clone(),
                remote: remote.clone(),
                pending_canonical: pending,
                local_observation: local.version.map(|v| OperationId(v.0)),
                remote_observation: Some(observation),
            };
            let decision = reconcile_rating(&input);
            let operation = self.catalog.rating_operation(&format!(
                "rating:{group}:{asset}:{:?}:{observation}",
                local.version
            ))?;
            let records = vec![crate::archive::ArchiveRecord::RatingObserved {
                operation,
                group,
                local: local.value.clone(),
                remote: remote.clone(),
            }];
            self.catalog.db.execute("INSERT INTO pending_shared_records(operation,records) VALUES(?,?) ON CONFLICT DO NOTHING",params![operation.to_string(),serde_json::to_string(&records)?])?;
            let (mut new_common, mut new_applied, mut new_pending, mut pending_version) =
                (common, applied, pending, prior_version);
            match decision {
                RatingDecision::Unsupported | RatingDecision::Conflict { .. } => {
                    self.catalog.db.execute(
                        "UPDATE residency SET conflict=1,protected=1 WHERE group_id=?",
                        [group.to_string()],
                    )?;
                    self.catalog.record_rating_conflict(
                        group,
                        "rating",
                        serde_json::json!({"local":local.value,"remote":remote,"pending":pending}),
                    )?;
                    report.conflicts += 1;
                }
                RatingDecision::PublishLocal(rating) => {
                    self.catalog.prepare_rating_write(
                        group,
                        asset,
                        remote.clone(),
                        rating,
                        operation,
                        self.client.origin(),
                    )?;
                    new_pending = Some(rating);
                    report.pending += 1;
                }
                RatingDecision::Converge(rating) => {
                    new_common = RatingObservation::Stars(rating);
                    new_applied = local.value.clone();
                    new_pending = None;
                }
                RatingDecision::AdoptRemote(rating) | RatingDecision::KeepPending(rating) => {
                    if matches!(remote,RatingObservation::Stars(r) if r==rating) {
                        new_common = remote.clone();
                    }
                    if let Some(sidecar) = local.sidecar {
                        let heads = self.catalog.heads(sidecar)?;
                        if heads.len() > 1 {
                            report.conflicts += 1;
                            continue;
                        }
                        let mut canonical = heads.first().copied();
                        if let Some(prior) = prior_version
                            && (canonical.is_none()
                                || canonical.is_some_and(|c| {
                                    self.catalog.is_ancestor(c, prior).unwrap_or(false)
                                }))
                        {
                            canonical = Some(prior);
                        }
                        let unchanged = prior_version.is_some()
                            && pending == Some(rating)
                            && previous_source == local.version.map(|v| v.to_string())
                            && previous_observation == Some(observation.to_string())
                            && canonical == prior_version;
                        if !unchanged {
                            let derivation = self.catalog.rating_operation(&format!(
                                "derive:{group}:{:?}:{canonical:?}:{observation}:{}",
                                local.version,
                                u8::from(rating)
                            ))?;
                            let version = derive_rating_version(
                                self.catalog,
                                &root,
                                RatingDerivation {
                                    sidecar,
                                    local: local.version,
                                    remote: canonical,
                                    previous: prior_version,
                                    rating,
                                    operation: derivation,
                                },
                                &adapter,
                            )?;
                            pending_version = Some(version);
                            new_pending = Some(rating);
                        }
                        report.pending += 1;
                    } else {
                        let group_data = self.catalog.group(group)?;
                        let primary = primary_raw(&group_data);
                        if let Some(primary) = primary {
                            let path = ArchivePath::parse(&format!("{}.rrdata", primary.as_str()))?;
                            let sidecar = self.catalog.managed_sidecar_id(&path)?;
                            self.catalog.register_sidecar(
                                sidecar,
                                Some(group),
                                path,
                                "rapidraw-base",
                            )?;
                            let bytes = adapter.initial_document(rating)?;
                            let version = crate::edit::VersionStore::new(self.catalog, &root)
                                .record(crate::edit::SnapshotIntent {
                                    sidecar,
                                    parents: vec![],
                                    operation,
                                    payload: crate::edit::SnapshotPayload::Content(bytes),
                                    origin: VersionOrigin::Derived,
                                })?;
                            pending_version = Some(version);
                            new_pending = Some(rating);
                            report.pending += 1;
                        } else {
                            new_pending = None;
                            new_common = RatingObservation::Stars(rating);
                        }
                    }
                }
                RatingDecision::Unchanged => {}
            }
            self.catalog.db.execute("INSERT INTO rating_baselines(group_id,common,applied_local,pending,pending_version,source_version,remote_observation) VALUES(?,?,?,?,?,?,?) ON CONFLICT(group_id) DO UPDATE SET common=excluded.common,applied_local=excluded.applied_local,pending=excluded.pending,pending_version=excluded.pending_version,source_version=excluded.source_version,remote_observation=excluded.remote_observation",params![group.to_string(),serde_json::to_string(&new_common)?,serde_json::to_string(&new_applied)?,new_pending.map(u8::from),pending_version.map(|v|v.to_string()),local.version.map(|v|v.to_string()),observation.to_string()])?;
            report.observed += 1;
        }
        Ok(report)
    }
}
fn primary_raw(group: &Group) -> Option<ArchivePath> {
    if let Some(primary) = &group.primary_raw {
        return Some(primary.clone());
    }
    let raw = group
        .members
        .iter()
        .filter(|m| m.role == MediaRole::Raw)
        .collect::<Vec<_>>();
    let orf = raw
        .iter()
        .filter(|m| m.path.as_str().to_ascii_lowercase().ends_with(".orf"))
        .collect::<Vec<_>>();
    if orf.len() == 1
        && raw.iter().all(|m| {
            m.path.as_str().to_ascii_lowercase().ends_with(".orf")
                || m.path.as_str().to_ascii_lowercase().ends_with(".ori")
        })
    {
        Some(orf[0].path.clone())
    } else if raw.len() == 1 {
        Some(raw[0].path.clone())
    } else {
        None
    }
}
fn local_rating(
    catalog: &Catalog,
    root: &crate::fs::RootHandle,
    group: GroupId,
    adapter: &crate::edit::RapidRawAdapter,
) -> AppResult<LocalRating> {
    let group = catalog.group(group)?;
    let Some(primary) = primary_raw(&group) else {
        return Ok(LocalRating {
            value: RatingObservation::Unobserved,
            sidecar: None,
            version: None,
        });
    };
    let sidecar: Option<String> = catalog
        .db
        .query_row(
            "SELECT id FROM sidecars WHERE group_id=? AND path=? AND owner='rapidraw-base'",
            params![group.id.to_string(), format!("{}.rrdata", primary.as_str())],
            |r| r.get(0),
        )
        .optional()?;
    let Some(sidecar) = sidecar else {
        return Ok(LocalRating {
            value: RatingObservation::Unobserved,
            sidecar: None,
            version: None,
        });
    };
    let sidecar: SidecarId = serde_json::from_str(&format!("\"{sidecar}\""))?;
    let version = catalog.local_head(sidecar)?;
    let value = match version {
        None => RatingObservation::Unobserved,
        Some(version) => {
            let version = catalog.version(version)?;
            match version.body {
                VersionBody::Reset => RatingObservation::Unobserved,
                VersionBody::Content { .. } => {
                    load_rating_document(root, &version, adapter)?.rating
                }
            }
        }
    };
    Ok(LocalRating {
        value,
        sidecar: Some(sidecar),
        version,
    })
}
fn load_rating_document(
    root: &crate::fs::RootHandle,
    version: &Version,
    adapter: &crate::edit::RapidRawAdapter,
) -> AppResult<crate::edit::EditDocument> {
    let VersionBody::Content { digest, size } = &version.body else {
        return adapter.inspect(&adapter.initial_document(Rating::try_from(0)?)?);
    };
    if *size > 64 * 1024 * 1024 {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "opaque metadata cannot be transformed",
        ));
    }
    let bytes = root.read(
        &ArchivePath::parse(&format!(
            ".photo-sorter/snapshots/{}",
            &digest.as_str()[7..]
        ))?,
        *size,
    )?;
    if bytes.len() as u64 != *size || Digest::of(&bytes) != *digest {
        return Err(AppError::integrity("rating history is corrupt"));
    }
    adapter.inspect(&bytes)
}
pub fn derive_rating_version(
    catalog: &mut Catalog,
    root: &crate::fs::RootHandle,
    request: RatingDerivation,
    adapter: &crate::edit::RapidRawAdapter,
) -> AppResult<VersionId> {
    let RatingDerivation {
        sidecar,
        local,
        remote,
        previous,
        rating,
        operation,
    } = request;
    let local_version = local.map(|v| catalog.version(v)).transpose()?;
    let remote_version = remote.map(|v| catalog.version(v)).transpose()?;
    let bytes = match (&local_version, &remote_version) {
        (None, None) => adapter.initial_document(rating)?,
        (Some(v), None) | (None, Some(v)) => {
            adapter.with_rating(&load_rating_document(root, v, adapter)?, rating)?
        }
        (Some(left), Some(right)) => {
            if left.sidecar != sidecar || right.sidecar != sidecar {
                return Err(AppError::integrity("cross-sidecar rating merge"));
            }
            let local_doc = load_rating_document(root, left, adapter)?;
            let remote_doc = load_rating_document(root, right, adapter)?;
            let remote_doc = adapter.inspect(&adapter.with_rating(&remote_doc, rating)?)?;
            if local_doc.non_rating() == remote_doc.non_rating() {
                adapter.with_rating(&local_doc, rating)?
            } else {
                let common = catalog.common_version(left.id, right.id)?.ok_or_else(|| {
                    AppError::new(
                        ErrorCode::Conflict,
                        "independent adjustment histories cannot merge",
                    )
                })?;
                let base = load_rating_document(root, &catalog.version(common)?, adapter)?;
                adapter.merge_rating_adjustments(&base, &local_doc, &remote_doc, rating)?
            }
        }
    };
    let mut parents = local
        .into_iter()
        .chain(remote)
        .chain(previous)
        .collect::<Vec<_>>();
    parents.sort();
    parents.dedup();
    crate::edit::VersionStore::new(catalog, root).record(crate::edit::SnapshotIntent {
        sidecar,
        parents,
        operation,
        payload: crate::edit::SnapshotPayload::Content(bytes),
        origin: VersionOrigin::Derived,
    })
}

impl Catalog {
    pub fn record_rating_conflict(
        &mut self,
        group: GroupId,
        kind: &str,
        details: serde_json::Value,
    ) -> AppResult<ConflictId> {
        let group = self.canonical_group(group)?;
        let previous: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT id,details FROM conflicts WHERE group_id=? AND kind=? AND resolved=0",
                params![group.to_string(), kind],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let encoded = serde_json::to_string(&details)?;
        let id = if let Some((id, old)) = previous {
            if old != encoded {
                self.db.execute(
                    "UPDATE conflicts SET details=?,revision=revision+1 WHERE id=?",
                    params![encoded, id],
                )?;
            }
            serde_json::from_str(&format!("\"{id}\""))?
        } else {
            let id = ConflictId::new();
            self.db.execute(
                "INSERT INTO conflicts(id,kind,group_id,details) VALUES(?,?,?,?)",
                params![id.to_string(), kind, group.to_string(), encoded],
            )?;
            id
        };
        self.db.execute(
            "UPDATE residency SET conflict=1,protected=1 WHERE group_id=?",
            [group.to_string()],
        )?;
        Ok(id)
    }
    pub fn select_rating_authority(
        &mut self,
        group: GroupId,
        asset: uuid::Uuid,
        server: &Url,
        revision: MembershipRevision,
    ) -> AppResult<()> {
        let group = self.group(group)?;
        if group.membership_revision != revision {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "membership changed before rating authority selection",
            ));
        }
        let mapped: String = self.db.query_row(
            "SELECT group_id FROM mappings WHERE server=? AND asset=? AND stale=0",
            params![server.origin().ascii_serialization(), asset.to_string()],
            |r| r.get(0),
        )?;
        if self.canonical_group(serde_json::from_str(&format!("\"{mapped}\""))?)? != group.id {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "selected asset belongs to another photograph",
            ));
        }
        self.db.execute("INSERT INTO rating_authorities(group_id,server,asset) VALUES(?,?,?) ON CONFLICT(group_id) DO UPDATE SET server=excluded.server,asset=excluded.asset",params![group.id.to_string(),server.origin().ascii_serialization(),asset.to_string()])?;
        Ok(())
    }
}
