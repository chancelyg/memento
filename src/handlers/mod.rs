//! HTTP handler modules.

pub mod diary;
pub mod favorites;
pub mod images;
pub mod settings;

use axum::{response::IntoResponse, Json};
use serde_json::json;

use crate::error::ApiResponse;

/// `GET /api/health` — liveness probe.
pub async fn health() -> impl IntoResponse {
    Json(ApiResponse::ok(json!({ "status": "ok" })))
}

/// `GET /api/auth/verify` — confirm the supplied `X-API-Key` is valid.
///
/// This route lives behind the API-key middleware, so reaching the handler at
/// all means the key was correct; a wrong/missing key is rejected upstream with
/// `401`. Lets a client (bot / agent) check its key before attempting writes.
pub async fn verify_key() -> impl IntoResponse {
    Json(ApiResponse::ok(json!({ "valid": true })))
}
