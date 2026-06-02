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

    // Constant-time comparison (`subtle`). For equal-length inputs `ct_eq`
    // compares all bytes in constant time, revealing no per-byte timing
    // signal; on a length mismatch it short-circuits and returns false. Since
    // the configured key is a fixed-length hex string, the length is not
    // secret and no useful timing oracle leaks.
    use subtle::ConstantTimeEq;
    let matches: bool = provided.as_bytes().ct_eq(state.api_key.as_bytes()).into();
    if provided.is_empty() || !matches {
        return Err(AppError::Unauthorized);
    }

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::{
        body::Body,
        http::{Request as HttpRequest, StatusCode},
        middleware::from_fn_with_state,
        routing::get,
        Router,
    };
    use tower::ServiceExt; // for `oneshot`

    use crate::db::build_pool;

    /// Build a minimal router guarded by `require_api_key` with key "secret".
    fn guarded_router() -> Router {
        // In-memory SQLite pool keeps the test self-contained and fast.
        let pool = build_pool(":memory:").expect("build in-memory pool");
        let state = AppState::new(pool, "secret".into());

        Router::new()
            .route("/t", get(|| async { "ok" }))
            .layer(from_fn_with_state(state, require_api_key))
    }

    /// Issue a GET /t request with an optional `X-API-Key` header value.
    async fn request_with_key(key: Option<&str>) -> StatusCode {
        let mut builder = HttpRequest::builder().uri("/t");
        if let Some(value) = key {
            builder = builder.header(API_KEY_HEADER, value);
        }
        let request = builder.body(Body::empty()).expect("build request");

        guarded_router()
            .oneshot(request)
            .await
            .expect("router oneshot")
            .status()
    }

    #[tokio::test]
    async fn missing_api_key_is_unauthorized() {
        assert_eq!(request_with_key(None).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn wrong_api_key_is_unauthorized() {
        assert_eq!(
            request_with_key(Some("wrong")).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn correct_api_key_passes_through() {
        assert_eq!(request_with_key(Some("secret")).await, StatusCode::OK);
    }
}
