//! memento — a personal poster wall (海报墙) web app.
//!
//! Loads configuration, initialises tracing and the SQLite database, builds
//! the axum router (public + API-key-guarded routes), and serves.
//!
//! This crate exposes both a library and a binary target. The library exists
//! so that integration tests under `tests/` can reach internal types such as
//! [`state::AppState`], [`db::build_pool`], [`db::init_schema`] and
//! [`build_router`].

pub mod assets;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod handlers;
pub mod image;
pub mod models;
pub mod repo;
pub mod state;

use axum::{
    extract::DefaultBodyLimit,
    http::{header, HeaderValue},
    middleware,
    routing::{get, post, put},
    Router,
};
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

/// Max request body for write routes (~20 MiB): enough for a base64-encoded
/// 10 MiB image (≈13.4 MiB) plus JSON, while bounding multipart/raw uploads.
const MAX_REQUEST_BYTES: usize = 20 * 1024 * 1024;

use crate::config::Config;
use crate::error::AppError;
use crate::state::AppState;

/// Initialise the tracing subscriber honouring `RUST_LOG` (default `info`).
pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}

/// Application bootstrap. Returns an error rather than panicking.
pub async fn run() -> Result<(), AppError> {
    let config = Config::from_env();

    if config.api_key_generated {
        tracing::warn!(
            "generated ephemeral API key: {} (set MEMENTO_API_KEY to persist it in production)",
            config.api_key
        );
    }

    let pool = db::build_pool(&config.db_path)?;
    db::init_schema(&pool)?;
    tracing::info!(db_path = %config.db_path, "database ready");

    let state = AppState::new(pool, config.api_key.clone()).with_site(config.site.clone());
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| AppError::Internal(Box::new(e)))?;
    tracing::info!(addr = %local_addr, "memento listening");

    axum::serve(listener, app)
        .await
        .map_err(|e| AppError::Internal(Box::new(e)))?;

    Ok(())
}

/// Build the full router: public routes, guarded write routes, static assets.
pub fn build_router(state: AppState) -> Router {
    // Write routes guarded by the API-key middleware.
    let guarded = Router::new()
        .route("/api/auth/verify", get(handlers::verify_key))
        .route("/api/favorites", post(handlers::favorites::create_favorite))
        .route(
            "/api/favorites/{id}",
            put(handlers::favorites::update_favorite).delete(handlers::favorites::delete_favorite),
        )
        .route(
            "/api/favorites/{id}/image",
            post(handlers::images::put_image),
        )
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ));

    // Public routes (no auth).
    let public = Router::new()
        .route("/", get(assets::index_handler))
        .route("/static/{*file}", get(assets::static_handler))
        .route("/api/health", get(handlers::health))
        .route("/api/favorites", get(handlers::favorites::list_favorites))
        .route(
            "/api/favorites/{id}",
            get(handlers::favorites::get_favorite),
        )
        .route(
            "/api/favorites/{id}/image",
            get(handlers::images::get_image),
        );

    public
        .merge(guarded)
        // Security headers applied to every response. `nosniff` is important for
        // the image endpoint (prevents content-type sniffing of stored blobs);
        // `DENY`/`no-referrer` mitigate clickjacking and referrer leakage.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(TraceLayer::new_for_http())
        // Reads are intentionally public (personal poster wall); writes still
        // require the X-API-Key header, which CORS preflight blocks cross-origin.
        .layer(CorsLayer::permissive())
        .with_state(state)
}
