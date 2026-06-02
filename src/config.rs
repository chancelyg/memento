//! Runtime configuration loaded from environment variables.

use std::env;

use rand::Rng;

/// Default listen address.
const DEFAULT_BIND: &str = "0.0.0.0:23457";
/// Default SQLite database path.
const DEFAULT_DB_PATH: &str = "./memento.db";
/// Length (in bytes) of a generated ephemeral API key before hex-encoding.
const GENERATED_KEY_BYTES: usize = 24;

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
}

impl Config {
    /// Build a [`Config`] from environment variables, applying defaults.
    ///
    /// If `MEMENTO_API_KEY` is unset or empty a random key is generated and
    /// [`Config::api_key_generated`] is set to `true` so the caller can log it.
    pub fn from_env() -> Self {
        let (api_key, api_key_generated) = match env::var("MEMENTO_API_KEY") {
            Ok(key) if !key.trim().is_empty() => (key, false),
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
