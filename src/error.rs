//! Application error type and the JSON response envelope.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

/// Unified JSON response envelope used by every API endpoint.
#[derive(Debug, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    /// Construct a successful envelope carrying `data`.
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }
}

/// Helper to build an error envelope with no data payload.
pub fn error_envelope(message: &str) -> ApiResponse<()> {
    ApiResponse {
        success: false,
        data: None,
        error: Some(message.to_string()),
    }
}

/// Central application error. Each variant maps to an HTTP status and a
/// user-friendly message; internal detail is logged, never leaked.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Client sent invalid input (bad type, empty name, bad base64, etc.).
    #[error("{0}")]
    BadRequest(String),

    /// Authentication failed (missing or wrong API key).
    #[error("invalid or missing API key")]
    Unauthorized,

    /// Requested resource does not exist.
    #[error("not found")]
    NotFound,

    #[error("diary version has changed; reload before retrying")]
    PreconditionFailed,

    #[error("{0}")]
    Conflict(String),

    /// Database / connection-pool failure.
    #[error("database error")]
    Database(#[source] anyhow_like::BoxError),

    /// Failure fetching a remote image.
    #[error("failed to fetch remote image")]
    ImageFetch(#[source] anyhow_like::BoxError),

    /// Any other unexpected internal failure.
    #[error("internal error")]
    Internal(#[source] anyhow_like::BoxError),
}

/// Minimal boxed-error alias module to avoid an extra dependency.
pub mod anyhow_like {
    /// A boxed, thread-safe error.
    pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
}

impl AppError {
    /// HTTP status code corresponding to this error.
    fn status(&self) -> StatusCode {
        match self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::PreconditionFailed => StatusCode::PRECONDITION_FAILED,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::Database(_) | AppError::ImageFetch(_) | AppError::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    /// User-facing message (never leaks internal detail).
    fn public_message(&self) -> String {
        match self {
            AppError::BadRequest(msg) => msg.clone(),
            AppError::Unauthorized => "invalid or missing API key".to_string(),
            AppError::NotFound => "not found".to_string(),
            AppError::PreconditionFailed => {
                "diary version has changed; reload before retrying".into()
            }
            AppError::Conflict(message) => message.clone(),
            AppError::Database(_) => "a database error occurred".to_string(),
            AppError::ImageFetch(_) => "failed to fetch the remote image".to_string(),
            AppError::Internal(_) => "an internal error occurred".to_string(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // Log full internal detail server-side.
        match &self {
            AppError::Database(e) => tracing::error!(error = %e, "database error"),
            AppError::ImageFetch(e) => tracing::warn!(error = %e, "image fetch error"),
            AppError::Internal(e) => tracing::error!(error = %e, "internal error"),
            AppError::BadRequest(msg) => tracing::debug!(message = %msg, "bad request"),
            AppError::Unauthorized => tracing::debug!("unauthorized request"),
            AppError::NotFound => tracing::debug!("resource not found"),
            AppError::PreconditionFailed | AppError::Conflict(_) => {}
        }

        let status = self.status();
        let body = Json(error_envelope(&self.public_message()));
        (status, body).into_response()
    }
}

/// Convenience: convert r2d2 pool errors.
impl From<r2d2::Error> for AppError {
    fn from(e: r2d2::Error) -> Self {
        AppError::Database(Box::new(e))
    }
}

/// Convenience: convert rusqlite errors.
impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Database(Box::new(e))
    }
}

/// Convenience: convert serde_json errors (treated as bad request when parsing
/// input, internal when serializing output — default to internal here).
impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Internal(Box::new(e))
    }
}

/// Convenience: convert join errors from spawned blocking tasks.
impl From<tokio::task::JoinError> for AppError {
    fn from(e: tokio::task::JoinError) -> Self {
        AppError::Internal(Box::new(e))
    }
}

/// Result alias used throughout the crate.
pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    fn boxed() -> anyhow_like::BoxError {
        Box::new(std::io::Error::other("boom"))
    }

    #[test]
    fn status_codes_map_correctly() {
        assert_eq!(
            AppError::BadRequest("x".into()).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(AppError::Unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(AppError::NotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            AppError::Database(boxed()).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            AppError::ImageFetch(boxed()).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            AppError::Internal(boxed()).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn public_message_never_leaks_internal_detail() {
        // BadRequest passes the message through verbatim.
        assert_eq!(
            AppError::BadRequest("bad name".into()).public_message(),
            "bad name"
        );
        // Internal/database/image errors yield generic text, not the source.
        assert_eq!(
            AppError::Unauthorized.public_message(),
            "invalid or missing API key"
        );
        assert_eq!(AppError::NotFound.public_message(), "not found");
        assert_eq!(
            AppError::Database(boxed()).public_message(),
            "a database error occurred"
        );
        assert_eq!(
            AppError::ImageFetch(boxed()).public_message(),
            "failed to fetch the remote image"
        );
        assert_eq!(
            AppError::Internal(boxed()).public_message(),
            "an internal error occurred"
        );
        // The internal "boom" detail must not appear anywhere user-facing.
        assert!(!AppError::Database(boxed())
            .public_message()
            .contains("boom"));
    }

    #[tokio::test]
    async fn into_response_sets_status_and_envelope() {
        let resp = AppError::BadRequest("nope".into()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["success"], serde_json::json!(false));
        assert_eq!(v["data"], serde_json::Value::Null);
        assert_eq!(v["error"], serde_json::json!("nope"));
    }

    #[tokio::test]
    async fn into_response_logs_each_variant_without_panicking() {
        // Drive every match arm in IntoResponse / logging.
        for err in [
            AppError::Unauthorized,
            AppError::NotFound,
            AppError::Database(boxed()),
            AppError::ImageFetch(boxed()),
            AppError::Internal(boxed()),
        ] {
            let _ = err.into_response();
        }
    }

    #[test]
    fn from_conversions_classify_sources() {
        let sqlite_err: AppError = rusqlite::Error::QueryReturnedNoRows.into();
        assert!(matches!(sqlite_err, AppError::Database(_)));

        let json_err: AppError = serde_json::from_str::<i32>("not json").unwrap_err().into();
        assert!(matches!(json_err, AppError::Internal(_)));
    }

    #[test]
    fn ok_envelope_carries_data() {
        let env = ApiResponse::ok(42);
        assert!(env.success);
        assert_eq!(env.data, Some(42));
        assert!(env.error.is_none());
    }

    #[test]
    fn error_envelope_helper_has_no_data() {
        let env = error_envelope("oops");
        assert!(!env.success);
        assert!(env.data.is_none());
        assert_eq!(env.error.as_deref(), Some("oops"));
    }
}
