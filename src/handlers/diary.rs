//! Shared diary handlers for API-key and browser-session route groups.

use axum::{
    extract::{
        rejection::{PathRejection, QueryRejection},
        DefaultBodyLimit, FromRequest, Path, Query, Request, State,
    },
    http::{
        header::{ETAG, IF_MATCH},
        HeaderMap, HeaderValue, StatusCode,
    },
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{Map, Value};

use crate::{
    diary::{self, ContentPayload, DiaryDto, ListParams, ListQuery, PreparedContent},
    error::{error_envelope, ApiResponse, AppError, AppResult},
    state::AppState,
};

async fn content(mut request: Request) -> Result<PreparedContent, Box<Response>> {
    DefaultBodyLimit::max(64 * 1024).apply(&mut request);
    let Json(payload) = Json::<Map<String, Value>>::from_request(request, &())
        .await
        .map_err(|error| {
            // Do not display or log extractor errors: they can contain private input.
            let (status, message) = match error.status() {
                StatusCode::UNSUPPORTED_MEDIA_TYPE => (
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "content-type must be application/json",
                ),
                StatusCode::PAYLOAD_TOO_LARGE => {
                    (StatusCode::PAYLOAD_TOO_LARGE, "request body exceeds 64 KiB")
                }
                _ => (StatusCode::BAD_REQUEST, "invalid diary JSON body"),
            };
            Box::new((status, Json(error_envelope(message))).into_response())
        })?;
    let payload =
        serde_json::from_value::<ContentPayload>(Value::Object(payload)).map_err(|_| {
            Box::new(
                (
                    StatusCode::BAD_REQUEST,
                    Json(error_envelope("invalid diary JSON body")),
                )
                    .into_response(),
            )
        })?;
    PreparedContent::from_payload(payload).map_err(|error| Box::new(error.into_response()))
}

fn path(path: Result<Path<i64>, PathRejection>) -> AppResult<i64> {
    path.map(|Path(id)| id)
        .map_err(|_| AppError::BadRequest("invalid diary id".into()))
}

fn version(headers: &HeaderMap) -> AppResult<Option<i64>> {
    let mut values = headers.get_all(IF_MATCH).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    let invalid =
        || AppError::BadRequest("If-Match must contain one quoted positive version".into());
    if values.next().is_some() {
        return Err(invalid());
    }
    let digits = value
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix('"'))
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(invalid)?;
    if digits.is_empty()
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    digits.parse::<i64>().map(Some).map_err(|_| invalid())
}

fn dto_response(status: StatusCode, dto: DiaryDto) -> Response {
    let etag = HeaderValue::from_str(&format!("\"{}\"", dto.version))
        .expect("an integer version is a valid header value");
    (status, [(ETAG, etag)], Json(ApiResponse::ok(dto))).into_response()
}

pub async fn list(
    State(state): State<AppState>,
    query: Result<Query<ListParams>, QueryRejection>,
) -> AppResult<Response> {
    let Query(params) = query.map_err(|_| AppError::BadRequest("invalid diary query".into()))?;
    let query = ListQuery::from_params(params)?;
    let pool = state.pool;
    let page = tokio::task::spawn_blocking(move || {
        let mut conn = pool.get()?;
        diary::list(&mut conn, &query)
    })
    .await??;
    Ok(Json(ApiResponse::ok(page)).into_response())
}

pub async fn get(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> AppResult<Response> {
    let id = path(id)?;
    let pool = state.pool;
    let dto = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        diary::get(&conn, id)
    })
    .await??;
    Ok(dto_response(StatusCode::OK, dto))
}

pub async fn create(State(state): State<AppState>, request: Request) -> AppResult<Response> {
    let content = match content(request).await {
        Ok(content) => content,
        Err(response) => return Ok(*response),
    };
    let pool = state.pool;
    let timezone = state.diary_timezone;
    let dto = tokio::task::spawn_blocking(move || {
        let mut conn = pool.get()?;
        diary::create(&mut conn, &content, timezone)
    })
    .await??;
    Ok(dto_response(StatusCode::CREATED, dto))
}

pub async fn update(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    request: Request,
) -> AppResult<Response> {
    let id = path(id)?;
    let expected = version(request.headers())?;
    let content = match content(request).await {
        Ok(content) => content,
        Err(response) => return Ok(*response),
    };
    let pool = state.pool;
    let dto = tokio::task::spawn_blocking(move || {
        let mut conn = pool.get()?;
        diary::update(&mut conn, id, expected, &content)
    })
    .await??;
    Ok(dto_response(StatusCode::OK, dto))
}

pub async fn delete(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let id = path(id)?;
    let expected = version(&headers)?;
    let pool = state.pool;
    tokio::task::spawn_blocking(move || {
        let mut conn = pool.get()?;
        diary::delete(&mut conn, id, expected)
    })
    .await??;
    Ok(StatusCode::NO_CONTENT.into_response())
}
