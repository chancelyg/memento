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
