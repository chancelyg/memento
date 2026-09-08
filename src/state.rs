//! Shared, clone-able application state injected into handlers.

use std::sync::Arc;

use crate::config::SiteConfig;
use crate::db::DbPool;

/// Clone-able application state. Cloning is cheap (pool + Arc are reference
/// counted).
#[derive(Clone)]
pub struct AppState {
    /// Browser sessions never substitute for an external API key.
    pub browser: Arc<crate::browser::BrowserAuth>,
    /// Only the first diary uses today's date in this business timezone.
    pub diary_timezone: chrono_tz::Tz,
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
            browser: Arc::new(crate::browser::BrowserAuth::disabled()),
            diary_timezone: chrono_tz::Asia::Shanghai,
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

    pub fn with_browser(self, browser: crate::browser::BrowserAuth) -> Self {
        Self {
            browser: Arc::new(browser),
            ..self
        }
    }

    pub fn with_diary_timezone(self, diary_timezone: chrono_tz::Tz) -> Self {
        Self {
            diary_timezone,
            ..self
        }
    }
}
