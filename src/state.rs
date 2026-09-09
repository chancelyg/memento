//! Shared, clone-able application state injected into handlers.

use std::sync::Arc;

use crate::db::DbPool;
use crate::settings::{AppSettings, SettingsStore, SiteSettings};

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
    /// Runtime YAML settings and their persistent store.
    pub settings: Arc<SettingsStore>,
}

impl AppState {
    /// Construct new state with default site display config.
    pub fn new(pool: DbPool, api_key: String) -> Self {
        Self {
            browser: Arc::new(crate::browser::BrowserAuth::disabled()),
            diary_timezone: chrono_tz::Asia::Shanghai,
            pool,
            api_key: Arc::new(api_key),
            settings: Arc::new(SettingsStore::in_memory()),
        }
    }

    /// Return a copy of this state with the given settings store.
    pub fn with_settings(self, settings: Arc<SettingsStore>) -> Self {
        Self { settings, ..self }
    }

    /// Test helper for rendering a legacy environment-style site config.
    pub fn with_site(self, site: crate::config::SiteConfig) -> Self {
        self.with_settings(Arc::new(SettingsStore::in_memory_with(AppSettings {
            site: SiteSettings::from(site),
            ..AppSettings::default()
        })))
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
