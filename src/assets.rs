//! Embedded static frontend assets and their HTTP handlers.

use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

/// Compile-time embedded contents of the `static/` directory.
#[derive(RustEmbed)]
#[folder = "static/"]
pub struct StaticAssets;

/// Serve `index.html` at `/`.
pub async fn index_handler() -> Response {
    serve_embedded("index.html")
}

/// Serve an arbitrary embedded asset at `/static/{file}`.
pub async fn static_handler(Path(file): Path<String>) -> Response {
    // Reject path traversal attempts.
    if file.contains("..") || file.starts_with('/') {
        return (StatusCode::BAD_REQUEST, "invalid path").into_response();
    }
    serve_embedded(&file)
}

/// Look up an embedded file and return it with the correct Content-Type.
fn serve_embedded(path: &str) -> Response {
    match StaticAssets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                content.data.into_owned(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
