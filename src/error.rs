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
