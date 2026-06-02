//! HTTP handlers for reading and replacing poster images.

use axum::{
    extract::{FromRequest, Multipart, Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use crate::error::{ApiResponse, AppError, AppResult};
use crate::image::{detect_image_mime, validate_size, DecodedImage, MAX_IMAGE_BYTES};
use crate::models::FavoriteDto;
use crate::repo;
use crate::state::AppState;

/// Cache lifetime for served images (1 day).
const IMAGE_CACHE_CONTROL: &str = "public, max-age=86400";

/// `GET /api/favorites/{id}/image` — raw bytes (public).
pub async fn get_image(State(state): State<AppState>, Path(id): Path<i64>) -> AppResult<Response> {
    let pool = state.pool.clone();
    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get().map_err(AppError::from)?;
        repo::get_image(&conn, id)
    })
    .await??;

    match result {
        Some((bytes, mime)) => Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, IMAGE_CACHE_CONTROL.to_string()),
            ],
            bytes,
        )
            .into_response()),
        None => Err(AppError::NotFound),
    }
}

/// `POST /api/favorites/{id}/image` (auth) — replace image.
///
/// Accepts either `multipart/form-data` with an `image` field, or a raw body
/// whose `Content-Type` header gives the mime.
pub async fn put_image(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    request: axum::extract::Request,
) -> AppResult<Response> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let decoded = if content_type.starts_with("multipart/form-data") {
        read_multipart_image(request).await?
    } else {
        read_raw_body_image(request, &content_type).await?
    };

    let pool = state.pool.clone();
    let updated = tokio::task::spawn_blocking(move || {
        let conn = pool.get().map_err(AppError::from)?;
        repo::set_image(&conn, id, &decoded)
    })
    .await??;

    if !updated {
        return Err(AppError::NotFound);
    }

    let pool = state.pool.clone();
    let row = tokio::task::spawn_blocking(move || {
        let conn = pool.get().map_err(AppError::from)?;
        repo::get(&conn, id)
    })
    .await??;

    let dto = row
        .as_ref()
        .map(FavoriteDto::from)
        .ok_or(AppError::NotFound)?;
    Ok((StatusCode::OK, Json(ApiResponse::ok(dto))).into_response())
}

/// Extract the `image` field from a multipart body.
async fn read_multipart_image(request: axum::extract::Request) -> AppResult<DecodedImage> {
    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|e| AppError::BadRequest(format!("invalid multipart body: {e}")))?;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("invalid multipart field: {e}")))?
    {
        if field.name() == Some("image") {
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::BadRequest(format!("failed reading image field: {e}")))?
                .to_vec();
            validate_size(bytes.len())?;
            if bytes.is_empty() {
                return Err(AppError::BadRequest("uploaded image is empty".into()));
            }
            // Trust the bytes, not the declared part Content-Type.
            let mime = detect_image_mime(&bytes)?;
            return Ok(DecodedImage { bytes, mime });
        }
    }

    Err(AppError::BadRequest(
        "multipart body missing 'image' field".into(),
    ))
}

/// Read a raw request body as an image, using the supplied content type.
async fn read_raw_body_image(
    request: axum::extract::Request,
    content_type: &str,
) -> AppResult<DecodedImage> {
    let declared = if content_type.is_empty() {
        ""
    } else {
        content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim()
    };

    if !declared.is_empty() && !declared.starts_with("image/") {
        return Err(AppError::BadRequest(
            "Content-Type must be an image/* type or multipart/form-data".into(),
        ));
    }

    let body = request.into_body();
    let bytes = axum::body::to_bytes(body, MAX_IMAGE_BYTES)
        .await
        .map_err(|_| {
            AppError::BadRequest(format!(
                "request body too large (max {MAX_IMAGE_BYTES} bytes)"
            ))
        })?
        .to_vec();

    validate_size(bytes.len())?;
    if bytes.is_empty() {
        return Err(AppError::BadRequest("request body is empty".into()));
    }

    // Authoritative mime comes from the actual bytes, not the header.
    let mime = detect_image_mime(&bytes)?;
    Ok(DecodedImage { bytes, mime })
}
