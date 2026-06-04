//! Integration tests covering the full HTTP API surface of `memento`.
//!
//! Each test builds a fresh router backed by its own temporary SQLite DB so the
//! tests are fully isolated and can run in parallel. Requests are driven through
//! the router with `tower::ServiceExt::oneshot`; response bodies are collected
//! with `http_body_util::BodyExt`.

use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// A valid 1x1 PNG (base64). Decodes to a real PNG that passes magic-byte sniffing.
const PNG_1X1_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M8AAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

/// Build a fresh router with its own temporary DB file. Returns the router and
/// the `NamedTempFile` guard (kept alive for the duration of the test).
fn fresh_app() -> (Router, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().expect("create temp db file");
    let path = tmp.path().to_str().expect("temp path is valid utf-8");
    let pool = memento::db::build_pool(path).unwrap();
    memento::db::init_schema(&pool).unwrap();
    let state = memento::state::AppState::new(pool, "testkey".to_string());
    let app = memento::build_router(state);
    (app, tmp)
}

/// Send a request through the router, returning the status and the collected
/// response body bytes.
async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec();
    (status, bytes)
}

/// Send a request and additionally capture the `Content-Type` header.
async fn send_with_content_type(
    app: &Router,
    req: Request<Body>,
) -> (StatusCode, Option<String>, Vec<u8>) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec();
    (status, content_type, bytes)
}

/// Parse response bytes as a JSON envelope value.
fn parse_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("response body is valid JSON")
}

/// Build the standard create body for a game named "X" with the 1x1 PNG.
fn create_body_game_x() -> String {
    json!({
        "type": "game",
        "name": "X",
        "image_base64": format!("data:image/png;base64,{PNG_1X1_B64}"),
    })
    .to_string()
}

/// Helper: create one favorite via the API and return its parsed `id`.
async fn create_favorite_x(app: &Router) -> i64 {
    let req = Request::builder()
        .method("POST")
        .uri("/api/favorites")
        .header("content-type", "application/json")
        .header("x-api-key", "testkey")
        .body(Body::from(create_body_game_x()))
        .unwrap();
    let (status, bytes) = send(app, req).await;
    assert_eq!(status, StatusCode::CREATED, "create should return 201");
    let json = parse_json(&bytes);
    assert_eq!(json["success"], json!(true));
    json["data"]["id"].as_i64().expect("data.id is an integer")
}

#[tokio::test]
async fn health_returns_ok() {
    let (app, _tmp) = fresh_app();
    let req = Request::builder()
        .method("GET")
        .uri("/api/health")
        .body(Body::empty())
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let json = parse_json(&bytes);
    assert_eq!(json["success"], json!(true));
    assert_eq!(json["data"]["status"], json!("ok"));
    assert_eq!(json["error"], Value::Null);
}

#[tokio::test]
async fn create_without_api_key_is_unauthorized() {
    let (app, _tmp) = fresh_app();
    let req = Request::builder()
        .method("POST")
        .uri("/api/favorites")
        .header("content-type", "application/json")
        .body(Body::from(create_body_game_x()))
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let json = parse_json(&bytes);
    assert_eq!(json["success"], json!(false));
    assert!(json["error"].is_string());
}

#[tokio::test]
async fn create_with_wrong_api_key_is_unauthorized() {
    let (app, _tmp) = fresh_app();
    let req = Request::builder()
        .method("POST")
        .uri("/api/favorites")
        .header("content-type", "application/json")
        .header("x-api-key", "wrongkey")
        .body(Body::from(create_body_game_x()))
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let json = parse_json(&bytes);
    assert_eq!(json["success"], json!(false));
}

#[tokio::test]
async fn create_missing_image_is_bad_request() {
    let (app, _tmp) = fresh_app();
    let body = json!({ "type": "game", "name": "X" }).to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/api/favorites")
        .header("content-type", "application/json")
        .header("x-api-key", "testkey")
        .body(Body::from(body))
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let json = parse_json(&bytes);
    assert_eq!(json["success"], json!(false));
    assert!(json["error"].is_string());
}

#[tokio::test]
async fn create_succeeds_and_returns_id() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;
    assert!(id >= 1, "created id should be a positive integer");
}

#[tokio::test]
async fn list_returns_created_item() {
    let (app, _tmp) = fresh_app();
    create_favorite_x(&app).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/favorites")
        .body(Body::empty())
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let json = parse_json(&bytes);
    assert_eq!(json["success"], json!(true));
    let items = json["data"]["items"].as_array().expect("items array");
    assert!(!items.is_empty(), "expected at least one item");
    assert!(
        json["data"]["total"].as_i64().unwrap() >= 1,
        "expected total >= 1"
    );
}

#[tokio::test]
async fn get_by_id_returns_matching_name() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/favorites/{id}"))
        .body(Body::empty())
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let json = parse_json(&bytes);
    assert_eq!(json["success"], json!(true));
    assert_eq!(json["data"]["name"], json!("X"));
    assert_eq!(json["data"]["id"], json!(id));
}

#[tokio::test]
async fn get_image_returns_png_bytes() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/favorites/{id}/image"))
        .body(Body::empty())
        .unwrap();
    let (status, content_type, bytes) = send_with_content_type(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/png"));
    assert!(!bytes.is_empty(), "image body should be non-empty");
}

#[tokio::test]
async fn update_rating_then_get_reflects_change() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;

    let body = json!({ "rating": 9.0 }).to_string();
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/favorites/{id}"))
        .header("content-type", "application/json")
        .header("x-api-key", "testkey")
        .body(Body::from(body))
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let json = parse_json(&bytes);
    assert_eq!(json["success"], json!(true));

    // Confirm via GET.
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/favorites/{id}"))
        .body(Body::empty())
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let json = parse_json(&bytes);
    assert_eq!(json["data"]["rating"], json!(9.0));
}

#[tokio::test]
async fn update_without_api_key_is_unauthorized() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;

    let body = json!({ "rating": 9.0 }).to_string();
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/favorites/{id}"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let json = parse_json(&bytes);
    assert_eq!(json["success"], json!(false));
}

#[tokio::test]
async fn delete_then_get_is_not_found() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/favorites/{id}"))
        .header("x-api-key", "testkey")
        .body(Body::empty())
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(bytes.is_empty(), "204 response should have empty body");

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/favorites/{id}"))
        .body(Body::empty())
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let json = parse_json(&bytes);
    assert_eq!(json["success"], json!(false));
    assert!(json["error"].is_string());
}

#[tokio::test]
async fn list_filters_by_type_and_query() {
    let (app, _tmp) = fresh_app();
    create_favorite_x(&app).await;

    // ?type=game returns the row.
    let req = Request::builder()
        .method("GET")
        .uri("/api/favorites?type=game")
        .body(Body::empty())
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let json = parse_json(&bytes);
    let items = json["data"]["items"].as_array().unwrap();
    assert!(!items.is_empty(), "type=game should return the row");

    // ?q=X returns the row.
    let req = Request::builder()
        .method("GET")
        .uri("/api/favorites?q=X")
        .body(Body::empty())
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let json = parse_json(&bytes);
    let items = json["data"]["items"].as_array().unwrap();
    assert!(!items.is_empty(), "q=X should return the row");

    // ?type=movie returns empty items.
    let req = Request::builder()
        .method("GET")
        .uri("/api/favorites?type=movie")
        .body(Body::empty())
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let json = parse_json(&bytes);
    let items = json["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 0, "type=movie should return no items");
    assert_eq!(json["data"]["total"], json!(0));
}

/// Decode the test PNG into raw bytes.
fn png_bytes() -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(PNG_1X1_B64)
        .expect("valid base64 png")
}

#[tokio::test]
async fn put_image_raw_body_replaces_and_returns_dto() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/favorites/{id}/image"))
        .header("content-type", "image/png")
        .header("x-api-key", "testkey")
        .body(Body::from(png_bytes()))
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let json = parse_json(&bytes);
    assert_eq!(json["success"], json!(true));
    assert_eq!(json["data"]["id"], json!(id));

    // The image is now fetchable.
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/favorites/{id}/image"))
        .body(Body::empty())
        .unwrap();
    let (status, content_type, body) = send_with_content_type(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("image/png"));
    assert!(!body.is_empty());
}

#[tokio::test]
async fn put_image_multipart_replaces_image() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;

    let boundary = "BOUNDARY123";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"image\"; filename=\"p.png\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    body.extend_from_slice(&png_bytes());
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/favorites/{id}/image"))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header("x-api-key", "testkey")
        .body(Body::from(body))
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK, "multipart upload should succeed");
    let json = parse_json(&bytes);
    assert_eq!(json["data"]["id"], json!(id));
}

#[tokio::test]
async fn put_image_multipart_missing_field_is_bad_request() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;

    let boundary = "BOUNDARY123";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"other\"\r\n\r\n");
    body.extend_from_slice(b"value");
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/favorites/{id}/image"))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header("x-api-key", "testkey")
        .body(Body::from(body))
        .unwrap();
    let (status, _bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn put_image_non_image_content_type_is_bad_request() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/favorites/{id}/image"))
        .header("content-type", "text/plain")
        .header("x-api-key", "testkey")
        .body(Body::from("hello"))
        .unwrap();
    let (status, _bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn put_image_non_image_bytes_is_bad_request() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;

    // Declared image/* but the bytes are not a recognised image format.
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/favorites/{id}/image"))
        .header("content-type", "image/png")
        .header("x-api-key", "testkey")
        .body(Body::from(vec![0u8, 1, 2, 3, 4, 5]))
        .unwrap();
    let (status, _bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn put_image_empty_body_is_bad_request() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/favorites/{id}/image"))
        .header("content-type", "image/png")
        .header("x-api-key", "testkey")
        .body(Body::empty())
        .unwrap();
    let (status, _bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn put_image_missing_row_is_not_found() {
    let (app, _tmp) = fresh_app();

    let req = Request::builder()
        .method("POST")
        .uri("/api/favorites/99999/image")
        .header("content-type", "image/png")
        .header("x-api-key", "testkey")
        .body(Body::from(png_bytes()))
        .unwrap();
    let (status, _bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn put_image_without_api_key_is_unauthorized() {
    let (app, _tmp) = fresh_app();
    let id = create_favorite_x(&app).await;

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/favorites/{id}/image"))
        .header("content-type", "image/png")
        .body(Body::from(png_bytes()))
        .unwrap();
    let (status, _bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn get_image_missing_row_is_not_found() {
    let (app, _tmp) = fresh_app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/favorites/99999/image")
        .body(Body::empty())
        .unwrap();
    let (status, _bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn index_route_serves_html() {
    let (app, _tmp) = fresh_app();
    let req = Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let (status, content_type, body) = send_with_content_type(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.as_deref().unwrap_or("").contains("html"),
        "index should be served as html"
    );
    assert!(!body.is_empty());
}

#[tokio::test]
async fn static_route_serves_known_asset() {
    let (app, _tmp) = fresh_app();
    let req = Request::builder()
        .method("GET")
        .uri("/static/app.js")
        .body(Body::empty())
        .unwrap();
    let (status, _content_type, body) = send_with_content_type(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.is_empty());
}

#[tokio::test]
async fn static_route_unknown_asset_is_not_found() {
    let (app, _tmp) = fresh_app();
    let req = Request::builder()
        .method("GET")
        .uri("/static/does-not-exist.js")
        .body(Body::empty())
        .unwrap();
    let (status, _bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn static_route_rejects_path_traversal() {
    let (app, _tmp) = fresh_app();
    let req = Request::builder()
        .method("GET")
        .uri("/static/..%2f..%2fetc%2fpasswd")
        .body(Body::empty())
        .unwrap();
    let (status, _bytes) = send(&app, req).await;
    // Either rejected as bad request by the traversal guard, or not found.
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
        "traversal attempt must not return 200, got {status}"
    );
}

#[tokio::test]
async fn index_substitutes_default_site_strings() {
    let (app, _tmp) = fresh_app();
    let req = Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8_lossy(&body);
    assert!(!html.contains("{{"), "all placeholders must be substituted");
    assert!(html.contains("memento"), "default site name present");
    assert!(
        html.contains("所有的美好都值得被珍藏与分享。"),
        "default slogan present"
    );
}

#[tokio::test]
async fn index_substitutes_custom_site_strings() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = memento::db::build_pool(tmp.path().to_str().unwrap()).unwrap();
    memento::db::init_schema(&pool).unwrap();
    let site = memento::config::SiteConfig {
        name: "老王的收藏".to_string(),
        slogan: "随心记录每一份热爱".to_string(),
        icon: "https://example.com/fav.png".to_string(),
    };
    let state = memento::state::AppState::new(pool, "testkey".to_string()).with_site(site);
    let app = memento::build_router(state);

    let req = Request::builder()
        .method("GET")
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("老王的收藏"), "custom site name injected");
    assert!(
        html.contains("随心记录每一份热爱"),
        "custom slogan injected"
    );
    assert!(
        html.contains("https://example.com/fav.png"),
        "custom favicon injected"
    );
}

#[tokio::test]
async fn verify_with_correct_key_returns_valid() {
    let (app, _tmp) = fresh_app();
    let req = Request::builder()
        .method("GET")
        .uri("/api/auth/verify")
        .header("x-api-key", "testkey")
        .body(Body::empty())
        .unwrap();
    let (status, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let json = parse_json(&bytes);
    assert_eq!(json["success"], json!(true));
    assert_eq!(json["data"]["valid"], json!(true));
}

#[tokio::test]
async fn verify_without_key_is_unauthorized() {
    let (app, _tmp) = fresh_app();
    let req = Request::builder()
        .method("GET")
        .uri("/api/auth/verify")
        .body(Body::empty())
        .unwrap();
    let (status, _bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn verify_with_wrong_key_is_unauthorized() {
    let (app, _tmp) = fresh_app();
    let req = Request::builder()
        .method("GET")
        .uri("/api/auth/verify")
        .header("x-api-key", "nope")
        .body(Body::empty())
        .unwrap();
    let (status, _bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
