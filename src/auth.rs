//! API-key authentication middleware. Guards write endpoints via the
//! `X-API-Key` header using a constant-time comparison.

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::error::AppError;
use crate::state::AppState;

/// Header carrying the write-auth key.
const API_KEY_HEADER: &str = "x-api-key";

/// Axum middleware that rejects requests lacking a valid `X-API-Key` header.
///
/// On success the request proceeds to the inner handler; otherwise an
/// `Unauthorized` envelope is returned.
pub async fn require_api_key(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let provided = request
        .headers()
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Audited constant-time comparison (`subtle`). `ct_eq` returns false for
    // differing lengths without an input-length-dependent loop, so the secret
    // length is not leaked via a timing oracle.
    use subtle::ConstantTimeEq;
    let matches: bool = provided
        .as_bytes()
        .ct_eq(state.api_key.as_bytes())
        .into();
    if provided.is_empty() || !matches {
        return Err(AppError::Unauthorized);
    }

    Ok(next.run(request).await)
}
