//! Shared, clone-able application state injected into handlers.

use std::sync::Arc;

use crate::db::DbPool;

/// Clone-able application state. Cloning is cheap (pool + Arc are reference
/// counted).
#[derive(Clone)]
pub struct AppState {
    /// SQLite connection pool.
    pub pool: DbPool,
    /// Write-auth API key.
    pub api_key: Arc<String>,
}

impl AppState {
    /// Construct new state.
    pub fn new(pool: DbPool, api_key: String) -> Self {
        Self {
            pool,
            api_key: Arc::new(api_key),
        }
    }
}
