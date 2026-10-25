use serde::{Deserialize, Serialize};
use std::fmt;
pub type AppResult<T> = Result<T, AppError>;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    InvalidConfig,
    Unsupported,
    Unavailable,
    Capacity,
    Conflict,
    Integrity,
    SourceChanged,
    Cancelled,
    Io,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetryClass {
    Never,
    Correction,
    Transient,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub retry: RetryClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errno: Option<i32>,
}
impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry: RetryClass::Correction,
            errno: None,
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidConfig, message)
    }
    pub fn integrity(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Integrity, message)
    }
}
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}
impl std::error::Error for AppError {}
impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self {
            code: ErrorCode::Io,
            message: e.to_string(),
            retry: RetryClass::Transient,
            errno: e.raw_os_error(),
        }
    }
}
impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        Self::integrity(e.to_string())
    }
}
impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        Self::new(ErrorCode::Io, e.to_string())
    }
}
