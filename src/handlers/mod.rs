//! HTTP handler modules.

pub mod favorites;
pub mod images;

use axum::{response::IntoResponse, Json};
use serde_json::json;

use crate::error::ApiResponse;

/// `GET /api/health` — liveness probe.
pub async fn health() -> impl IntoResponse {
    Json(ApiResponse::ok(json!({ "status": "ok" })))
}
