//! Shared, clone-able application state injected into handlers.

use std::sync::Arc;

use crate::config::SiteConfig;
use crate::db::DbPool;

/// Clone-able application state. Cloning is cheap (pool + Arc are reference
/// counted).
#[derive(Clone)]
pub struct AppState {
    /// SQLite connection pool.
    pub pool: DbPool,
    /// Write-auth API key.
    pub api_key: Arc<String>,
    /// Operator-customisable site display strings (name / slogan / icon).
    pub site: Arc<SiteConfig>,
}

impl AppState {
    /// Construct new state with default site display config.
    pub fn new(pool: DbPool, api_key: String) -> Self {
        Self {
            pool,
            api_key: Arc::new(api_key),
            site: Arc::new(SiteConfig::default()),
        }
    }

    /// Return a copy of this state with the given site display config
    /// (immutable update — does not mutate the receiver).
    pub fn with_site(self, site: SiteConfig) -> Self {
        Self {
            site: Arc::new(site),
            ..self
        }
    }
}
