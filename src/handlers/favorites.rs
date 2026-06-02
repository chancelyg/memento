//! HTTP handlers for favorites CRUD (list/get/create/update/delete).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::{Map, Value};

use crate::error::{ApiResponse, AppError, AppResult};
use crate::image::{decode_base64_image, fetch_image_blocking, DecodedImage};
use crate::models::{FavoriteDto, ItemType, PreparedCreate, PreparedUpdate, RawPayload};
use crate::repo::{self, ListQuery};
use crate::state::AppState;

/// Default page size.
const DEFAULT_PER_PAGE: u32 = 24;
/// Maximum page size.
const MAX_PER_PAGE: u32 = 100;
/// Maximum length of the `q` search string (chars).
const MAX_Q_LEN: usize = 256;

/// Raw query string params for the list endpoint.
#[derive(Debug, Deserialize)]
pub struct ListParams {
    #[serde(rename = "type")]
    pub item_type: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub q: Option<String>,
}

/// Paginated list payload.
#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub items: Vec<FavoriteDto>,
    pub page: u32,
    pub per_page: u32,
    pub total: i64,
}

/// `GET /api/favorites`
pub async fn list_favorites(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> AppResult<Response> {
    let item_type = ItemType::parse_filter(params.item_type.as_deref())?;
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(DEFAULT_PER_PAGE);
    if !(1..=MAX_PER_PAGE).contains(&per_page) {
        return Err(AppError::BadRequest(format!(
            "per_page must be between 1 and {MAX_PER_PAGE}"
        )));
    }
    let q = params
        .q
        .map(|s| s.trim().chars().take(MAX_Q_LEN).collect::<String>())
        .filter(|s| !s.is_empty());

    let query = ListQuery {
        item_type,
        page,
        per_page,
        q,
    };

    let pool = state.pool.clone();
    let (rows, total) = tokio::task::spawn_blocking(move || {
        let conn = pool.get().map_err(AppError::from)?;
        repo::list(&conn, &query)
    })
    .await??;

    let items = rows.iter().map(FavoriteDto::from).collect();
    let payload = ListResponse {
        items,
        page,
        per_page,
        total,
    };
    Ok((StatusCode::OK, Json(ApiResponse::ok(payload))).into_response())
}

/// `GET /api/favorites/{id}`
pub async fn get_favorite(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let pool = state.pool.clone();
    let row = tokio::task::spawn_blocking(move || {
        let conn = pool.get().map_err(AppError::from)?;
        repo::get(&conn, id)
    })
    .await??;

    match row {
        Some(f) => {
            let dto = FavoriteDto::from(&f);
            Ok((StatusCode::OK, Json(ApiResponse::ok(dto))).into_response())
        }
        None => Err(AppError::NotFound),
    }
}

/// Resolve an at-most-one image source into decoded bytes (off the async
/// runtime for the blocking fetch path).
async fn resolve_image(
    image_base64: Option<String>,
    image_url: Option<String>,
) -> AppResult<Option<DecodedImage>> {
    if let Some(b64) = image_base64 {
        let decoded = tokio::task::spawn_blocking(move || decode_base64_image(&b64)).await??;
        return Ok(Some(decoded));
    }
    if let Some(url) = image_url {
        let decoded = tokio::task::spawn_blocking(move || fetch_image_blocking(&url)).await??;
        return Ok(Some(decoded));
    }
    Ok(None)
}

/// `POST /api/favorites` (auth)
pub async fn create_favorite(
    State(state): State<AppState>,
    Json(RawPayload(body)): Json<RawPayload>,
) -> AppResult<Response> {
    let prepared = PreparedCreate::from_body(&body)?;
    let image = resolve_image(prepared.image_base64.clone(), prepared.image_url.clone()).await?;

    let pool = state.pool.clone();
    let new_id = tokio::task::spawn_blocking(move || {
        let conn = pool.get().map_err(AppError::from)?;
        repo::create(&conn, &prepared, image.as_ref())
    })
    .await??;

    let dto = fetch_dto(&state, new_id).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::ok(dto))).into_response())
}

/// `PUT /api/favorites/{id}` (auth)
pub async fn update_favorite(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(RawPayload(body)): Json<RawPayload>,
) -> AppResult<Response> {
    let prepared = PreparedUpdate::from_body(&body)?;
    if prepared.is_empty() {
        return Err(AppError::BadRequest("no updatable fields supplied".into()));
    }

    let image = resolve_image(prepared.image_base64.clone(), prepared.image_url.clone()).await?;

    let pool = state.pool.clone();
    let updated = tokio::task::spawn_blocking(move || {
        let mut conn = pool.get().map_err(AppError::from)?;
        repo::update(&mut conn, id, &prepared, image.as_ref())
    })
    .await??;

    if !updated {
        return Err(AppError::NotFound);
    }

    let dto = fetch_dto(&state, id).await?;
    Ok((StatusCode::OK, Json(ApiResponse::ok(dto))).into_response())
}

/// `DELETE /api/favorites/{id}` (auth)
pub async fn delete_favorite(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let pool = state.pool.clone();
    let deleted = tokio::task::spawn_blocking(move || {
        let conn = pool.get().map_err(AppError::from)?;
        repo::delete(&conn, id)
    })
    .await??;

    if deleted {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Err(AppError::NotFound)
    }
}

/// Load a favorite by id and convert to DTO, mapping a missing row to
/// `Internal` (callers only invoke this immediately after a write).
async fn fetch_dto(state: &AppState, id: i64) -> AppResult<FavoriteDto> {
    let pool = state.pool.clone();
    let row = tokio::task::spawn_blocking(move || {
        let conn = pool.get().map_err(AppError::from)?;
        repo::get(&conn, id)
    })
    .await??;

    row.as_ref().map(FavoriteDto::from).ok_or_else(|| {
        AppError::Internal(Box::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "row vanished after write",
        )))
    })
}

/// Helper exposed for tests / introspection: validate a body without writing.
#[cfg(test)]
pub fn validate_create(body: &Map<String, Value>) -> AppResult<PreparedCreate> {
    PreparedCreate::from_body(body)
}
