//! Two-step browser login and persistent, revocable cookie sessions.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    extract::{FromRequest, Request, State},
    http::{header::SET_COOKIE, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use rand::{rngs::OsRng, RngCore};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use totp_rs::{Algorithm, Secret, TOTP};
use url::Url;

use crate::{
    config::Environment,
    error::{error_envelope, ApiResponse, AppError, AppResult},
    state::AppState,
};

const CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_CHALLENGES: usize = 64;

// Deliberately not Debug: neither configuration nor login bodies belong in logs.
pub struct BrowserConfig {
    pub username: String,
    pub password_hash: String,
    pub totp_secret: String,
    pub origin: String,
    pub environment: Environment,
    pub session_ttl_days: u64,
}

pub struct BrowserAuth {
    config: Option<BrowserConfig>,
    credential_hash: String,
    challenges: Mutex<HashMap<String, Challenge>>,
    totp: Option<TOTP>,
    session_ttl: i64,
}

struct Challenge {
    created_at: Instant,
    attempts: u8,
}

impl BrowserAuth {
    pub fn new(config: BrowserConfig) -> AppResult<Self> {
        if config.username.trim().is_empty() || config.username.len() > 256 {
            return Err(config_error());
        }
        if !config.password_hash.is_ascii()
            || !["$2a$", "$2b$", "$2y$"]
                .iter()
                .any(|prefix| config.password_hash.starts_with(prefix))
        {
            return Err(config_error());
        }
        let hash = config
            .password_hash
            .parse::<bcrypt::HashParts>()
            .map_err(|_| config_error())?;
        if !(4..=31).contains(&hash.get_cost()) {
            return Err(config_error());
        }
        let secret = Secret::Encoded(config.totp_secret.clone())
            .to_bytes()
            .map_err(|_| config_error())?;
        if secret.len() < 20 {
            return Err(config_error());
        }
        let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, secret).map_err(|_| config_error())?;
        let session_ttl = config
            .session_ttl_days
            .checked_mul(86400)
            .and_then(|seconds| i64::try_from(seconds).ok())
            .filter(|seconds| *seconds > 0)
            .ok_or_else(config_error)?;
        chrono::Utc::now()
            .timestamp()
            .checked_add(session_ttl)
            .ok_or_else(config_error)?;
        if matches!(config.environment, Environment::Production)
            && parse_origin(&config.origin).is_none()
        {
            return Err(config_error());
        }
        // Length-prefix the fields so different field boundaries cannot collide.
        let mut digest = Sha256::new();
        digest.update(b"memento/browser-credential/v1");
        for field in [&config.username, &config.password_hash, &config.totp_secret] {
            digest.update((field.len() as u64).to_be_bytes());
            digest.update(field.as_bytes());
        }
        Ok(Self {
            config: Some(config),
            credential_hash: format!("{:x}", digest.finalize()),
            challenges: Mutex::new(HashMap::new()),
            totp: Some(totp),
            session_ttl,
        })
    }

    pub fn disabled() -> Self {
        Self {
            config: None,
            credential_hash: String::new(),
            challenges: Mutex::new(HashMap::new()),
            totp: None,
            session_ttl: 0,
        }
    }

    pub fn from_env() -> AppResult<Self> {
        fn read(name: &str) -> AppResult<Option<String>> {
            match std::env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(std::env::VarError::NotUnicode(_)) => Err(config_error()),
            }
        }
        Self::from_values(
            read("MEMENTO_LOGIN_USERNAME")?,
            read("MEMENTO_PASSWORD_HASH")?,
            read("MEMENTO_TOTP_SECRET")?,
            read("MEMENTO_PUBLIC_ORIGIN")?,
            Environment::from_env()?,
            read("MEMENTO_SESSION_TTL_DAYS")?,
        )
    }

    fn from_values(
        username: Option<String>,
        password_hash: Option<String>,
        totp_secret: Option<String>,
        origin: Option<String>,
        environment: Environment,
        ttl: Option<String>,
    ) -> AppResult<Self> {
        let origin = origin.filter(|value| !value.is_empty());
        let session_ttl_days: u64 = match ttl {
            None => 7,
            Some(value) if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                value.parse().map_err(|_| config_error())?
            }
            Some(_) => return Err(config_error()),
        };
        if session_ttl_days == 0
            || session_ttl_days
                .checked_mul(86400)
                .and_then(|s| i64::try_from(s).ok())
                .and_then(|s| chrono::Utc::now().timestamp().checked_add(s))
                .is_none()
        {
            return Err(config_error());
        }
        match password_hash {
            None if username.is_none() && origin.is_none() && totp_secret.is_none() => {
                Ok(Self::disabled())
            }
            None => Err(config_error()),
            Some(password_hash) => Self::new(BrowserConfig {
                username: username.unwrap_or_else(|| "admin".to_owned()),
                password_hash,
                totp_secret: totp_secret.ok_or_else(config_error)?,
                origin: origin.unwrap_or_default(),
                environment,
                session_ttl_days,
            }),
        }
    }

    fn challenge(&self, now: Instant) -> Result<String, StatusCode> {
        let mut pending = self
            .challenges
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        pending.retain(|_, entry| now.saturating_duration_since(entry.created_at) < CHALLENGE_TTL);
        if pending.len() >= MAX_CHALLENGES {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        let token = random_token()?;
        pending.insert(
            token.clone(),
            Challenge {
                created_at: now,
                attempts: 0,
            },
        );
        Ok(token)
    }

    fn consume_challenge(
        &self,
        challenge: &str,
        code: &str,
        now: Instant,
        timestamp: u64,
    ) -> Result<u64, StatusCode> {
        let mut pending = self
            .challenges
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        pending.retain(|_, entry| now.saturating_duration_since(entry.created_at) < CHALLENGE_TTL);
        let entry = pending.get_mut(challenge).ok_or(StatusCode::UNAUTHORIZED)?;
        let totp = self.totp.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
        // Verification and consumption share a lock, so only one request can succeed.
        let current_step = timestamp / 30;
        let last_step = current_step.saturating_add(1).min(u64::MAX / 30);
        for step in (current_step.saturating_sub(1)..=last_step).rev() {
            let expected = totp.generate(step * 30);
            if bool::from(code.as_bytes().ct_eq(expected.as_bytes())) {
                pending.remove(challenge);
                return Ok(step);
            }
        }
        entry.attempts += 1;
        if entry.attempts >= 5 {
            pending.remove(challenge);
        }
        Err(StatusCode::UNAUTHORIZED)
    }

    fn check_origin(&self, headers: &HeaderMap) -> Result<bool, StatusCode> {
        let config = self.config.as_ref().ok_or(StatusCode::FORBIDDEN)?;
        if matches!(config.environment, Environment::Development) {
            return Ok(false);
        }
        let value = single_header(headers, "origin").ok_or(StatusCode::FORBIDDEN)?;
        if value != config.origin {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(config.origin.starts_with("https://"))
    }

    fn check_csrf(&self, headers: &HeaderMap, session: &Session) -> Result<bool, StatusCode> {
        let secure = self.check_origin(headers)?;
        let csrf = single_header(headers, "x-csrf-token").ok_or(StatusCode::FORBIDDEN)?;
        if bool::from(csrf.as_bytes().ct_eq(session.csrf_token.as_bytes())) {
            Ok(secure)
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }

    fn cookie(&self, token: &str, clearing: bool, secure: bool) -> Result<HeaderValue, StatusCode> {
        let max_age = if clearing { 0 } else { self.session_ttl };
        let config = self
            .config
            .as_ref()
            .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
        let same_site = if matches!(config.environment, Environment::Development) {
            "Lax"
        } else {
            "Strict"
        };
        let secure = if secure { "; Secure" } else { "" };
        HeaderValue::from_str(&format!(
            "memento_session={token}; HttpOnly; SameSite={same_site}; Path=/; Max-Age={max_age}{secure}"
        ))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    }
}

fn parse_origin(value: &str) -> Option<Url> {
    let origin = Url::parse(value).ok()?;
    (matches!(origin.scheme(), "http" | "https")
        && origin.host().is_some()
        && origin.origin().ascii_serialization() == value)
        .then_some(origin)
}

fn config_error() -> AppError {
    AppError::BadRequest("invalid browser login configuration".to_owned())
}

fn failure(status: StatusCode) -> Response {
    let message = match status {
        StatusCode::BAD_REQUEST => "invalid login request",
        StatusCode::UNAUTHORIZED => "invalid or missing browser credentials",
        StatusCode::FORBIDDEN => "browser request forbidden",
        StatusCode::PAYLOAD_TOO_LARGE => "login request too large",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "login requires application/json",
        StatusCode::SERVICE_UNAVAILABLE => "browser login unavailable",
        _ => "browser authentication failed",
    };
    (status, Json(error_envelope(message))).into_response()
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        None
    } else {
        Some(value)
    }
}

fn session_token(headers: &HeaderMap) -> Result<&str, StatusCode> {
    let mut token = None;
    for value in headers.get_all("cookie").iter() {
        let value = value.to_str().map_err(|_| StatusCode::UNAUTHORIZED)?;
        for part in value.split(';').map(str::trim) {
            let Some((name, value)) = part.split_once('=') else {
                if part == "memento_session" {
                    return Err(StatusCode::UNAUTHORIZED);
                }
                continue;
            };
            if name == "memento_session" {
                if token.is_some()
                    || value.len() != 64
                    || !value.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(StatusCode::UNAUTHORIZED);
                }
                token = Some(value);
            }
        }
    }
    token.ok_or(StatusCode::UNAUTHORIZED)
}

fn random_token() -> Result<String, StatusCode> {
    let mut bytes = [0u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // Each byte contributes exactly two lowercase hexadecimal characters.
    const HEX: &[u8; 16] = b"0123456789abcdef";
    Ok(bytes
        .iter()
        .flat_map(|b| {
            [
                HEX[(b >> 4) as usize] as char,
                HEX[(b & 15) as usize] as char,
            ]
        })
        .collect())
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LoginBody {
    Password(PasswordBody),
    Totp(TotpBody),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordBody {
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TotpBody {
    challenge: String,
    code: String,
}

#[derive(Serialize)]
struct ChallengeData {
    requires_totp: bool,
    challenge: String,
}

#[derive(Serialize)]
struct SessionData {
    username: String,
    csrf_token: String,
}

struct Session {
    token_hash: String,
    csrf_token: String,
}

async fn lookup(state: &AppState, headers: &HeaderMap) -> Result<Session, StatusCode> {
    if state.browser.config.is_none() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let token_hash = format!("{:x}", Sha256::digest(session_token(headers)?.as_bytes()));
    let pool = state.pool.clone();
    let credential_hash = state.browser.credential_hash.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let csrf_token: Option<String> = conn
            .query_row(
                "SELECT csrf_token FROM browser_sessions
             WHERE token_hash = ?1 AND credential_hash = ?2 AND expires_at > ?3",
                rusqlite::params![token_hash, credential_hash, chrono::Utc::now().timestamp()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let csrf_token = csrf_token.ok_or(StatusCode::UNAUTHORIZED)?;
        if csrf_token.len() != 64 || !csrf_token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Ok(Session {
            token_hash,
            csrf_token,
        })
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
}

pub async fn login(State(state): State<AppState>, request: Request) -> Response {
    if state.browser.config.is_none() {
        return failure(StatusCode::SERVICE_UNAVAILABLE);
    }
    let secure = match state.browser.check_origin(request.headers()) {
        Ok(secure) => secure,
        Err(status) => return failure(status),
    };
    let body = match Json::<LoginBody>::from_request(request, &state).await {
        Ok(Json(body)) => body,
        Err(rejection) => {
            let status = match rejection.status() {
                StatusCode::PAYLOAD_TOO_LARGE => StatusCode::PAYLOAD_TOO_LARGE,
                StatusCode::UNSUPPORTED_MEDIA_TYPE => StatusCode::UNSUPPORTED_MEDIA_TYPE,
                _ => StatusCode::BAD_REQUEST,
            };
            return failure(status);
        }
    };
    if let LoginBody::Password(body) = body {
        if body.password.is_empty() || body.password.len() > 72 {
            return failure(StatusCode::BAD_REQUEST);
        }
        let auth = state.browser.clone();
        let verified = tokio::task::spawn_blocking(move || {
            let config = auth
                .config
                .as_ref()
                .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
            let password_matches = bcrypt::verify(body.password.as_bytes(), &config.password_hash)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            // Always perform password verification, including for an unknown username.
            let username_matches =
                bool::from(body.username.as_bytes().ct_eq(config.username.as_bytes()));
            if password_matches & username_matches {
                Ok(())
            } else {
                Err(StatusCode::UNAUTHORIZED)
            }
        })
        .await;
        match verified {
            Ok(Ok(())) => {}
            Ok(Err(status)) => return failure(status),
            Err(_) => return failure(StatusCode::INTERNAL_SERVER_ERROR),
        }
        return match state.browser.challenge(Instant::now()) {
            Ok(challenge) => Json(ApiResponse::ok(ChallengeData {
                requires_totp: true,
                challenge,
            }))
            .into_response(),
            Err(status) => failure(status),
        };
    }
    let LoginBody::Totp(body) = body else {
        unreachable!()
    };
    if body.challenge.len() != 64
        || !body.challenge.bytes().all(|b| b.is_ascii_hexdigit())
        || body.code.len() != 6
        || !body.code.bytes().all(|b| b.is_ascii_digit())
    {
        return failure(StatusCode::BAD_REQUEST);
    }
    let timestamp = match u64::try_from(chrono::Utc::now().timestamp()) {
        Ok(value) => value,
        Err(_) => return failure(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let totp_step = match state.browser.consume_challenge(
        &body.challenge,
        &body.code,
        Instant::now(),
        timestamp,
    ) {
        Ok(step) => step,
        Err(status) => return failure(status),
    };
    let token = match random_token() {
        Ok(token) => token,
        Err(status) => return failure(status),
    };
    let csrf_token = match random_token() {
        Ok(token) => token,
        Err(status) => return failure(status),
    };
    let cookie = match state.browser.cookie(&token, false, secure) {
        Ok(cookie) => cookie,
        Err(status) => return failure(status),
    };
    let token_hash = format!("{:x}", Sha256::digest(token.as_bytes()));
    let credential_hash = state.browser.credential_hash.clone();
    let pool = state.pool.clone();
    let stored_csrf = csrf_token.clone();
    let session_ttl = state.browser.session_ttl;
    let inserted = tokio::task::spawn_blocking(move || {
        let mut conn = pool.get().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let totp_step = i64::try_from(totp_step).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        // The step claim and session must commit together, including across auth instances.
        let changed = tx
            .execute(
                "INSERT INTO browser_totp_state (credential_hash, last_used_step) VALUES (?1, ?2)
             ON CONFLICT(credential_hash) DO UPDATE SET last_used_step = excluded.last_used_step
             WHERE browser_totp_state.last_used_step < excluded.last_used_step",
                rusqlite::params![credential_hash, totp_step],
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if changed == 0 {
            return Err(StatusCode::UNAUTHORIZED);
        }
        let now = chrono::Utc::now().timestamp();
        tx.execute(
            "DELETE FROM browser_sessions WHERE expires_at <= ?1 OR credential_hash != ?2",
            rusqlite::params![now, credential_hash],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let expiry = now
            .checked_add(session_ttl)
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
        tx.execute(
            "INSERT INTO browser_sessions (token_hash, csrf_token, credential_hash, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![token_hash, stored_csrf, credential_hash, expiry],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        tx.commit().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    })
    .await;
    match inserted {
        Ok(Ok(())) => {}
        Ok(Err(status)) => return failure(status),
        Err(_) => return failure(StatusCode::INTERNAL_SERVER_ERROR),
    }
    let Some(config) = state.browser.config.as_ref() else {
        return failure(StatusCode::SERVICE_UNAVAILABLE);
    };
    let mut response = Json(ApiResponse::ok(SessionData {
        username: config.username.clone(),
        csrf_token,
    }))
    .into_response();
    response.headers_mut().insert(SET_COOKIE, cookie);
    response
}

pub async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = match lookup(&state, &headers).await {
        Ok(session) => session,
        Err(status) => return failure(status),
    };
    let Some(config) = state.browser.config.as_ref() else {
        return failure(StatusCode::UNAUTHORIZED);
    };
    Json(ApiResponse::ok(SessionData {
        username: config.username.clone(),
        csrf_token: session.csrf_token,
    }))
    .into_response()
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = match lookup(&state, &headers).await {
        Ok(session) => session,
        Err(status) => return failure(status),
    };
    let secure = match state.browser.check_csrf(&headers, &session) {
        Ok(secure) => secure,
        Err(status) => return failure(status),
    };
    let cookie = match state.browser.cookie("", true, secure) {
        Ok(cookie) => cookie,
        Err(status) => return failure(status),
    };
    let pool = state.pool.clone();
    let credential_hash = state.browser.credential_hash.clone();
    let deleted = tokio::task::spawn_blocking(move || {
        let conn = pool.get().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let count = conn.execute(
            "DELETE FROM browser_sessions WHERE token_hash = ?1 AND credential_hash = ?2 AND expires_at > ?3",
            rusqlite::params![session.token_hash, credential_hash, chrono::Utc::now().timestamp()],
        ).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if count == 1 { Ok(()) } else { Err(StatusCode::UNAUTHORIZED) }
    }).await;
    match deleted {
        Ok(Ok(())) => {}
        Ok(Err(status)) => return failure(status),
        Err(_) => return failure(StatusCode::INTERNAL_SERVER_ERROR),
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(SET_COOKIE, cookie);
    response
}

pub async fn require_session(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let session = match lookup(&state, request.headers()).await {
        Ok(session) => session,
        Err(status) => return failure(status),
    };
    if !request.method().is_safe() {
        if let Err(status) = state.browser.check_csrf(request.headers(), &session) {
            return failure(status);
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    fn config() -> BrowserConfig {
        static HASH: OnceLock<String> = OnceLock::new();
        let hash = HASH.get_or_init(|| bcrypt::hash("synthetic", 4).unwrap());
        BrowserConfig {
            username: "admin".to_owned(),
            password_hash: hash.clone(),
            totp_secret: "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ".to_owned(),
            origin: "https://diary.example.test".to_owned(),
            environment: Environment::Production,
            session_ttl_days: 7,
        }
    }

    #[test]
    fn safe_origins_only_and_no_implicit_normalization() {
        for origin in [
            "http://diary.example.test",
            "http://localhost.attacker.test",
            "http://127.0.0.2",
            "http://192.168.1.10:23457",
            "https://diary.example.test",
            "https://diary.example.test:8443",
            "http://localhost",
            "http://127.0.0.1:23457",
            "http://[::1]:23457",
        ] {
            let mut value = config();
            value.origin = origin.to_owned();
            assert!(BrowserAuth::new(value).is_ok());
        }
        for origin in [
            "",
            "https://diary.example.test/",
            "https://diary.example.test/path",
            "https://diary.example.test?query",
            "https://diary.example.test#fragment",
            "https://user@diary.example.test",
            "https://diary.example.test:443",
            "https://DIARY.example.test",
            "null",
            "file:///tmp/diary",
            "ftp://localhost",
        ] {
            let mut value = config();
            value.origin = origin.to_owned();
            assert!(BrowserAuth::new(value).is_err());
        }
    }

    #[test]
    fn bcrypt_format_and_cost_validation() {
        let original = config().password_hash;
        for hash in [
            String::new(),
            "invalid".to_owned(),
            original.replace("$2b$", "$2x$"),
            original.replace("$04$", "$03$"),
            original.replace("$04$", "$32$"),
            original.rsplit_once('$').unwrap().0.to_owned(),
            "a".repeat(513),
        ] {
            let mut value = config();
            value.password_hash = hash;
            assert!(BrowserAuth::new(value).is_err());
        }
        for username in [String::new(), " ".to_owned(), "a".repeat(257)] {
            let mut value = config();
            value.username = username;
            assert!(BrowserAuth::new(value).is_err());
        }
    }

    #[test]
    fn environment_values_disable_only_when_all_unset() {
        assert!(
            BrowserAuth::from_values(None, None, None, None, Environment::Production, None)
                .unwrap()
                .config
                .is_none()
        );
        assert!(BrowserAuth::from_values(
            Some("admin".to_owned()),
            None,
            None,
            None,
            Environment::Production,
            None
        )
        .is_err());
        assert!(BrowserAuth::from_values(
            None,
            None,
            None,
            Some(config().origin),
            Environment::Production,
            None
        )
        .is_err());
        assert!(BrowserAuth::from_values(
            None,
            Some(config().password_hash),
            None,
            Some(config().origin),
            Environment::Production,
            None
        )
        .is_err());
        for origin in [None, Some(String::new())] {
            let auth = BrowserAuth::from_values(
                None,
                Some(config().password_hash),
                Some(config().totp_secret),
                origin,
                Environment::Development,
                None,
            )
            .unwrap();
            let value = auth.config.unwrap();
            assert_eq!(value.username, "admin");
            assert!(value.origin.is_empty());
        }
        assert!(BrowserAuth::from_values(
            None,
            None,
            None,
            Some(String::new()),
            Environment::Production,
            None
        )
        .unwrap()
        .config
        .is_none());
        let auth = BrowserAuth::from_values(
            None,
            Some(config().password_hash),
            Some(config().totp_secret),
            Some(config().origin),
            Environment::Production,
            None,
        )
        .unwrap();
        assert!(auth
            .config
            .as_ref()
            .is_some_and(|value| value.username == "admin"));
    }

    #[test]
    fn credential_fingerprint_binds_credentials_not_origin() {
        let original = BrowserAuth::new(config()).unwrap();
        let identical = BrowserAuth::new(config()).unwrap();
        assert!(original.credential_hash == identical.credential_hash);
        for field in 0..3 {
            let mut value = config();
            match field {
                0 => value.username.push('x'),
                1 => value.password_hash = bcrypt::hash("changed", 4).unwrap(),
                _ => value.totp_secret = "A".repeat(32),
            }
            let changed = BrowserAuth::new(value).unwrap();
            assert!(original.credential_hash != changed.credential_hash);
        }
        let mut value = config();
        value.origin = "http://other.test".to_owned();
        assert_eq!(
            BrowserAuth::new(value).unwrap().credential_hash,
            original.credential_hash
        );
    }

    #[test]
    fn development_ignores_origin_and_host() {
        let mut value = config();
        value.environment = Environment::Development;
        value.origin = "not an origin".to_owned();
        let auth = BrowserAuth::new(value).unwrap();
        assert_eq!(auth.check_origin(&HeaderMap::new()), Ok(false));
        for (origin, host) in [
            ("http://192.168.1.10:23457", "192.168.1.10:23457"),
            ("https://[fd00::1]", "[fd00::1]:443"),
            ("http://localhost", "localhost:80"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("origin", origin.parse().unwrap());
            headers.insert("host", host.parse().unwrap());
            assert_eq!(auth.check_origin(&headers), Ok(false));
        }
    }

    #[test]
    fn challenge_expiry_capacity_and_reclamation() {
        let auth = BrowserAuth::new(config()).unwrap();
        let start = Instant::now();
        let first = auth.challenge(start).unwrap();
        for _ in 1..64 {
            auth.challenge(start).unwrap();
        }
        assert_eq!(auth.challenge(start), Err(StatusCode::SERVICE_UNAVAILABLE));
        let code = auth.totp.as_ref().unwrap().generate(1234567890);
        assert_eq!(
            auth.consume_challenge(&first, &code, start + CHALLENGE_TTL, 1234567890),
            Err(StatusCode::UNAUTHORIZED)
        );
        assert!(auth.challenges.lock().unwrap().is_empty());
        auth.challenge(start + CHALLENGE_TTL).unwrap();
    }

    #[tokio::test]
    async fn expired_challenge_returns_401_without_cookie_or_session() {
        use axum::body::Body;
        use tower::ServiceExt;

        let auth = BrowserAuth::new(config()).unwrap();
        let challenge = auth.challenge(Instant::now() - CHALLENGE_TTL).unwrap();
        let code = auth.totp.as_ref().unwrap().generate_current().unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        let pool = crate::db::build_pool(file.path().to_str().unwrap()).unwrap();
        crate::db::init_schema(&pool).unwrap();
        let app =
            crate::build_router(AppState::new(pool.clone(), String::new()).with_browser(auth));
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/session")
            .header("origin", config().origin)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"challenge": challenge, "code": code}).to_string(),
            ))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(!response.headers().contains_key(SET_COOKIE));
        let count: i64 = pool
            .get()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM browser_sessions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn challenge_attempts_skew_leading_zero_and_replay() {
        let auth = BrowserAuth::new(config()).unwrap();
        let start = Instant::now();
        let totp = auth.totp.as_ref().unwrap();
        // RFC 6238's SHA1 vector at this timestamp has a leading zero at six digits.
        let timestamp = 1111111109;
        let code = totp.generate(timestamp);
        assert_eq!(code, "081804");
        let wrong = (0..1_000_000)
            .map(|n| format!("{n:06}"))
            .find(|s| !totp.check(s, timestamp))
            .unwrap();
        let challenge = auth.challenge(start).unwrap();
        for _ in 0..5 {
            assert_eq!(
                auth.consume_challenge(&challenge, &wrong, start, timestamp),
                Err(StatusCode::UNAUTHORIZED)
            );
        }
        assert_eq!(
            auth.consume_challenge(&challenge, &code, start, timestamp),
            Err(StatusCode::UNAUTHORIZED)
        );
        for offset in [-30i64, 0, 30] {
            let challenge = auth.challenge(start).unwrap();
            for _ in 0..4 {
                assert_eq!(
                    auth.consume_challenge(&challenge, &wrong, start, timestamp),
                    Err(StatusCode::UNAUTHORIZED)
                );
            }
            assert_eq!(
                auth.consume_challenge(
                    &challenge,
                    &code,
                    start,
                    (timestamp as i64 + offset) as u64
                ),
                Ok(timestamp / 30)
            );
            assert_eq!(
                auth.consume_challenge(&challenge, &code, start, timestamp),
                Err(StatusCode::UNAUTHORIZED)
            );
        }
    }

    #[test]
    fn challenge_step_matching_handles_clock_extremes() {
        let auth = BrowserAuth::new(config()).unwrap();
        let now = Instant::now();
        for timestamp in [0, u64::MAX] {
            let step = timestamp / 30;
            let code = auth.totp.as_ref().unwrap().generate(step * 30);
            let challenge = auth.challenge(now).unwrap();
            assert_eq!(
                auth.consume_challenge(&challenge, &code, now, timestamp),
                Ok(step)
            );
        }
        let timestamp = 1111111109;
        let code = auth.totp.as_ref().unwrap().generate(timestamp);
        for offset in [-60i64, 60] {
            let challenge = auth.challenge(now).unwrap();
            assert_eq!(
                auth.consume_challenge(&challenge, &code, now, (timestamp as i64 + offset) as u64),
                Err(StatusCode::UNAUTHORIZED)
            );
        }
    }

    #[test]
    fn ttl_and_totp_configuration_validation() {
        for ttl in [
            "",
            "0",
            "-1",
            "+2",
            "1.5",
            " 2",
            "18446744073709551615",
            "106751991167301",
        ] {
            assert!(BrowserAuth::from_values(
                None,
                Some(config().password_hash),
                Some(config().totp_secret),
                Some(config().origin),
                Environment::Production,
                Some(ttl.to_owned())
            )
            .is_err());
        }
        for secret in ["", "invalid!", "JBSWY3DPEHPK3PXP"] {
            let mut value = config();
            value.totp_secret = secret.to_owned();
            assert!(BrowserAuth::new(value).is_err());
        }
        for days in [0, u64::MAX, (i64::MAX as u64) / 86400] {
            let mut value = config();
            value.session_ttl_days = days;
            assert!(BrowserAuth::new(value).is_err());
        }
        let mut value = config();
        value.totp_secret = "A".repeat(64);
        assert!(BrowserAuth::new(value).is_ok());
        for days in [2, 366] {
            let auth = BrowserAuth::from_values(
                None,
                Some(config().password_hash),
                Some(config().totp_secret),
                Some(config().origin),
                Environment::Production,
                Some(days.to_string()),
            )
            .unwrap();
            assert_eq!(auth.session_ttl, days * 86400);
            assert!(auth
                .cookie("token", false, true)
                .unwrap()
                .to_str()
                .unwrap()
                .contains(&format!("Max-Age={}", days * 86400)));
        }
    }

    #[test]
    fn cookie_parser_accepts_other_cookies_but_rejects_ambiguous_session_values() {
        let token = random_token().unwrap();
        let mut headers = HeaderMap::new();
        headers.append("cookie", HeaderValue::from_static("other=value"));
        headers.append(
            "cookie",
            format!("memento_session={token}; another=value")
                .parse()
                .unwrap(),
        );
        assert!(session_token(&headers).ok() == Some(token.as_str()));
        headers.append(
            "cookie",
            format!("memento_session={token}").parse().unwrap(),
        );
        assert!(session_token(&headers).is_err());
        for value in [
            format!("memento_session={token}; memento_session={token}"),
            "memento_session=".to_owned(),
            "memento_session".to_owned(),
            format!("memento_session=\"{token}\""),
            format!("memento_session={token}x"),
            format!("memento_session={}", "z".repeat(64)),
            "other=value".to_owned(),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("cookie", value.parse().unwrap());
            assert!(session_token(&headers).is_err());
        }
    }

    #[test]
    fn origin_and_csrf_headers_must_be_single_and_session_bound() {
        let auth = BrowserAuth::new(config()).unwrap();
        let session = Session {
            token_hash: String::new(),
            csrf_token: random_token().unwrap(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("origin", config().origin.parse().unwrap());
        headers.insert("x-csrf-token", session.csrf_token.parse().unwrap());
        assert!(auth.check_csrf(&headers, &session).is_ok());
        headers.append("x-csrf-token", session.csrf_token.parse().unwrap());
        assert!(auth.check_csrf(&headers, &session).is_err());
        headers.insert("x-csrf-token", session.csrf_token.parse().unwrap());
        headers.append("origin", config().origin.parse().unwrap());
        assert!(auth.check_csrf(&headers, &session).is_err());
        headers.insert("origin", HeaderValue::from_static("https://attacker.test"));
        assert!(auth.check_csrf(&headers, &session).is_err());
    }
}
