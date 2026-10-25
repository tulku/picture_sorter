use crate::{
    domain::*,
    error::{AppError, AppResult, ErrorCode},
};
use serde_json::Value;
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PickFlag {
    Neutral,
    Pick,
    Reject,
    Unsupported,
}
impl PickFlag {
    pub fn favorite(self) -> Option<bool> {
        match self {
            Self::Pick => Some(true),
            Self::Neutral | Self::Reject => Some(false),
            Self::Unsupported => None,
        }
    }
}
#[derive(Debug, Clone)]
pub struct EditDocument {
    root: Value,
    pub rating: RatingObservation,
    pub flag: PickFlag,
    source: Vec<u8>,
}
impl EditDocument {
    pub fn value(&self) -> &Value {
        &self.root
    }
    pub fn source_bytes(&self) -> &[u8] {
        &self.source
    }
    pub fn non_flag(&self) -> Value {
        let mut value = self.root.clone();
        value.as_object_mut().unwrap().remove("flag");
        value
    }
    pub fn non_rating(&self) -> Value {
        let mut value = self.root.clone();
        if let Some(object) = value.as_object_mut() {
            object.remove("rating");
        }
        value
    }
}
pub struct RapidRawAdapter {
    pub source_revision: &'static str,
}
impl RapidRawAdapter {
    pub fn for_version(version: &str) -> AppResult<Self> {
        let source_revision = match version {
            "1.6.6" => "52138689a9c9aea400e37a30c902154b47db3799",
            "872804e8a21d4dff707f31fc9dcf5d3128ca663b" => {
                "872804e8a21d4dff707f31fc9dcf5d3128ca663b"
            }
            _ => {
                return Err(AppError::new(
                    ErrorCode::Unsupported,
                    "RapidRAW version has no source-backed transformation profile",
                ));
            }
        };
        Ok(Self { source_revision })
    }
    pub fn inspect(&self, bytes: &[u8]) -> AppResult<EditDocument> {
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "oversized editor data remains opaque",
            ));
        }
        let root: Value = serde_json::from_slice(bytes)?;
        if !root.is_object()
            || root.get("version").and_then(Value::as_u64) != Some(1)
            || root.get("adjustments").is_none()
        {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "unsupported RapidRAW metadata schema",
            ));
        }
        let n = root.get("rating").and_then(Value::as_u64).ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported,
                "missing or non-integral native rating",
            )
        })?;
        let flag = match root.get("flag") {
            None | Some(Value::Null) => PickFlag::Neutral,
            Some(Value::String(s)) if s == "pick" => PickFlag::Pick,
            Some(Value::String(s)) if s == "reject" => PickFlag::Reject,
            _ => PickFlag::Unsupported,
        };
        let rating = if n > 5 {
            RatingObservation::Unsupported("native rating outside 0..5".into())
        } else {
            match root.get("flag").and_then(Value::as_str) {
                Some("reject") => RatingObservation::Rejected,
                Some("pick") | None => RatingObservation::Stars(Rating::try_from(n as u8)?),
                Some(_) => RatingObservation::Unsupported("unknown native flag".into()),
            }
        };
        Ok(EditDocument {
            root,
            rating,
            flag,
            source: bytes.to_vec(),
        })
    }
    pub fn with_favorite(&self, document: &EditDocument, favorite: bool) -> AppResult<Vec<u8>> {
        if document.flag == PickFlag::Unsupported {
            return Err(AppError::new(ErrorCode::Unsupported, "unknown native flag"));
        }
        if document.flag == PickFlag::Reject {
            return Ok(document.source.clone());
        }
        let mut value = document.root.clone();
        value["flag"] = if favorite {
            Value::String("pick".into())
        } else {
            Value::Null
        };
        Ok(serde_json::to_vec(&value)?)
    }
    pub fn with_rating(&self, document: &EditDocument, rating: Rating) -> AppResult<Vec<u8>> {
        if matches!(
            document.rating,
            RatingObservation::Rejected | RatingObservation::Unsupported(_)
        ) {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "rejected or unsupported native observations block star propagation",
            ));
        }
        let mut value = document.root.clone();
        value["rating"] = serde_json::json!(u8::from(rating));
        Ok(serde_json::to_vec(&value)?)
    }
    pub fn initial_document(&self, rating: Rating) -> AppResult<Vec<u8>> {
        Ok(serde_json::to_vec(
            &serde_json::json!({"version":1,"rating":u8::from(rating),"adjustments":null,"tags":null}),
        )?)
    }
    pub fn is_rating_authority(&self, primary: &ArchivePath, sidecar: &ArchivePath) -> bool {
        sidecar.as_str() == format!("{}.rrdata", primary.as_str())
    }
    pub fn merge_rating_adjustments(
        &self,
        base: &EditDocument,
        local: &EditDocument,
        remote: &EditDocument,
        rating: Rating,
    ) -> AppResult<Vec<u8>> {
        let selected = if local.non_rating() == remote.non_rating()
            || remote.non_rating() == base.non_rating()
        {
            local
        } else if local.non_rating() == base.non_rating() {
            remote
        } else {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "divergent non-rating metadata requires explicit resolution",
            ));
        };
        self.with_rating(selected, rating)
    }
}
