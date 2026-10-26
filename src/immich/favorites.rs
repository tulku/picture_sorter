use super::*;
use crate::{
    catalog::*,
    domain::*,
    edit::*,
    error::{AppError, AppResult, ErrorCode},
    fs::*,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub struct FavoriteSynchronizer<'a> {
    pub catalog: &'a mut Catalog,
    pub client: &'a dyn ImmichAccess,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Baseline {
    server: String,
    asset: uuid::Uuid,
    common: Option<bool>,
    applied: Option<bool>,
    pending: Option<bool>,
    #[serde(default)]
    outbound: Option<OperationId>,
    source_version: Option<VersionId>,
    pending_version: Option<VersionId>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Intent {
    group: GroupId,
    asset: uuid::Uuid,
    server: String,
    path: String,
    digest: String,
    checksum: String,
    external: String,
    library: Option<String>,
    sidecar: SidecarId,
    source: VersionId,
    reject: bool,
    expected: bool,
    desired: bool,
}
struct Local {
    sidecar: Option<SidecarId>,
    version: Option<VersionId>,
    flag: Option<PickFlag>,
}
impl Catalog {
    pub fn next_favorite_intent(&self, after: i64) -> AppResult<Option<(i64, OperationId)>> {
        let row: Option<(i64,String)> = self.db.query_row("SELECT r.rowid,r.id FROM favorite_intents r WHERE r.state IN('prepared','sent') AND NOT EXISTS(SELECT 1 FROM favorite_intents p WHERE p.group_id=r.group_id AND json_extract(p.body,'$.server')=json_extract(r.body,'$.server') AND json_extract(p.body,'$.asset')=json_extract(r.body,'$.asset') AND p.rowid<r.rowid AND p.state IN('prepared','sent')) ORDER BY r.rowid<=?,r.rowid LIMIT 1",[after],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        row.map(|(row, id)| Ok((row, serde_json::from_value(id.into())?)))
            .transpose()
    }
    pub fn favorite_pending_version(&self, group: GroupId) -> AppResult<Option<VersionId>> {
        let id: Option<String> = self
            .db
            .query_row(
                "SELECT pending_version FROM favorite_baselines WHERE group_id=?",
                [self.canonical_group(group)?.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        id.map(|id| Ok(serde_json::from_value(id.into())?))
            .transpose()
    }
}
fn local(
    catalog: &Catalog,
    root: &RootHandle,
    group: GroupId,
    adapter: &RapidRawAdapter,
) -> AppResult<Local> {
    let primary = super::ratings::primary_raw(&catalog.group(group)?);
    let Some(primary) = primary else {
        return Ok(Local {
            sidecar: None,
            version: None,
            flag: None,
        });
    };
    let id: Option<String> = catalog
        .db
        .query_row(
            "SELECT id FROM sidecars WHERE group_id=? AND path=? AND owner='rapidraw-base'",
            params![group.to_string(), format!("{}.rrdata", primary.as_str())],
            |r| r.get(0),
        )
        .optional()?;
    let sidecar = id.map(|id| serde_json::from_value(id.into())).transpose()?;
    let version = sidecar
        .map(|s| catalog.local_head(s))
        .transpose()?
        .flatten();
    let flag = version
        .map(|v| catalog.version(v))
        .transpose()?
        .map(|v| -> AppResult<Option<PickFlag>> {
            match v.body {
                VersionBody::Reset => Ok(None),
                _ => Ok(Some(
                    super::ratings::load_rating_document(root, &v, adapter)?.flag,
                )),
            }
        })
        .transpose()?
        .flatten();
    Ok(Local {
        sidecar,
        version,
        flag,
    })
}
fn observation(value: Option<bool>) -> RatingObservation {
    value
        .map(|v| RatingObservation::Stars(Rating::try_from(u8::from(v)).unwrap()))
        .unwrap_or(RatingObservation::Unobserved)
}
impl FavoriteSynchronizer<'_> {
    fn primary_is_current(&self, intent: &Intent) -> AppResult<bool> {
        let group = self.catalog.group(intent.group)?;
        let Some(primary) = super::ratings::primary_raw(&group) else {
            return Ok(false);
        };
        if !group
            .members
            .iter()
            .any(|m| m.role == MediaRole::Raw && m.path == primary)
        {
            return Ok(false);
        }
        let row: Option<(Option<String>, String, String)> = self
            .catalog
            .db
            .query_row(
                "SELECT group_id,path,owner FROM sidecars WHERE id=?",
                [intent.sidecar.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((Some(group), path, owner)) = row else {
            return Ok(false);
        };
        Ok(self
            .catalog
            .canonical_group(serde_json::from_value(group.into())?)?
            == intent.group
            && path == format!("{}.rrdata", primary.as_str())
            && owner == "rapidraw-base")
    }
    fn retire_baseline(
        &mut self,
        group: GroupId,
        previous: &Baseline,
        next: &Baseline,
    ) -> AppResult<()> {
        let body = serde_json::to_string(previous)?;
        let id = Digest::of(serde_json::to_vec(&(group, &body))?.as_slice());
        let tx = self.catalog.db.transaction()?;
        tx.execute("INSERT INTO favorite_baseline_history(id,group_id,body,source_version,pending_version) VALUES(?,?,?,?,?) ON CONFLICT DO NOTHING",params![id.as_str(),group.to_string(),body,previous.source_version.map(|v|v.to_string()),previous.pending_version.map(|v|v.to_string())])?;
        tx.execute(
            "UPDATE favorite_intents SET state='superseded' WHERE group_id=? AND state='prepared'",
            [group.to_string()],
        )?;
        tx.execute("UPDATE favorite_baselines SET body=?,source_version=?,pending_version=? WHERE group_id=?",params![serde_json::to_string(next)?,next.source_version.map(|v|v.to_string()),next.pending_version.map(|v|v.to_string()),group.to_string()])?;
        tx.commit()?;
        Ok(())
    }
    fn supersede(&mut self, id: OperationId) -> AppResult<()> {
        let (group, body): (String, String) = self.catalog.db.query_row(
            "SELECT group_id,body FROM favorite_intents WHERE id=?",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let intent: Intent = serde_json::from_str(&body)?;
        let previous: Option<String> = self
            .catalog
            .db
            .query_row(
                "SELECT body FROM favorite_baselines WHERE group_id=?",
                [&group],
                |r| r.get(0),
            )
            .optional()?;
        let tx = self.catalog.db.transaction()?;
        let changed = tx.execute(
            "UPDATE favorite_intents SET state='superseded' WHERE id=? AND state='prepared'",
            [id.to_string()],
        )?;
        if changed > 0
            && let Some(previous) = previous
        {
            let mut baseline: Baseline = serde_json::from_str(&previous)?;
            if baseline.outbound == Some(id)
                || (baseline.outbound.is_none()
                    && baseline.pending == Some(intent.desired)
                    && baseline.source_version == Some(intent.source))
            {
                baseline.outbound = None;
                baseline.pending = None;
                tx.execute(
                    "UPDATE favorite_baselines SET body=? WHERE group_id=?",
                    params![serde_json::to_string(&baseline)?, group],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    fn reconcile_outbound(
        &mut self,
        group: GroupId,
        baseline: &mut Baseline,
        local: &Local,
        remote: Option<bool>,
    ) -> AppResult<bool> {
        if baseline.outbound.is_none() && baseline.pending.is_some() {
            let id:Option<String>=self.catalog.db.query_row("SELECT id FROM favorite_intents WHERE group_id=? AND json_extract(body,'$.server')=? AND json_extract(body,'$.asset')=? AND json_extract(body,'$.sidecar')=? AND state IN('prepared','sent') ORDER BY rowid DESC LIMIT 1",params![group.to_string(),baseline.server,baseline.asset.to_string(),local.sidecar.map(|v|v.to_string())],|r|r.get(0)).optional()?;
            baseline.outbound = id.map(|id| serde_json::from_value(id.into())).transpose()?;
        }
        let Some(id) = baseline.outbound else {
            return Ok(false);
        };
        let (body, state): (String, String) = self.catalog.db.query_row(
            "SELECT body,state FROM favorite_intents WHERE id=?",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let intent: Intent = serde_json::from_str(&body)?;
        if state == "sent" {
            return Ok(true);
        }
        if state == "prepared"
            && local.version == Some(intent.source)
            && local.sidecar == Some(intent.sidecar)
            && remote != Some(intent.desired)
        {
            return Ok(!intent.reject);
        }
        if state == "prepared" {
            self.supersede(id)?;
        }
        if state == "confirmed" {
            baseline.common = Some(intent.desired);
            baseline.applied = Some(intent.desired);
        }
        baseline.pending = None;
        baseline.outbound = None;
        Ok(false)
    }

    pub fn poll(&mut self, scope: RatingScope) -> AppResult<RatingPollReport> {
        use crate::ratings::*;
        if scope.limit == 0 || scope.limit > 128 {
            return Err(AppError::invalid("favorite poll batch must be 1..128"));
        }
        let root = RootHandle::open(&scope.state_root, RootPolicy::Local)?;
        let adapter = RapidRawAdapter::for_version(&scope.editor_version)?;
        let server = self.client.origin().origin().ascii_serialization();
        let mut q=self.catalog.db.prepare("SELECT asset,group_id FROM mappings WHERE server=? AND stale=0 AND asset>? ORDER BY asset LIMIT ?")?;
        let rows = q
            .query_map(
                params![
                    server,
                    scope.after.map(|v| v.to_string()).unwrap_or_default(),
                    scope.limit
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);
        let mut report = RatingPollReport::default();
        for (asset, group) in rows {
            let asset = uuid::Uuid::parse_str(&asset)
                .map_err(|_| AppError::integrity("invalid favorite asset"))?;
            report.next = Some(asset);
            let group = self
                .catalog
                .canonical_group(serde_json::from_value(group.into())?)?;
            let count: u64 = self.catalog.db.query_row(
                "SELECT count(*) FROM mappings WHERE server=? AND group_id=? AND stale=0",
                params![server, group.to_string()],
                |r| r.get(0),
            )?;
            let authority: Option<(String, String)> = self
                .catalog
                .db
                .query_row(
                    "SELECT server,asset FROM rating_authorities WHERE group_id=?",
                    [group.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if authority
                .as_ref()
                .is_some_and(|a| a != &(server.clone(), asset.to_string()))
            {
                continue;
            }
            if count > 1 && authority.is_none() {
                self.catalog.record_rating_conflict(group,"rating_authority",serde_json::json!({"server":server,"required":"select an authoritative JPEG asset"}))?;
                report.conflicts += 1;
                continue;
            }
            let remote = match self.client.asset(asset) {
                Ok(a) => a,
                Err(_) => {
                    report.pending += 1;
                    continue;
                }
            };
            let binding:(String,String,Option<String>)=self.catalog.db.query_row("SELECT external_path,remote_checksum,library FROM mappings WHERE server=? AND asset=? AND stale=0",params![server,asset.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if remote.original_path != binding.0
                || remote.checksum != binding.1
                || remote.library_id.map(|v| v.to_string()) != binding.2
                || remote.kind != "IMAGE"
                || remote.original_mime_type.as_deref() != Some("image/jpeg")
            {
                self.catalog.db.execute(
                    "UPDATE mappings SET stale=1 WHERE server=? AND asset=?",
                    params![server, asset.to_string()],
                )?;
                self.catalog.record_rating_conflict(
                    group,
                    "mapping_identity",
                    serde_json::json!({"asset":asset,"reason":"upstream JPEG identity changed"}),
                )?;
                report.conflicts += 1;
                continue;
            }
            let local = match local(self.catalog, &root, group, &adapter) {
                Ok(l) => l,
                Err(e) => {
                    self.conflict(group, serde_json::json!({"reason":e.to_string()}))?;
                    report.conflicts += 1;
                    continue;
                }
            };
            let encoded: Option<String> = self
                .catalog
                .db
                .query_row(
                    "SELECT body FROM favorite_baselines WHERE group_id=?",
                    [group.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            let mut baseline: Baseline = encoded
                .map(|s| serde_json::from_str(&s))
                .transpose()?
                .unwrap_or_default();
            let jpeg_changed = !baseline.server.is_empty()
                && (baseline.server != server || baseline.asset != asset);
            if jpeg_changed && authority.as_ref() != Some(&(server.clone(), asset.to_string())) {
                self.conflict(
                    group,
                    serde_json::json!({"reason":"favorite authority changed; baseline retained"}),
                )?;
                report.conflicts += 1;
                continue;
            }
            let previous_sidecar = baseline
                .source_version
                .or(baseline.pending_version)
                .map(|v| self.catalog.version(v).map(|v| v.sidecar))
                .transpose()?;
            let raw_changed = previous_sidecar.is_some() && previous_sidecar != local.sidecar;
            if jpeg_changed || raw_changed {
                let mut next = Baseline {
                    server: server.clone(),
                    asset,
                    pending_version: if raw_changed {
                        None
                    } else {
                        baseline.pending_version
                    },
                    ..Default::default()
                };
                if !jpeg_changed && let Some(id) = baseline.outbound {
                    let sent: bool = self.catalog.db.query_row(
                        "SELECT state='sent' FROM favorite_intents WHERE id=?",
                        [id.to_string()],
                        |r| r.get(0),
                    )?;
                    if sent {
                        next.outbound = Some(id);
                    }
                }
                if next.pending_version.is_some()
                    && local.flag.and_then(PickFlag::favorite) == remote.is_favorite
                    && remote.is_favorite.is_some()
                {
                    next.common = remote.is_favorite;
                    next.applied = remote.is_favorite;
                    next.pending = remote.is_favorite;
                }
                self.retire_baseline(group, &baseline, &next)?;
                baseline = next;
            }
            let remote = remote.is_favorite;
            if local.flag == Some(PickFlag::Unsupported) {
                self.conflict(
                    group,
                    serde_json::json!({"reason":"unsupported native flag"}),
                )?;
                report.conflicts += 1;
                continue;
            }
            if self.reconcile_outbound(group, &mut baseline, &local, remote)? {
                report.pending += 1;
                report.observed += 1;
                continue;
            }
            let value = local.flag.and_then(PickFlag::favorite);
            let reject = local.flag == Some(PickFlag::Reject);
            if let (Some(value), Some(remote), Some(version)) = (value, remote, local.version)
                && value == remote
            {
                self.catalog.queue_favorite_convergence(
                    OperationId::new(),
                    group,
                    &server,
                    asset,
                    version,
                    value,
                )?;
            }
            let decision = if reject {
                if remote == Some(true) {
                    RatingDecision::PublishLocal(Rating::try_from(0)?)
                } else {
                    RatingDecision::Converge(Rating::try_from(0)?)
                }
            } else if let (Some(value), Some(remote)) = (value, remote)
                && value == remote
                && baseline.pending.is_none()
            {
                RatingDecision::Converge(Rating::try_from(u8::from(value))?)
            } else {
                reconcile_rating(&RatingInputs {
                    last_common: observation(baseline.common),
                    last_applied_local: observation(baseline.applied),
                    local: observation(value),
                    remote: observation(remote),
                    pending_canonical: baseline
                        .pending
                        .map(|v| Rating::try_from(u8::from(v)))
                        .transpose()?,
                    local_observation: local.version.map(|v| OperationId(v.0)),
                    remote_observation: None,
                })
            };
            match decision {
                RatingDecision::PublishLocal(wanted) => {
                    if let (Some(expected), Some(sidecar), Some(source)) =
                        (remote, local.sidecar, local.version)
                    {
                        self.prepare(
                            group,
                            asset,
                            sidecar,
                            source,
                            reject,
                            expected,
                            u8::from(wanted) != 0,
                            &mut baseline,
                        )?;
                        baseline.pending = Some(u8::from(wanted) != 0);
                        report.pending += 1;
                    }
                }
                RatingDecision::AdoptRemote(wanted) | RatingDecision::KeepPending(wanted) => {
                    let wanted = u8::from(wanted) != 0;
                    let outbound = baseline.pending_version.is_none()
                        && value == Some(wanted)
                        && baseline.pending == Some(wanted);
                    if !outbound
                        && (baseline.pending != Some(wanted)
                            || baseline.source_version != local.version
                            || baseline.pending_version.is_none())
                    {
                        match self.derive(&root, &adapter, group, &local, &baseline, wanted) {
                            Ok(version) => baseline.pending_version = version,
                            Err(e)
                                if matches!(
                                    e.code,
                                    ErrorCode::Capacity | ErrorCode::Unavailable
                                ) =>
                            {
                                report.pending += 1;
                                report.retry = true;
                                continue;
                            }
                            Err(e) => {
                                self.conflict(group, serde_json::json!({"reason":e.to_string()}))?;
                                report.conflicts += 1;
                                continue;
                            }
                        }
                    }
                    if remote == Some(wanted) {
                        baseline.common = remote;
                    }
                    baseline.pending = Some(wanted);
                    report.pending += 1;
                }
                RatingDecision::Converge(wanted) => {
                    let wanted = u8::from(wanted) != 0;
                    if !reject && let Some(version) = baseline.pending_version {
                        let document = super::ratings::load_rating_document(
                            &root,
                            &self.catalog.version(version)?,
                            &adapter,
                        )?;
                        if document.flag.favorite() != Some(wanted) {
                            baseline.pending_version =
                                self.derive(&root, &adapter, group, &local, &baseline, wanted)?;
                        }
                    }
                    if remote.is_some() {
                        baseline.common = Some(wanted);
                    }
                    baseline.applied = value;
                    baseline.pending = None;
                }
                RatingDecision::Conflict { .. } | RatingDecision::Unsupported => {
                    self.conflict(group,serde_json::json!({"local":local.flag,"remote":remote,"pending":baseline.pending}))?;
                    report.conflicts += 1;
                }
                RatingDecision::Unchanged => {}
            }
            if reject && baseline.pending_version.is_some() {
                let old = super::ratings::load_rating_document(
                    &root,
                    &self.catalog.version(baseline.pending_version.unwrap())?,
                    &adapter,
                )?;
                if old.flag != PickFlag::Reject || baseline.source_version != local.version {
                    baseline.pending_version =
                        self.derive(&root, &adapter, group, &local, &baseline, false)?;
                }
                baseline.pending = None;
                baseline.applied = Some(false);
            }
            baseline.source_version = local.version;
            baseline.server = server.clone();
            baseline.asset = asset;
            self.catalog.db.execute("INSERT INTO favorite_baselines(group_id,body,source_version,pending_version) VALUES(?,?,?,?) ON CONFLICT(group_id) DO UPDATE SET body=excluded.body,source_version=excluded.source_version,pending_version=excluded.pending_version",params![group.to_string(),serde_json::to_string(&baseline)?,baseline.source_version.map(|v|v.to_string()),baseline.pending_version.map(|v|v.to_string())])?;
            report.observed += 1;
        }
        Ok(report)
    }
    fn conflict(&mut self, group: GroupId, details: serde_json::Value) -> AppResult<()> {
        self.catalog
            .record_rating_conflict(group, "favorite", details)?;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn prepare(
        &mut self,
        group: GroupId,
        asset: uuid::Uuid,
        sidecar: SidecarId,
        source: VersionId,
        reject: bool,
        expected: bool,
        desired: bool,
        baseline: &mut Baseline,
    ) -> AppResult<()> {
        let server = self.client.origin().origin().ascii_serialization();
        let (path,digest,checksum,external,library):(String,String,String,String,Option<String>)=self.catalog.db.query_row("SELECT path,digest,remote_checksum,external_path,library FROM mappings WHERE server=? AND asset=? AND stale=0",params![server,asset.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        let intent = Intent {
            group,
            asset,
            server,
            path,
            digest,
            checksum,
            external,
            library,
            sidecar,
            source,
            reject,
            expected,
            desired,
        };
        let body = serde_json::to_string(&intent)?;
        let old:Option<String>=self.catalog.db.query_row("SELECT id FROM favorite_intents WHERE group_id=? AND state IN('prepared','sent') AND body=? ORDER BY rowid DESC LIMIT 1",params![group.to_string(),body],|r|r.get(0)).optional()?;
        let id = old
            .map(|id| serde_json::from_value(id.into()))
            .transpose()?
            .unwrap_or_else(OperationId::new);
        baseline.outbound = Some(id);
        baseline.pending = Some(desired);
        baseline.server = intent.server.clone();
        baseline.asset = asset;
        let tx = self.catalog.db.transaction()?;
        tx.execute(
            "UPDATE favorite_intents SET state='superseded' WHERE group_id=? AND state='prepared' AND id<>?",
            params![group.to_string(),id.to_string()],
        )?;
        tx.execute(
            "INSERT INTO favorite_intents(id,group_id,body,state) VALUES(?,?,?,'prepared') ON CONFLICT(id) DO NOTHING",
            params![id.to_string(), group.to_string(), body],
        )?;
        tx.execute("INSERT INTO favorite_baselines(group_id,body,source_version,pending_version) VALUES(?,?,?,?) ON CONFLICT(group_id) DO UPDATE SET body=excluded.body,source_version=excluded.source_version,pending_version=excluded.pending_version",params![group.to_string(),serde_json::to_string(&baseline)?,baseline.source_version.map(|v|v.to_string()),baseline.pending_version.map(|v|v.to_string())])?;
        tx.execute(
            "UPDATE residency SET protected=1 WHERE group_id=?",
            [group.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    fn derive(
        &mut self,
        root: &RootHandle,
        adapter: &RapidRawAdapter,
        group: GroupId,
        local: &Local,
        baseline: &Baseline,
        wanted: bool,
    ) -> AppResult<Option<VersionId>> {
        let sidecar = if let Some(s) = local.sidecar {
            s
        } else {
            let Some(primary) = super::ratings::primary_raw(&self.catalog.group(group)?) else {
                return Ok(None);
            };
            let path = ArchivePath::parse(&format!("{}.rrdata", primary.as_str()))?;
            let id = self.catalog.managed_sidecar_id(&path)?;
            self.catalog
                .register_sidecar(id, Some(group), path, "rapidraw-base")?;
            id
        };
        let heads = self.catalog.heads(sidecar)?;
        if heads.len() > 128 {
            return Err(AppError::new(
                ErrorCode::Unavailable,
                "metadata heads exceed bounded favorite merge",
            ));
        }
        let mut parents = heads
            .into_iter()
            .chain(local.version)
            .chain(baseline.pending_version)
            .collect::<Vec<_>>();
        parents.sort();
        parents.dedup();
        let mut selected: Option<(VersionId, EditDocument)> = None;
        for id in &parents {
            let doc =
                super::ratings::load_rating_document(root, &self.catalog.version(*id)?, adapter)?;
            if let Some((previous, old)) = &selected {
                let use_new = if self.catalog.is_ancestor(*previous, *id)? {
                    true
                } else if self.catalog.is_ancestor(*id, *previous)?
                    || old.non_flag() == doc.non_flag()
                {
                    false
                } else {
                    let base = self
                        .catalog
                        .common_version(*previous, *id)?
                        .ok_or_else(|| {
                            AppError::new(
                                ErrorCode::Conflict,
                                "independent edit histories retained",
                            )
                        })?;
                    let base = super::ratings::load_rating_document(
                        root,
                        &self.catalog.version(base)?,
                        adapter,
                    )?;
                    if old.non_flag() == base.non_flag() {
                        true
                    } else if doc.non_flag() == base.non_flag() {
                        false
                    } else {
                        return Err(AppError::new(
                            ErrorCode::Conflict,
                            "divergent non-flag edits retained",
                        ));
                    }
                };
                if use_new {
                    selected = Some((*id, doc));
                }
            } else {
                selected = Some((*id, doc));
            }
        }
        let mut doc = if let Some((_, d)) = selected {
            d
        } else {
            adapter.inspect(&adapter.initial_document(Rating::try_from(0)?)?)?
        };
        if local.flag == Some(PickFlag::Reject) && doc.flag != PickFlag::Reject {
            let mut value = doc.value().clone();
            value["flag"] = "reject".into();
            doc = adapter.inspect(&serde_json::to_vec(&value)?)?;
        }
        let bytes = adapter.with_favorite(&doc, wanted)?;
        let key = Digest::of(
            serde_json::to_vec(&("favorite-derive-v1", sidecar, &parents, Digest::of(&bytes)))?
                .as_slice(),
        );
        let hash = blake3::hash(key.as_str().as_bytes());
        let mut id = [0; 16];
        id.copy_from_slice(&hash.as_bytes()[..16]);
        let operation = OperationId(uuid::Uuid::from_bytes(id));
        Ok(Some(VersionStore::new(self.catalog, root).record(
            SnapshotIntent {
                sidecar,
                parents,
                operation,
                payload: SnapshotPayload::Content(bytes),
                origin: VersionOrigin::Derived,
            },
        )?))
    }
    pub fn advance(&mut self, id: OperationId) -> AppResult<RatingSyncReport> {
        let (body, state): (String, String) = self.catalog.db.query_row(
            "SELECT body,state FROM favorite_intents WHERE id=?",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut intent: Intent = serde_json::from_str(&body)?;
        intent.group = self.catalog.canonical_group(intent.group)?;
        let waiting = |reason: &str| RatingSyncReport {
            waiting: true,
            reason: Some(reason.into()),
            ..Default::default()
        };
        if state == "confirmed" {
            return Ok(RatingSyncReport {
                confirmed: true,
                ..Default::default()
            });
        }
        if state != "prepared" && state != "sent" {
            return Ok(waiting("favorite intent is settled"));
        }
        if intent.server != self.client.origin().origin().ascii_serialization() {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "favorite origin changed",
            ));
        }
        let binding:Option<(String,String,String,String,Option<String>,String)>=self.catalog.db.query_row("SELECT path,digest,remote_checksum,external_path,library,group_id FROM mappings WHERE server=? AND asset=? AND stale=0",params![intent.server,intent.asset.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
        let Some((path, digest, checksum, external, library, group)) = binding else {
            return Ok(waiting("favorite mapping is stale"));
        };
        if (path, digest, checksum, external, library)
            != (
                intent.path.clone(),
                intent.digest.clone(),
                intent.checksum.clone(),
                intent.external.clone(),
                intent.library.clone(),
            )
            || self
                .catalog
                .canonical_group(serde_json::from_value(group.into())?)?
                != intent.group
        {
            return Ok(waiting("favorite JPEG binding changed"));
        }
        let prior:bool=self.catalog.db.query_row("SELECT EXISTS(SELECT 1 FROM favorite_intents WHERE group_id=? AND json_extract(body,'$.server')=? AND json_extract(body,'$.asset')=? AND rowid<(SELECT rowid FROM favorite_intents WHERE id=?) AND state IN('prepared','sent'))",params![intent.group.to_string(),intent.server,intent.asset.to_string(),id.to_string()],|r|r.get(0))?;
        if prior {
            return Ok(waiting("earlier favorite intent must settle"));
        }
        if state == "prepared" {
            if !self.primary_is_current(&intent)?
                || self.catalog.local_head(intent.sidecar)? != Some(intent.source)
            {
                self.supersede(id)?;
                return Ok(waiting("favorite source superseded"));
            }
            let mut q = self
                .catalog
                .db
                .prepare("SELECT kind,details FROM conflicts WHERE group_id=? AND resolved=0")?;
            let conflicts = q
                .query_map([intent.group.to_string()], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(q);
            for (kind, details) in conflicts {
                let details: serde_json::Value = serde_json::from_str(&details)?;
                if !(intent.reject
                    && !intent.desired
                    && ((kind == "rating"
                        && details.get("local") == Some(&serde_json::json!("Rejected")))
                        || (kind == "favorite"
                            && details.get("local").is_some()
                            && details.get("remote").is_some())))
                {
                    return Ok(waiting(
                        "unresolved photograph conflict blocks favorite write",
                    ));
                }
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
            let count: u64 = self.catalog.db.query_row(
                "SELECT count(*) FROM mappings WHERE server=? AND group_id=? AND stale=0",
                params![intent.server, intent.group.to_string()],
                |r| r.get(0),
            )?;
            if (count > 1 && authority.is_none())
                || authority.is_some_and(|a| a != (intent.server.clone(), intent.asset.to_string()))
            {
                return Ok(waiting("favorite authority changed"));
            }
            if !self.client.outbound_enabled() {
                return Ok(waiting("outbound favorites disabled"));
            }
            self.catalog.db.execute(
                "UPDATE favorite_intents SET state='sent' WHERE id=?",
                [id.to_string()],
            )?;
            let outcome = self.client.set_favorite(FavoriteWrite {
                asset: intent.asset,
                expected: intent.expected,
                desired: intent.desired,
                jpeg_checksum: intent.checksum.clone(),
                external_path: intent.external.clone(),
                library_id: intent
                    .library
                    .as_deref()
                    .map(uuid::Uuid::parse_str)
                    .transpose()
                    .map_err(|_| AppError::integrity("invalid favorite library identity"))?,
            });
            return match outcome {
                Ok(FavoriteOutcome::Uncertain) => {
                    Ok(waiting("favorite outcome uncertain; read before any retry"))
                }
                Ok(outcome) => self.finish(id, &intent, outcome),
                Err(error) => {
                    self.catalog.db.execute(
                        "UPDATE favorite_intents SET state='prepared' WHERE id=?",
                        [id.to_string()],
                    )?;
                    Ok(waiting(&error.to_string()))
                }
            };
        }
        match self.client.favorite_progress(intent.asset) {
            FavoriteProgress::Pending => return Ok(waiting("favorite request in flight")),
            FavoriteProgress::Finished(Ok(FavoriteOutcome::Uncertain)) | FavoriteProgress::None => {
            }
            FavoriteProgress::Finished(Ok(outcome)) => return self.finish(id, &intent, outcome),
            FavoriteProgress::Finished(Err(error)) => {
                self.catalog.db.execute(
                    "UPDATE favorite_intents SET state='prepared' WHERE id=?",
                    [id.to_string()],
                )?;
                return Ok(waiting(&error.to_string()));
            }
        }
        let asset = self.client.asset(intent.asset)?;
        if asset.checksum != intent.checksum
            || asset.original_path != intent.external
            || asset.library_id.map(|v| v.to_string()) != intent.library
            || asset.kind != "IMAGE"
            || asset.original_mime_type.as_deref() != Some("image/jpeg")
        {
            return Ok(waiting("readback JPEG identity changed"));
        }
        let Some(observed) = asset.is_favorite else {
            return Ok(waiting("favorite readback unobserved"));
        };
        self.finish(
            id,
            &intent,
            if observed == intent.desired {
                FavoriteOutcome::Confirmed
            } else {
                FavoriteOutcome::Conflict { observed }
            },
        )
    }
    fn finish(
        &mut self,
        id: OperationId,
        intent: &Intent,
        outcome: FavoriteOutcome,
    ) -> AppResult<RatingSyncReport> {
        if matches!(outcome, FavoriteOutcome::IdentityChanged) {
            self.catalog.db.execute(
                "UPDATE favorite_intents SET state='identity_changed' WHERE id=?",
                [id.to_string()],
            )?;
            self.catalog.record_rating_conflict(intent.group,"mapping_identity",serde_json::json!({"intent":id,"asset":intent.asset,"reason":"live JPEG identity changed during favorite write"}))?;
            return Ok(RatingSyncReport {
                conflict: true,
                ..Default::default()
            });
        }
        let selected: Option<(String, String)> = self
            .catalog
            .db
            .query_row(
                "SELECT server,asset FROM rating_authorities WHERE group_id=?",
                [intent.group.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let authority_changed = selected
            .is_some_and(|a| a != (intent.server.clone(), intent.asset.to_string()))
            || !self.primary_is_current(intent)?;
        if let FavoriteOutcome::Conflict { observed } = outcome
            && ((intent.reject && !intent.desired) || authority_changed)
        {
            let tx = self.catalog.db.transaction()?;
            tx.execute(
                "INSERT INTO favorite_receipts(id,intent,body) VALUES(?,?,?)",
                params![
                    OperationId::new().to_string(),
                    id.to_string(),
                    serde_json::to_string(
                        &serde_json::json!({"binding":intent,"observed":observed})
                    )?
                ],
            )?;
            tx.execute(
                "UPDATE favorite_intents SET state='readback_different' WHERE id=?",
                [id.to_string()],
            )?;
            tx.commit()?;
            return Ok(RatingSyncReport{waiting:true,reason:Some("Reject readback retained; fresh flag and binding observation required for a new intent".into()),..Default::default()});
        }
        let confirmed = matches!(outcome, FavoriteOutcome::Confirmed);
        if matches!(outcome, FavoriteOutcome::Uncertain) {
            return Ok(RatingSyncReport {
                waiting: true,
                ..Default::default()
            });
        }
        self.catalog.db.execute(
            "UPDATE favorite_intents SET state=? WHERE id=?",
            params![
                if confirmed { "confirmed" } else { "conflict" },
                id.to_string()
            ],
        )?;
        if !confirmed {
            self.conflict(intent.group,serde_json::json!({"intent":id,"expected":intent.expected,"desired":intent.desired,"local":if intent.desired { PickFlag::Pick } else { PickFlag::Neutral },"remote":match outcome { FavoriteOutcome::Conflict { observed }=>Some(observed), _=>None },"reason":"favorite changed concurrently"}))?;
        }
        Ok(RatingSyncReport {
            confirmed,
            conflict: !confirmed,
            ..Default::default()
        })
    }
}
