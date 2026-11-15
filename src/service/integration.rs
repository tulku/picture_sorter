use crate::{
    config::Config,
    domain::*,
    error::{AppResult, ErrorCode},
    fs::RootHandle,
    import::ImportCoordinator,
    qualification::EditorRuntimeIdentity,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::ImmichConfig, fs::RootPolicy, qualification::*};

    #[test]
    fn favorite_evidence_does_not_enable_stars_and_readonly_keeps_qualified_inbound() {
        let temp = tempfile::tempdir().unwrap();
        let root = RootHandle::open(temp.path(), RootPolicy::Local).unwrap();
        let mut config = Config::new(
            temp.path().to_path_buf(),
            temp.path().to_path_buf(),
            temp.path().to_path_buf(),
        );
        let binary = temp.path().join("editor");
        std::fs::write(&binary, b"fixture editor").unwrap();
        config.rapidraw_binary = Some(binary.clone());
        let origin = url::Url::parse("https://fixture.invalid/").unwrap();
        config.immich = Some(ImmichConfig {
            origin: origin.to_string(),
            key_file: temp.path().join("key"),
            library_id: None,
            write_mode: "disabled".into(),
        });
        let report = |capability: &str,
                      backend: &str,
                      version: &str,
                      fingerprint,
                      scope,
                      checks: &[&str]| CapabilityReport {
            schema_version: 1,
            capability: capability.into(),
            backend_or_application: backend.into(),
            tested_version: version.into(),
            mount_identity: None,
            config_fingerprint: fingerprint,
            scope,
            results: checks
                .iter()
                .map(|name| ProbeResult {
                    name: (*name).into(),
                    outcome: ProbeOutcome::Pass,
                    detail: "fixture".into(),
                })
                .collect(),
            limitations: vec![],
        };
        let native = report(
            "rapidraw-transform",
            "RapidRAW",
            &config.rapidraw_version,
            editor_fingerprint(&config.rapidraw_version, Some(&binary)).unwrap(),
            ProbeScope::EditorRuntime,
            &[
                "save_three",
                "save_five",
                "save_zero",
                "reopen",
                "reset_regeneration",
                "runtime_application",
                "save_pick",
                "clear_pick",
                "reject_priority",
            ],
        );
        let scope = ProbeScope::IsolatedRuntime {
            origin: origin.to_string(),
            asset: uuid::Uuid::new_v4(),
        };
        let favorite = report(
            "immich-favorites",
            "immich",
            "3.4.0",
            favorite_fingerprint(&origin, "3.4.0"),
            scope.clone(),
            &[
                "supported_schema",
                "favorite_true",
                "favorite_false",
                "restore_original",
            ],
        );
        for (name, report) in [("native.json", native), ("favorite.json", favorite)] {
            root.write_new(
                &ArchivePath::parse(name).unwrap(),
                &serde_json::to_vec(&report).unwrap(),
            )
            .unwrap();
            config.capability_evidence.push(temp.path().join(name));
        }
        let identity = EditorRuntimeIdentity::capture(&config).unwrap().unwrap();
        let mut integration = Integration::new(&config, &root, &root);
        assert!(integration.transform_allowed(&config, Some(&identity), true));
        assert!(!integration.transform_allowed(&config, Some(&identity), false));
        assert!(
            !integration
                .write_decision(&config, Some(&identity), true)
                .allowed
        );
        let rating = report(
            "immich-ratings",
            "immich",
            "3.4.0",
            rating_fingerprint(&origin, "3.4.0"),
            scope,
            &["supported_schema"],
        );
        root.write_new(
            &ArchivePath::parse("rating.json").unwrap(),
            &serde_json::to_vec(&rating).unwrap(),
        )
        .unwrap();
        config
            .capability_evidence
            .push(temp.path().join("rating.json"));
        assert!(integration.transform_allowed(&config, Some(&identity), false));
        assert!(
            !integration
                .write_decision(&config, Some(&identity), false)
                .allowed
        );
        config.immich.as_mut().unwrap().write_mode = "best_effort".into();
        assert!(
            integration
                .write_decision(&config, Some(&identity), false)
                .allowed
        );
        let cursor = Some(Some(uuid::Uuid::new_v4()));
        integration.favorite_cursor = cursor;
        integration.rating_cursor = cursor;
        integration.discovery_page = Some(4);
        integration.request_poll();
        assert_eq!(integration.favorite_cursor, cursor);
        assert_eq!(integration.rating_cursor, cursor);
        assert_eq!(integration.discovery_page, Some(4));
    }
}

#[derive(PartialEq, Eq)]
pub(super) struct IntegrationProgress {
    discovery_page: Option<u32>,
    discovery_turn: bool,
    rating_cursor: Option<Option<uuid::Uuid>>,
    favorite_cursor: Option<Option<uuid::Uuid>>,
    rating_intent_cursor: i64,
    favorite_intent_cursor: i64,
    rating_active_intent: Option<(i64, RatingIntentId)>,
    favorite_active_intent: Option<(i64, OperationId)>,
}
pub(super) struct Integration {
    discovery_library: Option<uuid::Uuid>,
    discovery_page: Option<u32>,
    discovery_turn: bool,
    pub(super) discovery_counts: crate::immich::DiscoveryCounts,
    pub(super) discovery_error: Option<String>,
    pub(super) http: Option<super::http::AsyncImmich>,
    pub(super) reason: Option<String>,
    rating_cursor: Option<Option<uuid::Uuid>>,
    favorite_cursor: Option<Option<uuid::Uuid>>,
    favorite_intent_cursor: i64,
    favorite_active_intent: Option<(i64, OperationId)>,
    favorite_turn: bool,
    rating_intent_cursor: i64,
    rating_active_intent: Option<(i64, RatingIntentId)>,
}
impl Integration {
    pub(super) fn discovery_status(
        &self,
        catalog: &crate::catalog::Catalog,
    ) -> AppResult<serde_json::Value> {
        let Some(http) = &self.http else {
            return Ok(serde_json::Value::Null);
        };
        let mapped: u64 = catalog.db.query_row(
            "SELECT count(*) FROM mappings WHERE server=? AND stale=0",
            [crate::immich::ImmichAccess::origin(http)
                .origin()
                .ascii_serialization()],
            |row| row.get(0),
        )?;
        Ok(
            serde_json::json!({"mapped":mapped,"scanning":self.discovery_page.is_some(),"counts":self.discovery_counts,"error":self.discovery_error}),
        )
    }
    pub(super) fn write_decision(
        &self,
        config: &Config,
        editor: Option<&EditorRuntimeIdentity>,
        favorite: bool,
    ) -> crate::qualification::CapabilityDecision {
        let allowed = config
            .immich
            .as_ref()
            .is_some_and(|immich| immich.write_mode == "best_effort")
            && self.transform_allowed(config, editor, favorite);
        crate::qualification::CapabilityDecision { allowed, reason: if allowed {"Native editor and matching Immich capability evidence present; writes enabled"} else {"Writes require matching native editor and Immich capability evidence and explicit best_effort"}.into() }
    }
    fn transform_allowed(
        &self,
        config: &Config,
        editor: Option<&EditorRuntimeIdentity>,
        favorite: bool,
    ) -> bool {
        use crate::qualification::*;
        self.http.as_ref().is_some_and(|http| {
            editor.is_some_and(|identity| {
                if favorite {
                    identity.pick_decision(config).allowed
                } else {
                    identity.decision(config).allowed
                }
            }) && load_registry(config)
                .unwrap_or_default()
                .allows(if favorite {
                    OperationKind::FavoriteWrite {
                        origin: crate::immich::ImmichAccess::origin(http),
                        asset: uuid::Uuid::nil(),
                        version: "",
                    }
                } else {
                    OperationKind::RatingWrite {
                        origin: crate::immich::ImmichAccess::origin(http),
                        asset: uuid::Uuid::nil(),
                        version: "",
                    }
                })
                .allowed
        })
    }
    pub(super) fn progress(&self) -> IntegrationProgress {
        IntegrationProgress {
            discovery_page: self.discovery_page,
            discovery_turn: self.discovery_turn,
            rating_cursor: self.rating_cursor,
            favorite_cursor: self.favorite_cursor,
            rating_intent_cursor: self.rating_intent_cursor,
            favorite_intent_cursor: self.favorite_intent_cursor,
            rating_active_intent: self.rating_active_intent,
            favorite_active_intent: self.favorite_active_intent,
        }
    }

    pub(super) fn new(config: &Config, local: &RootHandle, state: &RootHandle) -> Self {
        let (http, reason) = match super::http::AsyncImmich::new(config, local, state) {
            Ok(http) => (http, None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
            discovery_library: config
                .immich
                .as_ref()
                .and_then(|immich| immich.library_id.as_ref())
                .and_then(|id| uuid::Uuid::parse_str(id).ok()),
            discovery_page: None,
            discovery_turn: true,
            discovery_counts: Default::default(),
            discovery_error: None,
            http,
            reason,
            rating_cursor: None,
            favorite_cursor: None,
            favorite_intent_cursor: 0,
            favorite_active_intent: None,
            favorite_turn: false,
            rating_intent_cursor: 0,
            rating_active_intent: None,
        }
    }
    pub(super) fn request_poll(&mut self) {
        if self.http.is_some() {
            self.rating_cursor.get_or_insert(None);
            self.favorite_cursor.get_or_insert(None);
            if self.discovery_library.is_some() && self.discovery_page.is_none() {
                self.discovery_page = Some(1);
                self.discovery_counts = Default::default();
            }
        }
    }
    pub(super) fn pump(&mut self) {
        if let Some(http) = &self.http
            && let Err(error) = http.pump()
        {
            self.reason = Some(error.to_string());
        }
    }
    pub(super) fn advance(
        &mut self,
        engine: &mut ImportCoordinator,
        editor_identity: Option<&EditorRuntimeIdentity>,
    ) -> AppResult<()> {
        if self.http.is_none() {
            return Ok(());
        }
        let editor_allowed = engine.config.local_test_backend
            || self.transform_allowed(&engine.config, editor_identity, false);
        let favorite_allowed = engine.config.local_test_backend
            || editor_identity
                .as_ref()
                .is_some_and(|identity| identity.pick_decision(&engine.config).allowed);
        if let Some(http) = &self.http {
            if let (Some(library), Some(page)) = (self.discovery_library, self.discovery_page)
                && !http.in_flight()
            {
                self.discovery_turn = !self.discovery_turn;
                if self.discovery_turn {
                    match http.discover_page(library, page)? {
                        Some(Ok(result)) => {
                            let counts = crate::immich::discover_assets(
                                &mut engine.catalog,
                                crate::immich::ImmichAccess::origin(http),
                                &engine.config.immich_path_mappings,
                                &result.items,
                            )?;
                            self.discovery_counts.matched += counts.matched;
                            self.discovery_counts.skipped += counts.skipped;
                            self.discovery_counts.conflicts += counts.conflicts;
                            self.discovery_page = result.next_page;
                            self.discovery_error = None;
                            self.favorite_cursor.get_or_insert(None);
                            self.rating_cursor.get_or_insert(None);
                        }
                        Some(Err(error)) => {
                            self.discovery_error = Some(error.to_string());
                            self.discovery_page = None;
                        }
                        None => {}
                    }
                }
            }
            if let Some(after) = self.favorite_cursor.filter(|_| {
                favorite_allowed
                    && self.favorite_active_intent.is_none()
                    && self.rating_active_intent.is_none()
            }) {
                let scope = crate::immich::RatingScope {
                    state_root: engine.config.state_root.clone(),
                    editor_version: engine.config.rapidraw_version.clone(),
                    after,
                    limit: 1,
                };
                match (crate::immich::FavoriteSynchronizer {
                    catalog: &mut engine.catalog,
                    client: http,
                })
                .poll(scope)
                {
                    Ok(report) => {
                        if !report.retry && (report.observed > 0 || !http.in_flight()) {
                            self.favorite_cursor = report.next.map(Some);
                        }
                    }
                    Err(error) => {
                        self.reason = Some(error.to_string());
                        if error.code != ErrorCode::Capacity {
                            self.favorite_cursor = None;
                        }
                    }
                }
            }
            if let Some(after) = self.rating_cursor.filter(|_| {
                editor_allowed
                    && self.rating_active_intent.is_none()
                    && self.favorite_active_intent.is_none()
            }) {
                let config = &engine.config;
                let scope = crate::immich::RatingScope {
                    state_root: config.state_root.clone(),
                    editor_version: config.rapidraw_version.clone(),
                    after,
                    limit: 1,
                };
                match (crate::immich::RatingSynchronizer {
                    catalog: &mut engine.catalog,
                    client: http,
                })
                .poll(scope)
                {
                    Ok(report) => {
                        if report.observed > 0 || !http.in_flight() {
                            self.rating_cursor = report.next.map(Some);
                        }
                    }
                    Err(error) => {
                        self.reason = Some(error.to_string());
                        self.rating_cursor = None;
                    }
                }
            }
            if self.rating_active_intent.is_none()
                && self.favorite_active_intent.is_none()
                && !http.in_flight()
            {
                self.favorite_turn = !self.favorite_turn;
                if self.favorite_turn && favorite_allowed {
                    self.favorite_active_intent = engine
                        .catalog
                        .next_favorite_intent(self.favorite_intent_cursor)?;
                }
                if self.favorite_active_intent.is_none() {
                    self.rating_active_intent = super::rating_queue::next_intent(
                        &engine.catalog,
                        self.rating_intent_cursor,
                    )?;
                }
                if self.rating_active_intent.is_none()
                    && self.favorite_active_intent.is_none()
                    && favorite_allowed
                {
                    self.favorite_active_intent = engine
                        .catalog
                        .next_favorite_intent(self.favorite_intent_cursor)?;
                }
            }
            if let Some((row, id)) = self.favorite_active_intent {
                match (crate::immich::FavoriteSynchronizer {
                    catalog: &mut engine.catalog,
                    client: http,
                })
                .advance(id)
                {
                    Ok(report) => {
                        if let Some(reason) = report.reason {
                            self.reason = Some(reason);
                        }
                    }
                    Err(error) => self.reason = Some(error.to_string()),
                }
                if !http.in_flight() {
                    self.favorite_intent_cursor = row;
                    self.favorite_active_intent = None;
                }
            }
            if let Some((row, id)) = self.rating_active_intent {
                match (crate::immich::RatingSynchronizer {
                    catalog: &mut engine.catalog,
                    client: http,
                })
                .advance(id)
                {
                    Ok(report) => {
                        if let Some(reason) = report.reason {
                            self.reason = Some(reason);
                        }
                    }
                    Err(error) => self.reason = Some(error.to_string()),
                }
                if !http.in_flight() {
                    self.rating_intent_cursor = row;
                    self.rating_active_intent = None;
                }
            }
        }
        Ok(())
    }
}
