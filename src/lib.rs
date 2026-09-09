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
pub mod browser;
pub mod cli;
pub mod config;
pub mod db;
pub mod diary;
pub mod error;
pub mod handlers;
pub mod image;
pub mod models;
pub mod repo;
pub mod settings;
pub mod state;

use axum::{
    extract::{DefaultBodyLimit, Request},
    http::{header, HeaderValue, StatusCode},
    middleware,
    response::{IntoResponse, Response},
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
use std::sync::Arc;

/// Initialise the tracing subscriber honouring `RUST_LOG` (default `info`).
pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}

/// Application bootstrap. Returns an error rather than panicking.
pub async fn run() -> Result<(), AppError> {
    let config = Config::from_env();
    let browser = browser::BrowserAuth::from_env()?;
    let diary_timezone = std::env::var("MEMENTO_DIARY_TIMEZONE")
        .unwrap_or_else(|_| "Asia/Shanghai".into())
        .parse::<chrono_tz::Tz>()
        .map_err(|_| AppError::BadRequest("invalid diary timezone".into()))?;

    if config.api_key_generated {
        tracing::warn!("MEMENTO_API_KEY is unset; external API access is disabled until a fixed key is configured");
    }

    let settings = Arc::new(settings::SettingsStore::load_or_create(
        &config.config_path,
        Some(config.site.clone()),
    )?);
    let pool = db::build_pool(&config.db_path)?;
    db::init_schema(&pool)?;
    tracing::info!(db_path = %config.db_path, "database ready");

    let state = AppState::new(
        pool,
        if config.api_key_generated {
            String::new()
        } else {
            config.api_key.clone()
        },
    )
    .with_settings(settings)
    .with_browser(browser)
    .with_diary_timezone(diary_timezone);
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
    // Cookie routes stay same-origin. Production checks the configured Origin;
    // development skips that deployment constraint, but both retain CSRF tokens.
    let sessions = Router::new()
        .route(
            "/session",
            post(browser::login)
                .get(browser::status)
                .delete(browser::logout),
        )
        .layer(DefaultBodyLimit::max(8 * 1024));
    let private = Router::new()
        .route(
            "/private/diaries",
            get(handlers::diary::list).post(handlers::diary::create),
        )
        .route(
            "/private/diaries/{id}",
            get(handlers::diary::get)
                .patch(handlers::diary::update)
                .delete(handlers::diary::delete),
        )
        .route(
            "/private/settings/site",
            get(handlers::settings::get_site).put(handlers::settings::put_site),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            browser::require_session,
        ));
    let diary_api = Router::new()
        .route(
            "/api/diaries",
            get(handlers::diary::list).post(handlers::diary::create),
        )
        .route(
            "/api/diaries/{id}",
            get(handlers::diary::get)
                .patch(handlers::diary::update)
                .delete(handlers::diary::delete),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ));
    let diary_routes = sessions
        .merge(private)
        .merge(diary_api)
        .route("/diary", get(assets::diary_handler))
        .route("/login", get(assets::login_handler))
        .route("/admin", get(assets::admin_handler))
        .layer(middleware::from_fn(private_response));
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
        // Preserve existing key-based collection CORS without granting it to sessions.
        .layer(CorsLayer::permissive())
        .merge(diary_routes)
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
        // Do not log request URIs: diary search queries may contain private text.
        .layer(TraceLayer::new_for_http().make_span_with(
            |request: &Request| tracing::debug_span!("http", method = %request.method()),
        ))
        .with_state(state)
}

async fn private_response(request: Request, next: middleware::Next) -> Response {
    let mut response = next.run(request).await;
    if response.status() == StatusCode::METHOD_NOT_ALLOWED {
        response = (
            StatusCode::METHOD_NOT_ALLOWED,
            axum::Json(error::error_envelope("method not allowed")),
        )
            .into_response();
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
