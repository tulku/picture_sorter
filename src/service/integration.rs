use crate::{
    config::Config,
    domain::*,
    error::{AppResult, ErrorCode},
    fs::RootHandle,
    import::ImportCoordinator,
    qualification::EditorRuntimeIdentity,
};

pub(super) struct Integration {
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
    pub(super) fn new(config: &Config, local: &RootHandle, state: &RootHandle) -> Self {
        let (http, reason) = match super::http::AsyncImmich::new(config, local, state) {
            Ok(http) => (http, None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
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
            self.rating_cursor = Some(None);
            self.favorite_cursor = Some(None);
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
            || editor_identity
                .as_ref()
                .is_some_and(|identity| identity.decision(&engine.config).allowed);
        let favorite_allowed = engine.config.local_test_backend
            || editor_identity
                .as_ref()
                .is_some_and(|identity| identity.pick_decision(&engine.config).allowed);
        if let Some(http) = &self.http {
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
