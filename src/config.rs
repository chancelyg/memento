//! Runtime configuration loaded from environment variables.

use std::env;

use rand::Rng;

/// Default listen address.
const DEFAULT_BIND: &str = "0.0.0.0:23457";
/// Default SQLite database path.
const DEFAULT_DB_PATH: &str = "./memento.db";
/// Length (in bytes) of a generated ephemeral API key before hex-encoding.
const GENERATED_KEY_BYTES: usize = 24;

/// Default site name (shown in the page title and top-bar brand).
pub const DEFAULT_SITE_NAME: &str = "memento";
/// Default hero slogan / subtitle.
pub const DEFAULT_SLOGAN: &str = "所有的美好都值得被珍藏与分享。";
/// Default favicon — an inline emoji SVG data URI (no external request).
pub const DEFAULT_ICON: &str = "data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><text y='.9em' font-size='90'>🗂️</text></svg>";

/// Operator-customisable display strings, injected into `index.html` at serve
/// time. Each falls back to a built-in default when its env var is unset/blank.
#[derive(Debug, Clone)]
pub struct SiteConfig {
    /// `MEMENTO_SITE_NAME` — page title + brand text.
    pub name: String,
    /// `MEMENTO_SLOGAN` — hero subtitle.
    pub slogan: String,
    /// `MEMENTO_ICON` — favicon href (a URL or data URI).
    pub icon: String,
}

impl Default for SiteConfig {
    fn default() -> Self {
        Self {
            name: DEFAULT_SITE_NAME.to_string(),
            slogan: DEFAULT_SLOGAN.to_string(),
            icon: DEFAULT_ICON.to_string(),
        }
    }
}

impl SiteConfig {
    /// Read the site display config from the environment, applying defaults.
    pub fn from_env() -> Self {
        Self {
            name: env_or("MEMENTO_SITE_NAME", DEFAULT_SITE_NAME),
            slogan: env_or("MEMENTO_SLOGAN", DEFAULT_SLOGAN),
            icon: env_or("MEMENTO_ICON", DEFAULT_ICON),
        }
    }
}

/// Read a trimmed non-empty env var, or fall back to `default`.
fn env_or(key: &str, default: &str) -> String {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Application configuration derived from the process environment.
#[derive(Debug, Clone)]
pub struct Config {
    /// API key required for write endpoints.
    pub api_key: String,
    /// Whether the key was generated at startup (vs. supplied via env).
    pub api_key_generated: bool,
    /// Path to the SQLite database file.
    pub db_path: String,
    /// Address to bind the HTTP server to.
    pub bind: String,
    /// Operator-customisable site display strings.
    pub site: SiteConfig,
}

impl Config {
    /// Build a [`Config`] from environment variables, applying defaults.
    ///
    /// If `MEMENTO_API_KEY` is unset or empty a random key is generated and
    /// [`Config::api_key_generated`] is set to `true` so the caller can log it.
    pub fn from_env() -> Self {
        let (api_key, api_key_generated) = match env::var("MEMENTO_API_KEY") {
            Ok(key) if !key.trim().is_empty() => (key.trim().to_string(), false),
            _ => (generate_api_key(), true),
        };

        let db_path = env::var("MEMENTO_DB_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_DB_PATH.to_string());

        let bind = env::var("MEMENTO_BIND")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BIND.to_string());

        Self {
            api_key,
            api_key_generated,
            db_path,
            bind,
            site: SiteConfig::from_env(),
        }
    }
}

/// Generate a random hex-encoded API key.
fn generate_api_key() -> String {
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; GENERATED_KEY_BYTES];
    rng.fill(&mut bytes[..]);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_is_48_lowercase_hex_chars() {
        let key = generate_api_key();

        // 24 bytes hex-encoded == 48 characters.
        assert_eq!(key.len(), GENERATED_KEY_BYTES * 2);
        assert_eq!(key.len(), 48);

        // Every character must be a lowercase hex digit.
        assert!(
            key.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "key contained non-lowercase-hex characters: {key}"
        );
    }

    #[test]
    fn generated_keys_differ_between_calls() {
        // Two independent draws of 24 random bytes colliding is astronomically
        // unlikely, so this is deterministic in practice (non-flaky).
        let a = generate_api_key();
        let b = generate_api_key();
        assert_ne!(a, b);
    }

    use std::sync::Mutex;

    /// Serialises env-var mutation across tests in this module (process env is
    /// global and shared between parallel test threads).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        env::remove_var("MEMENTO_API_KEY");
        env::remove_var("MEMENTO_DB_PATH");
        env::remove_var("MEMENTO_BIND");
        env::remove_var("MEMENTO_SITE_NAME");
        env::remove_var("MEMENTO_SLOGAN");
        env::remove_var("MEMENTO_ICON");
    }

    #[test]
    fn from_env_uses_defaults_and_generates_key_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();

        let cfg = Config::from_env();
        assert!(
            cfg.api_key_generated,
            "key should be generated when env unset"
        );
        assert_eq!(cfg.api_key.len(), 48);
        assert_eq!(cfg.db_path, DEFAULT_DB_PATH);
        assert_eq!(cfg.bind, DEFAULT_BIND);
    }

    #[test]
    fn from_env_reads_supplied_values_and_trims() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("MEMENTO_API_KEY", "  secret  ");
        env::set_var("MEMENTO_DB_PATH", "/tmp/x.db");
        env::set_var("MEMENTO_BIND", "127.0.0.1:9000");

        let cfg = Config::from_env();
        assert!(!cfg.api_key_generated);
        assert_eq!(cfg.api_key, "secret");
        assert_eq!(cfg.db_path, "/tmp/x.db");
        assert_eq!(cfg.bind, "127.0.0.1:9000");

        clear_env();
    }

    #[test]
    fn site_config_defaults_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();

        let site = SiteConfig::from_env();
        assert_eq!(site.name, DEFAULT_SITE_NAME);
        assert_eq!(site.slogan, DEFAULT_SLOGAN);
        assert_eq!(site.icon, DEFAULT_ICON);
    }

    #[test]
    fn site_config_reads_and_trims_overrides() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("MEMENTO_SITE_NAME", "  老王的收藏  ");
        env::set_var("MEMENTO_SLOGAN", "随心记录");
        env::set_var("MEMENTO_ICON", "https://example.com/fav.png");

        let site = SiteConfig::from_env();
        assert_eq!(site.name, "老王的收藏");
        assert_eq!(site.slogan, "随心记录");
        assert_eq!(site.icon, "https://example.com/fav.png");

        // Blank override falls back to the default.
        env::set_var("MEMENTO_SLOGAN", "   ");
        assert_eq!(SiteConfig::from_env().slogan, DEFAULT_SLOGAN);

        clear_env();
    }

    #[test]
    fn from_env_blank_key_falls_back_to_generated() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("MEMENTO_API_KEY", "   ");
        env::set_var("MEMENTO_DB_PATH", "   ");
        env::set_var("MEMENTO_BIND", "   ");

        let cfg = Config::from_env();
        assert!(
            cfg.api_key_generated,
            "blank key should be treated as unset"
        );
        assert_eq!(
            cfg.db_path, DEFAULT_DB_PATH,
            "blank db path falls back to default"
        );
        assert_eq!(cfg.bind, DEFAULT_BIND, "blank bind falls back to default");

        clear_env();
    }
}
