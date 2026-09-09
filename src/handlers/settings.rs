//! Browser-session handlers for typed public site settings.

use axum::{
    extract::{DefaultBodyLimit, FromRequest, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{Map, Value};

use crate::{
    error::{error_envelope, ApiResponse, AppResult},
    settings::SiteSettings,
    state::AppState,
};

const SETTINGS_BODY_BYTES: usize = 512 * 1024;

async fn site_payload(mut request: Request) -> Result<SiteSettings, Box<Response>> {
    DefaultBodyLimit::max(SETTINGS_BODY_BYTES).apply(&mut request);
    let Json(payload) = Json::<Map<String, Value>>::from_request(request, &())
        .await
        .map_err(|error| {
            let (status, message) = match error.status() {
                StatusCode::UNSUPPORTED_MEDIA_TYPE => (
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "content-type must be application/json",
                ),
                StatusCode::PAYLOAD_TOO_LARGE => (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body exceeds 512 KiB",
                ),
                _ => (StatusCode::BAD_REQUEST, "invalid site settings JSON body"),
            };
            Box::new((status, Json(error_envelope(message))).into_response())
        })?;
    if payload.len() != 3
        || !payload.contains_key("name")
        || !payload.contains_key("slogan")
        || !payload.contains_key("icon")
    {
        return Err(Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(error_envelope(
                    "site settings require exactly name, slogan and icon",
                )),
            )
                .into_response(),
        ));
    }
    serde_json::from_value::<SiteSettings>(Value::Object(payload)).map_err(|_| {
        Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(error_envelope("invalid site settings JSON body")),
            )
                .into_response(),
        )
    })
}

pub async fn get_site(State(state): State<AppState>) -> AppResult<Response> {
    let site = state.settings.snapshot().site;
    Ok(Json(ApiResponse::ok(site)).into_response())
}

pub async fn put_site(State(state): State<AppState>, request: Request) -> AppResult<Response> {
    let site = match site_payload(request).await {
        Ok(site) => site,
        Err(response) => return Ok(*response),
    };
    let settings = state.settings.replace_site(site).await?;
    Ok(Json(ApiResponse::ok(settings.site)).into_response())
}
