use crate::{
    domain::*,
    error::{AppError, AppResult, ErrorCode},
};
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::Duration,
};
use url::Url;
pub const SCHEMA_REVISION: &str = "8303cb117e197c700332b33b61b45314c9cc5bb4";
pub const TESTED_VERSION: &str = "3.3.5";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetInfo {
    pub id: uuid::Uuid,
    #[serde(rename = "type")]
    pub kind: String,
    pub original_path: String,
    pub original_file_name: String,
    pub original_mime_type: Option<String>,
    pub library_id: Option<uuid::Uuid>,
    pub checksum: String,
    pub exif_info: Option<ExifInfo>,
}
#[derive(Debug, Clone)]
pub struct ExifInfo {
    pub rating: Option<i8>,
    pub rating_observed: bool,
}
impl<'de> Deserialize<'de> for ExifInfo {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("expected EXIF object"))?;
        match object.get("rating") {
            None => Ok(Self {
                rating: None,
                rating_observed: false,
            }),
            Some(value) => Ok(Self {
                rating: serde_json::from_value(value.clone()).map_err(serde::de::Error::custom)?,
                rating_observed: true,
            }),
        }
    }
}
impl Serialize for ExifInfo {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(usize::from(self.rating_observed)))?;
        if self.rating_observed {
            map.serialize_entry("rating", &self.rating)?;
        }
        map.end()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub version: String,
    pub version_url: String,
    pub licensed: bool,
}
pub struct ImmichClient {
    rating_mode: super::RatingWriteMode,
    rating_registry: crate::qualification::CapabilityRegistry,
    origin: Url,
    http: Client,
}
impl ImmichClient {
    pub fn new(origin: Url, key_file: &Path) -> AppResult<Self> {
        Self::new_with_timeout(origin, key_file, Duration::from_secs(5))
    }
    pub fn new_with_timeout(origin: Url, key_file: &Path, timeout: Duration) -> AppResult<Self> {
        if timeout.is_zero() {
            return Err(AppError::invalid("HTTP timeout must be positive"));
        }
        if !matches!(origin.scheme(), "http" | "https")
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
        {
            return Err(AppError::invalid(
                "Immich origin must contain only HTTP(S) scheme, host and port",
            ));
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(key_file)?;
        let m = file.metadata()?;
        // Secrets must be a regular owner-only file belonging to the effective local user.
        let uid = unsafe { libc::geteuid() };
        if !m.is_file() || m.uid() != uid || m.mode() & 0o777 != 0o600 || m.len() > 4096 {
            return Err(AppError::invalid(
                "Immich key requires an owned regular mode-0600 file of at most 4096 bytes",
            ));
        }
        let mut secret = String::new();
        file.take(4097).read_to_string(&mut secret)?;
        let mut key = HeaderValue::from_str(secret.trim())
            .map_err(|_| AppError::invalid("invalid Immich key file format"))?;
        key.set_sensitive(true);
        if secret.trim().is_empty() {
            return Err(AppError::invalid("empty Immich key file"));
        }
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", key);
        let http = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .timeout(timeout)
            .default_headers(headers)
            .build()
            .map_err(|_| AppError::invalid("cannot configure TLS HTTP client"))?;
        Ok(Self {
            origin,
            http,
            rating_mode: super::RatingWriteMode::Disabled,
            rating_registry: Default::default(),
        })
    }
    fn get<T: DeserializeOwned>(&self, path: &str) -> AppResult<T> {
        let url = self
            .origin
            .join(path)
            .map_err(|_| AppError::invalid("invalid API path"))?;
        if url.origin() != self.origin.origin() {
            return Err(AppError::invalid("API origin changed"));
        }
        let response = self.http.get(url).send().map_err(|_| {
            AppError::new(ErrorCode::Unavailable, "Immich network request unavailable")
        })?;
        let code = response.status().as_u16();
        if code != 200 {
            return Err(AppError::new(
                match code {
                    401 => ErrorCode::Authentication,
                    403 => ErrorCode::Permission,
                    404 => ErrorCode::NotFound,
                    _ => ErrorCode::Unavailable,
                },
                format!("Immich returned HTTP {code}"),
            ));
        }
        let mut bytes = Vec::new();
        response.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "Immich response exceeds supported bound",
            ));
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            AppError::new(
                ErrorCode::Unsupported,
                "Immich response does not match the supported schema",
            )
        })
    }
    pub fn asset(&self, id: uuid::Uuid) -> AppResult<AssetInfo> {
        let asset: AssetInfo = self.get(&format!("/api/assets/{id}"))?;
        if asset.id != id || asset.checksum.is_empty() {
            return Err(AppError::integrity("Immich asset identity mismatch"));
        }
        Ok(asset)
    }
    pub fn server_info(&self) -> AppResult<ServerInfo> {
        self.get("/api/server/about")
    }
    pub fn origin(&self) -> &Url {
        &self.origin
    }
    pub fn rating(&self, id: uuid::Uuid) -> AppResult<RatingObservation> {
        super::ratings::remote_rating(&self.asset(id)?)
    }
}
pub fn parse_photo_url(value: &str, server: &Url) -> AppResult<uuid::Uuid> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || value.contains('\\')
    {
        return Err(AppError::invalid(
            "encoded traversal or separator in photo URL",
        ));
    }
    let url = Url::parse(value).map_err(|_| AppError::invalid("invalid photo URL"))?;
    if url.origin() != server.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::invalid(
            "photo URL must use the configured origin without credentials or query",
        ));
    }
    let segments = url
        .path_segments()
        .ok_or_else(|| AppError::invalid("invalid photo URL path"))?
        .collect::<Vec<_>>();
    if segments.len() != 2 || segments[0] != "photos" {
        return Err(AppError::invalid("expected /photos/<asset UUID>"));
    }
    uuid::Uuid::parse_str(segments[1]).map_err(|_| AppError::invalid("invalid asset UUID"))
}

impl ImmichClient {
    pub fn with_rating_policy(
        mut self,
        mode: super::RatingWriteMode,
        registry: crate::qualification::CapabilityRegistry,
    ) -> Self {
        self.rating_mode = mode;
        self.rating_registry = registry;
        self
    }
    pub fn outbound_enabled(&self) -> bool {
        self.rating_mode != super::RatingWriteMode::Disabled
    }
    pub fn set_rating(&self, request: super::RatingWrite) -> AppResult<super::WriteOutcome> {
        use super::{RatingWriteMode, WriteOutcome};
        if self.rating_mode != RatingWriteMode::BestEffort {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "this profile requires explicit best_effort mode; conditional updates are not supported",
            ));
        }
        let server = self.server_info()?;
        if server.version != TESTED_VERSION {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "server upgrade invalidated rating write profile",
            ));
        }
        let decision =
            self.rating_registry
                .allows(crate::qualification::OperationKind::RatingWrite {
                    origin: &self.origin,
                    asset: request.asset,
                    version: &server.version,
                });
        if !decision.allowed {
            return Err(AppError::new(ErrorCode::Unsupported, decision.reason));
        }
        let current = self.asset(request.asset)?;
        let observed = super::ratings::remote_rating(&current)?;
        if current.checksum != request.jpeg_checksum {
            return Err(AppError::new(ErrorCode::Conflict, "JPEG identity changed"));
        }
        if observed == RatingObservation::Stars(request.desired) {
            return Ok(WriteOutcome::Confirmed);
        }
        if observed != request.expected {
            return Ok(WriteOutcome::Conflict { observed });
        }
        let desired = u8::from(request.desired);
        let body = serde_json::json!({"rating":if desired==0{None}else{Some(desired)}});
        let url = self
            .origin
            .join(&format!("/api/assets/{}", request.asset))
            .map_err(|_| AppError::invalid("invalid API path"))?;
        let response = self.http.put(url).json(&body).send();
        match response {
            Ok(response) if response.status().is_success() => {}
            Ok(response) if matches!(response.status().as_u16(), 401 | 403 | 404) => {
                return Err(AppError::new(
                    match response.status().as_u16() {
                        401 => ErrorCode::Authentication,
                        403 => ErrorCode::Permission,
                        _ => ErrorCode::NotFound,
                    },
                    "Immich denied rating write",
                ));
            }
            _ => return Ok(WriteOutcome::Uncertain),
        };
        let after = match self.asset(request.asset) {
            Ok(after) => after,
            Err(_) => return Ok(WriteOutcome::Uncertain),
        };
        let observed = super::ratings::remote_rating(&after)?;
        if after.checksum == request.jpeg_checksum
            && observed == RatingObservation::Stars(request.desired)
        {
            Ok(WriteOutcome::Confirmed)
        } else {
            Ok(WriteOutcome::Conflict { observed })
        }
    }
}
