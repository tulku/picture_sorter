use crate::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
macro_rules! ids { ($($name:ident),*) => {$ (
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct $name(pub uuid::Uuid);
    impl $name { pub fn new() -> Self { Self(uuid::Uuid::new_v4()) } }
    impl Default for $name { fn default() -> Self { Self::new() } }
    impl fmt::Display for $name { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) } }
)*}; }
ids!(
    ArchiveId,
    WriterId,
    ProfileId,
    GroupId,
    JobId,
    OperationId,
    VersionId,
    SidecarId,
    CommitId,
    ConflictId,
    ReservationId,
    ApplicationSessionId,
    RatingIntentId
);
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);
impl Digest {
    pub fn parse(s: &str) -> AppResult<Self> {
        let Some(hex) = s.strip_prefix("blake3:") else {
            return Err(AppError::invalid("expected blake3 digest"));
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(AppError::invalid(
                "digest requires 64 lowercase hexadecimal characters",
            ));
        }
        Ok(Self(s.to_owned()))
    }
    pub fn of(bytes: &[u8]) -> Self {
        Self(format!("blake3:{}", blake3::hash(bytes).to_hex()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for Digest {
    type Error = AppError;
    fn try_from(s: String) -> AppResult<Self> {
        Self::parse(&s)
    }
}
impl From<Digest> for String {
    fn from(d: Digest) -> Self {
        d.0
    }
}
impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ArchivePath(String);
impl ArchivePath {
    pub fn parse(s: &str) -> AppResult<Self> {
        if s.is_empty()
            || s.contains(['\\', '\0'])
            || s.split('/')
                .any(|c| c.is_empty() || c == "." || c == ".." || c.contains(':'))
        {
            return Err(AppError::invalid("expected confined relative UTF-8 path"));
        }
        Ok(Self(s.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ArchivePath {
    type Error = AppError;
    fn try_from(s: String) -> AppResult<Self> {
        Self::parse(&s)
    }
}
impl From<ArchivePath> for String {
    fn from(p: ArchivePath) -> Self {
        p.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteCount(pub u64);
impl ByteCount {
    pub fn checked_add(self, other: Self) -> AppResult<Self> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or_else(|| AppError::invalid("byte count overflow"))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipRevision(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Rating(u8);
impl TryFrom<u8> for Rating {
    type Error = AppError;
    fn try_from(n: u8) -> AppResult<Self> {
        if n > 5 {
            Err(AppError::invalid("rating outside 0..5"))
        } else {
            Ok(Self(n))
        }
    }
}
impl From<Rating> for u8 {
    fn from(r: Rating) -> Self {
        r.0
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RatingObservation {
    Unobserved,
    Stars(Rating),
    Rejected,
    Unsupported(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaRole {
    Raw,
    Jpeg,
    OtherStill,
    Video,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginalMember {
    pub digest: Digest,
    pub size: u64,
    pub path: ArchivePath,
    pub role: MediaRole,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: GroupId,
    pub membership_revision: MembershipRevision,
    pub members: Vec<OriginalMember>,
    pub primary_raw: Option<ArchivePath>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VersionBody {
    Content { digest: Digest, size: u64 },
    Reset,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VersionOrigin {
    Observed,
    Derived,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub id: VersionId,
    pub sidecar: SidecarId,
    pub parents: Vec<VersionId>,
    pub body: VersionBody,
    pub origin: VersionOrigin,
}
pub type Timestamp = u64;
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
    fn elapsed(&self) -> Duration;
}
pub struct SystemClock {
    start: Instant,
}
impl Default for SystemClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}
impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}
#[derive(Clone, Default)]
pub struct ManualClock {
    wall: Arc<AtomicU64>,
    ticks: Arc<AtomicU64>,
}
impl ManualClock {
    pub fn advance(&self, seconds: u64) {
        self.wall.fetch_add(seconds, Ordering::SeqCst);
        self.ticks.fetch_add(seconds, Ordering::SeqCst);
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        self.wall.load(Ordering::SeqCst)
    }
    fn elapsed(&self) -> Duration {
        Duration::from_secs(self.ticks.load(Ordering::SeqCst))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FaultPoint {
    IntentSaved,
    BeforeProjection,
    BeforeUnlink,
    StagePartial,
    Verified,
    OriginalPublished,
    HistoryPublished,
    CommitPublished,
    ProjectionPublished,
    LocalAck,
    UnlinkMember,
    ApiSent,
    ApiReceived,
}
pub trait FaultInjector: Send + Sync {
    fn hit(&self, point: FaultPoint) -> AppResult<()>;
}
pub struct NoFaults;
impl FaultInjector for NoFaults {
    fn hit(&self, _: FaultPoint) -> AppResult<()> {
        Ok(())
    }
}
impl MediaRole {
    pub fn for_filename(path: &str) -> Option<Self> {
        match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
            "cr2" | "nef" | "arw" | "dng" | "raw" | "orf" | "ori" => Some(Self::Raw),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "tif" | "tiff" | "png" => Some(Self::OtherStill),
            "mov" | "mp4" | "avi" | "mts" | "m4v" | "mpg" | "mpeg" => Some(Self::Video),
            _ => None,
        }
    }
}
impl OriginalMember {
    pub fn validate(&self) -> AppResult<()> {
        if MediaRole::for_filename(self.path.as_str()) != Some(self.role) {
            return Err(AppError::integrity(
                "original media role does not match supported original filename",
            ));
        }
        Ok(())
    }
}
pub fn validate_sidecar_path(path: &ArchivePath) -> AppResult<()> {
    if MediaRole::for_filename(path.as_str()).is_some() {
        return Err(AppError::integrity(
            "mutable sidecar cannot claim an original media path",
        ));
    }
    if !matches!(
        path.as_str()
            .rsplit('.')
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("rrdata" | "rrexif" | "xmp" | "orig" | "pp3" | "dop" | "on1")
    ) {
        return Err(AppError::integrity(
            "unrecognized companion path cannot be a mutable projection",
        ));
    }
    Ok(())
}
