//! Browser authentication contracts. All databases and credentials are synthetic.
//! Challenge expiry and deterministic TOTP vectors are also tested in browser.rs.

use std::{net::SocketAddr, sync::OnceLock};

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use memento::{
    browser::{BrowserAuth, BrowserConfig},
    config::Environment,
    db::DbPool,
    state::AppState,
};
use rand::{distributions::Alphanumeric, Rng};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use totp_rs::{Algorithm, Secret, TOTP};
use tower::ServiceExt;

const ORIGIN: &str = "https://diary.example.test";
const USERNAME: &str = "browser-test-user";
const API_KEY: &str = "synthetic-api-key";
const TTL: i64 = 7 * 24 * 60 * 60;
const TOTP_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

fn totp() -> TOTP {
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        Secret::Encoded(TOTP_SECRET.to_owned()).to_bytes().unwrap(),
    )
    .unwrap()
}

fn credentials() -> &'static (String, String) {
    static CREDENTIALS: OnceLock<(String, String)> = OnceLock::new();
    CREDENTIALS.get_or_init(|| {
        let password: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(40)
            .map(char::from)
            .collect();
        let hash = bcrypt::hash(&password, 4).expect("hash synthetic password");
        (password, hash)
    })
}

fn browser_config(origin: &str, hash: &str) -> BrowserConfig {
    BrowserConfig {
        username: USERNAME.to_owned(),
        password_hash: hash.to_owned(),
        totp_secret: TOTP_SECRET.to_owned(),
        origin: origin.to_owned(),
        environment: Environment::Production,
        session_ttl_days: 7,
    }
}

struct Fixture {
    app: Router,
    pool: DbPool,
    file: tempfile::NamedTempFile,
}

impl Fixture {
    fn new() -> Self {
        Self::with_auth(BrowserAuth::new(browser_config(ORIGIN, &credentials().1)).unwrap())
    }

    fn with_auth(auth: BrowserAuth) -> Self {
        let file = tempfile::NamedTempFile::new().unwrap();
        let pool = memento::db::build_pool(file.path().to_str().unwrap()).unwrap();
        memento::db::init_schema(&pool).unwrap();
        let app = memento::build_router(
            AppState::new(pool.clone(), API_KEY.to_owned()).with_browser(auth),
        );
        Self { app, pool, file }
    }

    fn reopen(&self, hash: &str) -> Router {
        let pool = memento::db::build_pool(self.file.path().to_str().unwrap()).unwrap();
        memento::db::init_schema(&pool).unwrap();
        let auth = BrowserAuth::new(browser_config(ORIGIN, hash)).unwrap();
        memento::build_router(AppState::new(pool, API_KEY.to_owned()).with_browser(auth))
    }

    fn session_count(&self) -> i64 {
        self.pool
            .get()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM browser_sessions", [], |r| r.get(0))
            .unwrap()
    }
}

fn request(method: &str, path: &str, headers: &[(&str, &str)], body: String) -> Request<Body> {
    let mut req = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    req.body(Body::from(body)).unwrap()
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

fn login_request(origin: Option<&str>, username: &str, password: &str) -> Request<Body> {
    let mut headers = vec![("content-type", "application/json")];
    if let Some(origin) = origin {
        headers.push(("origin", origin));
    }
    request(
        "POST",
        "/session",
        &headers,
        json!({"username": username, "password": password}).to_string(),
    )
}

fn no_store(headers: &HeaderMap) {
    assert!(headers
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|part| part.trim() == "no-store")));
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
    assert!(!headers.contains_key("access-control-allow-origin"));
    assert!(!headers.contains_key("access-control-allow-credentials"));
}

#[tokio::test]
async fn private_preflight_and_unsupported_methods_never_inherit_collection_cors() {
    let fixture = Fixture::new();
    for path in ["/session", "/private/diaries", "/api/diaries"] {
        let (status, headers, body) = send(
            &fixture.app,
            request(
                "OPTIONS",
                path,
                &[
                    ("origin", "https://other.test"),
                    ("access-control-request-method", "POST"),
                ],
                String::new(),
            ),
        )
        .await;
        assert!(status == StatusCode::UNAUTHORIZED || status == StatusCode::METHOD_NOT_ALLOWED);
        no_store(&headers);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["success"],
            false
        );
    }
    let (status, headers, _) =
        send(&fixture.app, request("GET", "/diary", &[], String::new())).await;
    assert_eq!(status, StatusCode::OK);
    no_store(&headers);
}

fn cookie_attributes(headers: &HeaderMap, secure: bool, clearing: bool) -> String {
    let cookies: Vec<_> = headers.get_all("set-cookie").iter().collect();
    assert_eq!(cookies.len(), 1);
    let cookie = cookies[0].to_str().unwrap();
    let parts: Vec<_> = cookie.split(';').map(str::trim).collect();
    assert!(parts[0].starts_with("memento_session="));
    assert!(parts.iter().any(|p| p.eq_ignore_ascii_case("HttpOnly")));
    assert!(parts
        .iter()
        .any(|p| p.eq_ignore_ascii_case("SameSite=Strict")));
    assert!(parts.contains(&"Path=/"));
    assert!(parts.contains(&if clearing {
        "Max-Age=0"
    } else {
        "Max-Age=604800"
    }));
    assert_eq!(
        parts.iter().any(|p| p.eq_ignore_ascii_case("Secure")),
        secure
    );
    assert!(!parts
        .iter()
        .any(|p| p.to_ascii_lowercase().starts_with("domain=")));
    parts[0].to_owned()
}

async fn login(app: &Router, origin: &str) -> (String, String) {
    login_at_step(app, origin, chrono::Utc::now().timestamp() as u64 / 30).await
}

async fn login_at_step(app: &Router, origin: &str, step: u64) -> (String, String) {
    let challenge = password_step(app, Some(origin), USERNAME, &credentials().0).await;
    let (status, headers, bytes) = send(
        app,
        otp_request(Some(origin), &challenge, &totp().generate(step * 30)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    no_store(&headers);
    let cookie = cookie_attributes(&headers, origin.starts_with("https://"), false);
    let token = cookie.strip_prefix("memento_session=").unwrap();
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["success"], true);
    assert_eq!(value["error"], Value::Null);
    assert_eq!(value["data"]["username"], USERNAME);
    let data = value["data"].as_object().unwrap();
    assert_eq!(data.len(), 2);
    let csrf = data["csrf_token"].as_str().unwrap().to_owned();
    assert!(!csrf.is_empty());
    assert!(csrf != token);
    assert!(!String::from_utf8_lossy(&bytes).contains(token));
    (cookie, csrf)
}

fn otp_request(origin: Option<&str>, challenge: &str, code: &str) -> Request<Body> {
    let mut headers = vec![("content-type", "application/json")];
    if let Some(origin) = origin {
        headers.push(("origin", origin));
    }
    request(
        "POST",
        "/session",
        &headers,
        json!({"challenge": challenge, "code": code}).to_string(),
    )
}

async fn password_step(
    app: &Router,
    origin: Option<&str>,
    username: &str,
    password: &str,
) -> String {
    let (status, headers, bytes) = send(app, login_request(origin, username, password)).await;
    assert_eq!(status, StatusCode::OK);
    no_store(&headers);
    assert!(!headers.contains_key("set-cookie"));
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["success"], true);
    assert_eq!(value["error"], Value::Null);
    assert_eq!(value["data"]["requires_totp"], true);
    let data = value["data"].as_object().unwrap();
    assert_eq!(data.len(), 2);
    let challenge = data["challenge"].as_str().unwrap().to_owned();
    assert_eq!(challenge.len(), 64);
    assert!(challenge.bytes().all(|b| b.is_ascii_hexdigit()));
    challenge
}

#[tokio::test]
async fn password_only_cannot_read_private_diaries_or_be_used_as_session() {
    let fixture = Fixture::new();
    let challenge = password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0).await;
    assert_eq!(fixture.session_count(), 0);
    let cookie = format!("memento_session={challenge}");
    for headers in [vec![], vec![("cookie", cookie.as_str())]] {
        for path in ["/session", "/private/diaries"] {
            assert_eq!(
                send(&fixture.app, request("GET", path, &headers, String::new()))
                    .await
                    .0,
                StatusCode::UNAUTHORIZED
            );
        }
    }
}

#[tokio::test]
async fn otp_wrong_attempts_retry_consumption_and_format_contract() {
    let fixture = Fixture::new();
    let challenge = password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0).await;
    for code in ["", "12345", "1234567", "abcdef"] {
        let (status, headers, _) =
            send(&fixture.app, otp_request(Some(ORIGIN), &challenge, code)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!headers.contains_key("set-cookie"));
    }
    let generator = totp();
    let now = chrono::Utc::now().timestamp() as u64;
    // Exclude adjacent future windows as well to avoid a boundary-sensitive wrong code.
    let wrong = (0..1_000_000)
        .map(|n| format!("{n:06}"))
        .find(|s| !generator.check(s, now) && !generator.check(s, now + 30))
        .unwrap();
    for _ in 0..4 {
        let (status, headers, _) =
            send(&fixture.app, otp_request(Some(ORIGIN), &challenge, &wrong)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!headers.contains_key("set-cookie"));
    }
    let code = generator.generate(now);
    assert_eq!(
        send(&fixture.app, otp_request(Some(ORIGIN), &challenge, &code))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        send(&fixture.app, otp_request(Some(ORIGIN), &challenge, &code))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let exhausted = password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0).await;
    for _ in 0..5 {
        assert_eq!(
            send(&fixture.app, otp_request(Some(ORIGIN), &exhausted, &wrong))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        send(
            &fixture.app,
            otp_request(
                Some(ORIGIN),
                &exhausted,
                &generator.generate((now / 30 + 1) * 30)
            )
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &fixture.app,
            otp_request(Some(ORIGIN), &"0".repeat(64), &code)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&fixture.app, otp_request(Some(ORIGIN), "bad", &code))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(fixture.session_count(), 1);
}

#[tokio::test]
async fn concurrent_challenge_consumption_creates_exactly_one_session() {
    let fixture = Fixture::new();
    let challenge = password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0).await;
    let code = totp().generate_current().unwrap();
    let (first, second) = tokio::join!(
        send(&fixture.app, otp_request(Some(ORIGIN), &challenge, &code)),
        send(&fixture.app, otp_request(Some(ORIGIN), &challenge, &code)),
    );
    let mut statuses = [first.0, second.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::UNAUTHORIZED]);
    assert_eq!(
        [first.1, second.1]
            .iter()
            .filter(|h| h.contains_key("set-cookie"))
            .count(),
        1
    );
    assert_eq!(fixture.session_count(), 1);
}

#[tokio::test]
async fn different_challenges_with_same_totp_concurrently_create_only_one_session() {
    let fixture = Fixture::new();
    let first = password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0).await;
    let second = password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0).await;
    assert_ne!(first, second);
    let code = totp().generate_current().unwrap();
    let (first, second) = tokio::join!(
        send(&fixture.app, otp_request(Some(ORIGIN), &first, &code)),
        send(&fixture.app, otp_request(Some(ORIGIN), &second, &code)),
    );
    let mut statuses = [first.0, second.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::UNAUTHORIZED]);
    assert_eq!(
        [first.1, second.1]
            .iter()
            .filter(|h| h.contains_key("set-cookie"))
            .count(),
        1
    );
    assert_eq!(fixture.session_count(), 1);
}

#[tokio::test]
async fn used_totp_survives_logout_and_auth_recreation_but_next_step_succeeds() {
    let fixture = Fixture::new();
    let step = chrono::Utc::now().timestamp() as u64 / 30;
    let (cookie, csrf) = login_at_step(&fixture.app, ORIGIN, step).await;
    assert_eq!(
        send(
            &fixture.app,
            request(
                "DELETE",
                "/session",
                &[
                    ("cookie", &cookie),
                    ("origin", ORIGIN),
                    ("x-csrf-token", &csrf)
                ],
                String::new()
            )
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let rebuilt = fixture.reopen(&credentials().1);
    let challenge = password_step(&rebuilt, Some(ORIGIN), USERNAME, &credentials().0).await;
    let (status, headers, _) = send(
        &rebuilt,
        otp_request(Some(ORIGIN), &challenge, &totp().generate(step * 30)),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!headers.contains_key("set-cookie"));
    assert_eq!(fixture.session_count(), 0);
    login_at_step(&rebuilt, ORIGIN, step + 1).await;
    assert_eq!(fixture.session_count(), 1);
    let stored: i64 = fixture
        .pool
        .get()
        .unwrap()
        .query_row("SELECT last_used_step FROM browser_totp_state", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(stored, (step + 1) as i64);
    let challenge = password_step(&rebuilt, Some(ORIGIN), USERNAME, &credentials().0).await;
    assert_eq!(
        send(
            &rebuilt,
            otp_request(Some(ORIGIN), &challenge, &totp().generate(step * 30))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn failed_session_insert_rolls_back_totp_insert_and_update() {
    for existing_state in [false, true] {
        let fixture = Fixture::new();
        let step = chrono::Utc::now().timestamp() as u64 / 30;
        if existing_state {
            login_at_step(&fixture.app, ORIGIN, step).await;
        }
        let next_step = step + u64::from(existing_state);
        fixture
            .pool
            .get()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_session_insert BEFORE INSERT ON browser_sessions
             BEGIN SELECT RAISE(ABORT, 'synthetic-insert-failure'); END;",
            )
            .unwrap();
        let challenge = password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0).await;
        let code = totp().generate(next_step * 30);
        let (status, headers, body) =
            send(&fixture.app, otp_request(Some(ORIGIN), &challenge, &code)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!headers.contains_key("set-cookie"));
        assert!(!String::from_utf8_lossy(&body).contains("synthetic-insert-failure"));
        assert_eq!(fixture.session_count(), i64::from(existing_state));
        let stored: Option<i64> = fixture
            .pool
            .get()
            .unwrap()
            .query_row(
                "SELECT MAX(last_used_step) FROM browser_totp_state",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, existing_state.then_some(step as i64));
        fixture
            .pool
            .get()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_session_insert")
            .unwrap();
        // A failed transaction consumes its challenge, but not the TOTP step.
        assert_eq!(
            send(&fixture.app, otp_request(Some(ORIGIN), &challenge, &code))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let rebuilt = fixture.reopen(&credentials().1);
        login_at_step(&rebuilt, ORIGIN, next_step).await;
        assert_eq!(fixture.session_count(), i64::from(existing_state) + 1);
        let stored: i64 = fixture
            .pool
            .get()
            .unwrap()
            .query_row("SELECT last_used_step FROM browser_totp_state", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stored, next_step as i64);
    }
}

#[tokio::test]
async fn concurrent_password_verification_is_not_rejected_by_a_global_semaphore() {
    let fixture = Fixture::new();
    let (first, second) = tokio::join!(
        password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0),
        password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0),
    );
    assert_ne!(first, second);
    assert_eq!(fixture.session_count(), 0);
}

#[tokio::test]
async fn production_second_step_requires_origin_without_consuming_challenge_on_403() {
    let fixture = Fixture::new();
    let challenge = password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0).await;
    let code = totp().generate_current().unwrap();
    for origin in [None, Some("https://attacker.test")] {
        let (status, headers, _) = send(&fixture.app, otp_request(origin, &challenge, &code)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!headers.contains_key("set-cookie"));
    }
    assert_eq!(fixture.session_count(), 0);
    assert_eq!(
        send(&fixture.app, otp_request(Some(ORIGIN), &challenge, &code))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn bcrypt_external_variants_and_utf8_byte_boundaries() {
    // Published cross-language bcrypt vectors; no local credentials are read.
    for (password, hash) in [
        (
            "password",
            "$2a$04$UuTkLRZZ6QofpDOlMz32MuuxEHA43WOemOYHPz6.SjsVsyO1tDU96",
        ),
        (
            "correctbatteryhorsestapler",
            "$2b$04$EGdrhbKUv8Oc9vGiXX0HQOxSg445d458Muh7DAHskb6QbtCvdxcie",
        ),
        (
            "password",
            "$2y$04$UuTkLRZZ6QofpDOlMz32MuuxEHA43WOemOYHPz6.SjsVsyO1tDU96",
        ),
    ] {
        let fixture = Fixture::with_auth(BrowserAuth::new(browser_config(ORIGIN, hash)).unwrap());
        let challenge = password_step(&fixture.app, Some(ORIGIN), USERNAME, password).await;
        assert_eq!(
            send(
                &fixture.app,
                otp_request(
                    Some(ORIGIN),
                    &challenge,
                    &totp().generate_current().unwrap()
                )
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    for password in ["a".to_owned(), "x".repeat(72), "\u{e9}".repeat(36)] {
        let hash = bcrypt::hash(&password, 4).unwrap();
        let fixture = Fixture::with_auth(BrowserAuth::new(browser_config(ORIGIN, &hash)).unwrap());
        password_step(&fixture.app, Some(ORIGIN), USERNAME, &password).await;
        for invalid in [
            String::new(),
            format!("{}x", "x".repeat(72)),
            format!("{}x", "\u{e9}".repeat(36)),
        ] {
            let (status, headers, _) = send(
                &fixture.app,
                login_request(Some(ORIGIN), USERNAME, &invalid),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(!headers.contains_key("set-cookie"));
        }
    }
}

#[tokio::test]
async fn configured_two_day_ttl_controls_cookie_and_persisted_expiry() {
    let mut config = browser_config(ORIGIN, &credentials().1);
    config.session_ttl_days = 2;
    let fixture = Fixture::with_auth(BrowserAuth::new(config).unwrap());
    let challenge = password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0).await;
    let before = chrono::Utc::now().timestamp();
    let (status, headers, _) = send(
        &fixture.app,
        otp_request(
            Some(ORIGIN),
            &challenge,
            &totp().generate_current().unwrap(),
        ),
    )
    .await;
    let after = chrono::Utc::now().timestamp();
    assert_eq!(status, StatusCode::OK);
    assert!(headers["set-cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age=172800"));
    let expiry: i64 = fixture
        .pool
        .get()
        .unwrap()
        .query_row("SELECT expires_at FROM browser_sessions", [], |r| r.get(0))
        .unwrap();
    assert!((before + 172800..=after + 172800).contains(&expiry));
}

#[tokio::test]
async fn origin_changes_preserve_sessions_but_totp_and_username_changes_revoke_them() {
    let fixture = Fixture::new();
    let (cookie, _) = login(&fixture.app, ORIGIN).await;
    for field in 0..3 {
        let mut config = browser_config(ORIGIN, &credentials().1);
        match field {
            0 => config.origin = "http://new.test".to_owned(),
            1 => config.username.push('x'),
            _ => config.totp_secret = "A".repeat(32),
        }
        let app = memento::build_router(
            AppState::new(fixture.pool.clone(), API_KEY.to_owned())
                .with_browser(BrowserAuth::new(config).unwrap()),
        );
        assert_eq!(
            send(
                &app,
                request("GET", "/session", &[("cookie", &cookie)], String::new())
            )
            .await
            .0,
            if field == 0 {
                StatusCode::OK
            } else {
                StatusCode::UNAUTHORIZED
            }
        );
    }
}

#[tokio::test]
async fn login_cookie_and_get_session_contract() {
    let fixture = Fixture::new();
    let (cookie, csrf) = login(&fixture.app, ORIGIN).await;
    let (status, headers, bytes) = send(
        &fixture.app,
        request("GET", "/session", &[("cookie", &cookie)], String::new()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    no_store(&headers);
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        value
            == json!({
                "success": true,
                "data": {"username": USERNAME, "csrf_token": csrf},
                "error": null,
            })
    );
    let (status, _, _) = send(
        &fixture.app,
        request(
            "GET",
            "/private/diaries",
            &[("cookie", &cookie)],
            String::new(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn session_schema_digest_fixed_ttl_and_router_recreation() {
    let fixture = Fixture::new();
    let before = chrono::Utc::now().timestamp();
    let (cookie, csrf) = login(&fixture.app, ORIGIN).await;
    let after = chrono::Utc::now().timestamp();
    let token = cookie.strip_prefix("memento_session=").unwrap();
    let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
    let conn = fixture.pool.get().unwrap();
    let columns: Vec<(String, String, i64, i64)> = conn
        .prepare("PRAGMA table_info(browser_sessions)")
        .unwrap()
        .query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get(3)?, r.get(5)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(columns.len(), 4);
    for name in ["token_hash", "csrf_token", "credential_hash", "expires_at"] {
        let column = columns.iter().find(|c| c.0 == name).unwrap();
        assert_eq!(
            column.1.to_ascii_uppercase(),
            if name == "expires_at" {
                "INTEGER"
            } else {
                "TEXT"
            }
        );
        if name == "token_hash" {
            assert_eq!(column.3, 1);
        } else {
            assert_eq!(column.2, 1);
        }
    }
    let (stored, stored_csrf, credential, expiry): (String, String, String, i64) = conn
        .query_row(
            "SELECT token_hash, csrf_token, credential_hash, expires_at FROM browser_sessions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert!(stored == digest);
    assert!(stored != token);
    assert!(stored_csrf == csrf);
    assert!(!credential.is_empty());
    assert!(![&stored, &stored_csrf, &credential]
        .iter()
        .any(|v| v.contains(token)));
    assert!((before + TTL..=after + TTL).contains(&expiry));
    // Move expiry back so a sliding renewal cannot hide within the same second.
    let expiry = expiry - 60;
    conn.execute(
        "UPDATE browser_sessions SET expires_at = ?1 WHERE token_hash = ?2",
        rusqlite::params![expiry, digest],
    )
    .unwrap();
    drop(conn);
    let rebuilt = fixture.reopen(&credentials().1);
    let (status, _, bytes) = send(
        &rebuilt,
        request("GET", "/session", &[("cookie", &cookie)], String::new()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value["data"]["csrf_token"].as_str() == Some(csrf.as_str()));
    let unchanged: i64 = fixture
        .pool
        .get()
        .unwrap()
        .query_row(
            "SELECT expires_at FROM browser_sessions WHERE token_hash = ?1",
            [&digest],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unchanged, expiry, "session reads must not extend TTL");
}

#[tokio::test]
async fn separate_logins_have_distinct_tokens_and_csrf() {
    let fixture = Fixture::new();
    let step = chrono::Utc::now().timestamp() as u64 / 30;
    let first = login_at_step(&fixture.app, ORIGIN, step).await;
    let second = login_at_step(&fixture.app, ORIGIN, step + 1).await;
    assert!(first.0 != second.0);
    assert!(first.1 != second.1);
    let (status, _, _) = send(
        &fixture.app,
        request(
            "DELETE",
            "/session",
            &[
                ("cookie", &first.0),
                ("origin", ORIGIN),
                ("x-csrf-token", &second.1),
            ],
            String::new(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn anonymous_session_and_all_private_methods_reject_api_key_substitution() {
    let fixture = Fixture::new();
    for headers in [vec![], vec![("x-api-key", API_KEY)]] {
        let (status, response_headers, _) = send(
            &fixture.app,
            request("GET", "/session", &headers, String::new()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        no_store(&response_headers);
        for path in ["/private/diaries", "/private/diaries/1"] {
            for method in [
                "GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "TRACE", "CONNECT",
            ] {
                let (status, _, _) =
                    send(&fixture.app, request(method, path, &headers, String::new())).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
            }
        }
    }
}

#[tokio::test]
async fn wrong_credentials_have_identical_public_rejection() {
    let fixture = Fixture::new();
    let mut responses = Vec::new();
    for (username, password) in [
        (USERNAME, "not-the-generated-password"),
        ("other-user", credentials().0.as_str()),
        ("other-user", "not-the-generated-password"),
    ] {
        let (status, headers, bytes) = send(
            &fixture.app,
            login_request(Some(ORIGIN), username, password),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!headers.contains_key("set-cookie"));
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["success"], false);
        assert_eq!(value["data"], Value::Null);
        assert!(value["error"].is_string());
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains(&credentials().0));
        assert!(!text.contains(&credentials().1));
        responses.push(bytes);
    }
    assert!(responses.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(fixture.session_count(), 0);
}

#[tokio::test]
async fn disabled_login_is_service_unavailable() {
    let fixture = Fixture::with_auth(BrowserAuth::disabled());
    let (status, headers, _) = send(
        &fixture.app,
        login_request(Some(ORIGIN), USERNAME, &credentials().0),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!headers.contains_key("set-cookie"));
}

#[tokio::test]
async fn login_requires_exact_origin_even_with_api_key_or_proxy_headers() {
    for origin in [
        None,
        Some("null"),
        Some("http://diary.example.test"),
        Some("https://diary.example.test/"),
        Some("https://diary.example.test:443"),
        Some("https://diary.example.test.attacker.test"),
        Some("https://attacker.test"),
    ] {
        let fixture = Fixture::new();
        let mut req = login_request(origin, USERNAME, &credentials().0);
        req.headers_mut()
            .insert("x-api-key", API_KEY.parse().unwrap());
        req.headers_mut()
            .insert("x-forwarded-host", "diary.example.test".parse().unwrap());
        req.headers_mut()
            .insert("x-forwarded-proto", "https".parse().unwrap());
        let (status, headers, _) = send(&fixture.app, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!headers.contains_key("set-cookie"));
        assert_eq!(fixture.session_count(), 0);
    }
}

#[tokio::test]
async fn logout_and_private_mutations_require_origin_and_session_bound_csrf() {
    let fixture = Fixture::new();
    let (cookie, csrf) = login(&fixture.app, ORIGIN).await;
    for (method, path) in [
        ("DELETE", "/session"),
        ("POST", "/private/diaries"),
        ("PATCH", "/private/diaries/1"),
        ("DELETE", "/private/diaries/1"),
    ] {
        for (origin, csrf_header) in [
            (None, Some(csrf.as_str())),
            (Some(ORIGIN), None),
            (Some("https://attacker.test"), Some(csrf.as_str())),
            (Some(ORIGIN), Some("forged-csrf")),
            (None, None),
        ] {
            let mut headers = vec![
                ("cookie", cookie.as_str()),
                ("content-type", "application/json"),
                ("x-api-key", API_KEY),
            ];
            if let Some(origin) = origin {
                headers.push(("origin", origin));
            }
            if let Some(csrf) = csrf_header {
                headers.push(("x-csrf-token", csrf));
            }
            let (status, _, _) = send(
                &fixture.app,
                request(method, path, &headers, "{}".to_owned()),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        }
    }
    let (status, _, _) = send(
        &fixture.app,
        request("GET", "/session", &[("cookie", &cookie)], String::new()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "rejected logout must not revoke session"
    );
}

#[tokio::test]
async fn logout_deletes_persisted_session_clears_cookie_and_rejects_replay() {
    let fixture = Fixture::new();
    let (cookie, csrf) = login(&fixture.app, ORIGIN).await;
    let (status, headers, bytes) = send(
        &fixture.app,
        request(
            "DELETE",
            "/session",
            &[
                ("cookie", &cookie),
                ("origin", ORIGIN),
                ("x-csrf-token", &csrf),
            ],
            String::new(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(bytes.is_empty());
    cookie_attributes(&headers, true, true);
    assert_eq!(fixture.session_count(), 0);
    let rebuilt = fixture.reopen(&credentials().1);
    for app in [&fixture.app, &rebuilt] {
        for (method, path) in [
            ("GET", "/session"),
            ("GET", "/private/diaries"),
            ("DELETE", "/session"),
        ] {
            let (status, _, _) = send(
                app,
                request(
                    method,
                    path,
                    &[
                        ("cookie", &cookie),
                        ("origin", ORIGIN),
                        ("x-csrf-token", &csrf),
                    ],
                    String::new(),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
    }
}

#[tokio::test]
async fn expired_session_is_rejected_for_reads_and_logout() {
    let fixture = Fixture::new();
    let (cookie, csrf) = login(&fixture.app, ORIGIN).await;
    fixture
        .pool
        .get()
        .unwrap()
        .execute(
            "UPDATE browser_sessions SET expires_at = ?1",
            [chrono::Utc::now().timestamp()],
        )
        .unwrap();
    for (method, path) in [
        ("GET", "/session"),
        ("GET", "/private/diaries"),
        ("DELETE", "/session"),
    ] {
        let (status, _, _) = send(
            &fixture.app,
            request(
                method,
                path,
                &[
                    ("cookie", &cookie),
                    ("origin", ORIGIN),
                    ("x-csrf-token", &csrf),
                ],
                String::new(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn tampered_malformed_and_digest_as_cookie_are_rejected() {
    let fixture = Fixture::new();
    let (cookie, _) = login(&fixture.app, ORIGIN).await;
    let token = cookie.strip_prefix("memento_session=").unwrap();
    let mut tampered = token.to_owned();
    tampered.replace_range(..1, if token.starts_with('0') { "1" } else { "0" });
    let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
    for token in [
        tampered,
        digest,
        String::new(),
        "z".repeat(64),
        "a".repeat(63),
        "a".repeat(65),
    ] {
        let invalid_cookie = format!("memento_session={token}");
        for path in ["/session", "/private/diaries"] {
            let (status, _, _) = send(
                &fixture.app,
                request(
                    "GET",
                    path,
                    &[("cookie", &invalid_cookie), ("x-api-key", API_KEY)],
                    String::new(),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
    }
    let combined = format!("unrelated=value; {cookie}; another=value");
    let (status, _, _) = send(
        &fixture.app,
        request("GET", "/session", &[("cookie", &combined)], String::new()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn changing_password_hash_invalidates_old_sessions_even_for_same_password() {
    let fixture = Fixture::new();
    let (cookie, _) = login(&fixture.app, ORIGIN).await;
    let rotated_hash = bcrypt::hash(&credentials().0, 4).unwrap();
    assert!(rotated_hash != credentials().1);
    let rotated = fixture.reopen(&rotated_hash);
    for path in ["/session", "/private/diaries"] {
        let (status, _, _) = send(
            &rotated,
            request("GET", path, &[("cookie", &cookie)], String::new()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    login(&rotated, ORIGIN).await;
}

#[tokio::test]
async fn missing_session_table_fails_closed_without_public_database_details() {
    let fixture = Fixture::new();
    let step = chrono::Utc::now().timestamp() as u64 / 30;
    let (cookie, csrf) = login_at_step(&fixture.app, ORIGIN, step).await;
    let challenge = password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0).await;
    fixture
        .pool
        .get()
        .unwrap()
        .execute_batch("DROP TABLE browser_sessions")
        .unwrap();
    let mut requests = vec![otp_request(
        Some(ORIGIN),
        &challenge,
        &totp().generate((step + 1) * 30),
    )];
    for (method, path) in [
        ("GET", "/session"),
        ("GET", "/private/diaries"),
        ("DELETE", "/session"),
    ] {
        requests.push(request(
            method,
            path,
            &[
                ("cookie", &cookie),
                ("origin", ORIGIN),
                ("x-csrf-token", &csrf),
            ],
            String::new(),
        ));
    }
    for req in requests {
        let (status, headers, bytes) = send(&fixture.app, req).await;
        assert!(
            status.is_server_error(),
            "DB failure must not become success or fallback"
        );
        assert!(!headers.contains_key("set-cookie"));
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["success"], false);
        let text = String::from_utf8_lossy(&bytes);
        for secret in [
            "browser_sessions",
            "no such table",
            credentials().0.as_str(),
            credentials().1.as_str(),
            cookie.strip_prefix("memento_session=").unwrap(),
        ] {
            assert!(!text.contains(secret));
        }
    }
}

#[tokio::test]
async fn missing_totp_state_fails_closed_on_login_without_affecting_session_reads() {
    let fixture = Fixture::new();
    let step = chrono::Utc::now().timestamp() as u64 / 30;
    let (cookie, _) = login_at_step(&fixture.app, ORIGIN, step).await;
    fixture
        .pool
        .get()
        .unwrap()
        .execute_batch("DROP TABLE browser_totp_state")
        .unwrap();
    let challenge = password_step(&fixture.app, Some(ORIGIN), USERNAME, &credentials().0).await;
    let (status, headers, bytes) = send(
        &fixture.app,
        otp_request(Some(ORIGIN), &challenge, &totp().generate((step + 1) * 30)),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!headers.contains_key("set-cookie"));
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("browser_totp_state"));
    assert!(!text.contains("no such table"));
    assert_eq!(fixture.session_count(), 1);
    assert_eq!(
        send(
            &fixture.app,
            request("GET", "/session", &[("cookie", &cookie)], String::new())
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn external_diary_api_requires_key_not_cookie_and_favorites_keep_existing_policy() {
    let fixture = Fixture::new();
    let (cookie, csrf) = login(&fixture.app, ORIGIN).await;
    for (method, path) in [
        ("GET", "/api/diaries"),
        ("POST", "/api/diaries"),
        ("GET", "/api/diaries/1"),
        ("PATCH", "/api/diaries/1"),
        ("DELETE", "/api/diaries/1"),
    ] {
        for key in [None, Some("wrong-key")] {
            let mut headers = vec![
                ("cookie", cookie.as_str()),
                ("origin", ORIGIN),
                ("x-csrf-token", csrf.as_str()),
            ];
            if let Some(key) = key {
                headers.push(("x-api-key", key));
            }
            let (status, _, _) =
                send(&fixture.app, request(method, path, &headers, String::new())).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        }
    }
    for path in ["/api/diaries", "/api/auth/verify"] {
        let (status, _, _) = send(
            &fixture.app,
            request("GET", path, &[("x-api-key", API_KEY)], String::new()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _, _) = send(
        &fixture.app,
        request("GET", "/api/favorites", &[], String::new()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for path in ["/api/favorites", "/api/auth/verify"] {
        let method = if path == "/api/favorites" {
            "POST"
        } else {
            "GET"
        };
        let (status, _, _) = send(
            &fixture.app,
            request(
                method,
                path,
                &[
                    ("cookie", &cookie),
                    ("origin", ORIGIN),
                    ("x-csrf-token", &csrf),
                    ("content-type", "application/json"),
                ],
                "{}".to_owned(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn malformed_json_and_wrong_field_types_are_bad_request() {
    for body in [
        "{",
        "null",
        "[]",
        "{}",
        r#"{"username":1,"password":true}"#,
        r#"{"username":"browser-test-user"}"#,
        r#"{"challenge":"abc","code":123456}"#,
        r#"{"challenge":"abc","code":"123456","unexpected":true}"#,
        r#"{"username":"browser-test-user","password":"test","challenge":"abc","code":"123456"}"#,
    ] {
        let fixture = Fixture::new();
        let (status, headers, _) = send(
            &fixture.app,
            request(
                "POST",
                "/session",
                &[("origin", ORIGIN), ("content-type", "application/json")],
                body.to_owned(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!headers.contains_key("set-cookie"));
        assert_eq!(fixture.session_count(), 0);
    }
}

#[tokio::test]
async fn login_rejects_non_json_media_types() {
    for content_type in [
        None,
        Some("text/plain"),
        Some("application/x-www-form-urlencoded"),
    ] {
        let fixture = Fixture::new();
        let mut headers = vec![("origin", ORIGIN)];
        if let Some(content_type) = content_type {
            headers.push(("content-type", content_type));
        }
        let (status, response_headers, _) = send(
            &fixture.app,
            request(
                "POST",
                "/session",
                &headers,
                json!({"username": USERNAME, "password": credentials().0}).to_string(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(!response_headers.contains_key("set-cookie"));
        assert_eq!(fixture.session_count(), 0);
    }
}

#[tokio::test]
async fn login_body_limit_accepts_8192_bytes_and_rejects_8193_without_content_length() {
    for size in [8192, 8193] {
        let fixture = Fixture::new();
        let mut body = json!({"username": USERNAME, "password": credentials().0}).to_string();
        body.push_str(&" ".repeat(size - body.len()));
        let (status, headers, _) = send(
            &fixture.app,
            request(
                "POST",
                "/session",
                &[("origin", ORIGIN), ("content-type", "application/json")],
                body,
            ),
        )
        .await;
        assert_eq!(
            status,
            if size == 8192 {
                StatusCode::OK
            } else {
                StatusCode::PAYLOAD_TOO_LARGE
            }
        );
        if size > 8192 {
            assert!(!headers.contains_key("set-cookie"));
            assert_eq!(fixture.session_count(), 0);
        }
    }
}

#[tokio::test]
async fn password_requests_are_not_limited_to_ten_attempts() {
    let fixture = Fixture::new();
    for attempt in 0..11 {
        let username = format!("unknown-user-{attempt}");
        let mut req = if attempt == 10 {
            login_request(Some(ORIGIN), USERNAME, &credentials().0)
        } else {
            login_request(Some(ORIGIN), &username, &credentials().0)
        };
        req.headers_mut().insert(
            "x-forwarded-for",
            format!("198.51.100.{}", attempt + 1).parse().unwrap(),
        );
        req.headers_mut().insert(
            "x-real-ip",
            format!("203.0.113.{}", attempt + 1).parse().unwrap(),
        );
        req.headers_mut().insert(
            "forwarded",
            format!("for=192.0.2.{}", attempt + 1).parse().unwrap(),
        );
        let (status, headers, _) = send(&fixture.app, req).await;
        assert_eq!(
            status,
            if attempt < 10 {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::OK
            }
        );
        assert!(!headers.contains_key("set-cookie"));
    }
    assert_eq!(fixture.session_count(), 0);
}

#[tokio::test]
async fn tcp_peers_and_forwarded_headers_do_not_apply_application_quotas() {
    let fixture = Fixture::new();
    for attempt in 0..11 {
        let username = format!("unknown-peer-user-{attempt}");
        let mut req = login_request(Some(ORIGIN), &username, &credentials().0);
        let peer: SocketAddr = format!("198.51.100.10:{}", 40000 + attempt)
            .parse()
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(peer));
        req.headers_mut().insert(
            "x-forwarded-for",
            format!("203.0.113.{}", attempt + 1).parse().unwrap(),
        );
        req.headers_mut().insert(
            "x-real-ip",
            format!("203.0.113.{}", attempt + 1).parse().unwrap(),
        );
        req.headers_mut().insert(
            "forwarded",
            format!("for=192.0.2.{}", attempt + 1).parse().unwrap(),
        );
        let (status, headers, _) = send(&fixture.app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!headers.contains_key("set-cookie"));
    }
    let mut req = login_request(Some(ORIGIN), USERNAME, &credentials().0);
    req.extensions_mut().insert(ConnectInfo(
        "[2001:db8::20]:45000".parse::<SocketAddr>().unwrap(),
    ));
    req.headers_mut()
        .insert("x-forwarded-for", "198.51.100.10".parse().unwrap());
    let (status, headers, _) = send(&fixture.app, req).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "one exhausted peer must not lock out another peer"
    );
    assert!(!headers.contains_key("set-cookie"));
    assert_eq!(fixture.session_count(), 0);

    let mut req = login_request(Some(ORIGIN), USERNAME, &credentials().0);
    req.extensions_mut().insert(ConnectInfo(
        "198.51.100.10:60000".parse::<SocketAddr>().unwrap(),
    ));
    let (status, _, _) = send(&fixture.app, req).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn login_rejects_unknown_json_fields_without_creating_session() {
    let fixture = Fixture::new();
    let (status, headers, bytes) = send(
        &fixture.app,
        request(
            "POST",
            "/session",
            &[("origin", ORIGIN), ("content-type", "application/json")],
            json!({
                "username": USERNAME,
                "password": credentials().0,
                "unexpected_field": "must-not-be-echoed",
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!headers.contains_key("set-cookie"));
    assert_eq!(fixture.session_count(), 0);
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["success"], false);
    assert_eq!(value["data"], Value::Null);
    assert_eq!(value["error"], "invalid login request");
}

#[tokio::test]
async fn explicit_http_is_supported_without_secure_cookie_and_still_locks_origin() {
    for origin in [
        "http://192.168.1.10:23457",
        "http://diary.example.test",
        "http://localhost:23457",
        "http://127.0.0.1:23457",
        "http://[::1]:23457",
    ] {
        let fixture =
            Fixture::with_auth(BrowserAuth::new(browser_config(origin, &credentials().1)).unwrap());
        login(&fixture.app, origin).await;
        let (status, _, _) = send(
            &fixture.app,
            login_request(Some("http://other.test"), USERNAME, &credentials().0),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}

#[test]
fn browser_config_rejects_invalid_hash_and_accepts_plaintext_origin() {
    assert!(BrowserAuth::new(browser_config(ORIGIN, "invalid-bcrypt-hash")).is_err());
    for origin in [
        "http://diary.example.test",
        "http://localhost.attacker.test",
        "http://0.0.0.0:23457",
    ] {
        assert!(BrowserAuth::new(browser_config(origin, &credentials().1)).is_ok());
    }
}

#[tokio::test]
async fn development_login_crud_and_logout_without_origin_or_host() {
    for configured_origin in ["", ORIGIN] {
        let mut config = browser_config(configured_origin, &credentials().1);
        config.environment = Environment::Development;
        let fixture = Fixture::with_auth(BrowserAuth::new(config).unwrap());
        let challenge = password_step(&fixture.app, None, USERNAME, &credentials().0).await;
        let (status, headers, bytes) = send(
            &fixture.app,
            otp_request(None, &challenge, &totp().generate_current().unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        no_store(&headers);
        let raw_cookie = headers["set-cookie"].to_str().unwrap();
        assert!(raw_cookie.contains("HttpOnly"));
        assert!(raw_cookie.contains("SameSite=Lax"));
        assert!(!raw_cookie.contains("Secure"));
        let cookie = raw_cookie.split(';').next().unwrap().to_owned();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let csrf = value["data"]["csrf_token"].as_str().unwrap();
        for path in ["/session", "/private/diaries"] {
            let (status, _, _) = send(
                &fixture.app,
                request("GET", path, &[("cookie", &cookie)], String::new()),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
        let headers = [
            ("cookie", cookie.as_str()),
            ("x-csrf-token", csrf),
            ("content-type", "application/json"),
        ];
        let (status, _, bytes) = send(
            &fixture.app,
            request(
                "POST",
                "/private/diaries",
                &headers,
                json!({"content":"synthetic development diary"}).to_string(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let created: Value = serde_json::from_slice(&bytes).unwrap();
        let path = format!("/private/diaries/{}", created["data"]["id"]);
        for (method, version, expected) in [
            ("PATCH", "\"1\"", StatusCode::OK),
            ("DELETE", "\"2\"", StatusCode::NO_CONTENT),
        ] {
            let mut headers = headers.to_vec();
            headers.push(("if-match", version));
            let (status, _, _) = send(
                &fixture.app,
                request(
                    method,
                    &path,
                    &headers,
                    json!({"content":"synthetic edited"}).to_string(),
                ),
            )
            .await;
            assert_eq!(status, expected);
        }
        // Development still requires session-bound CSRF on every unsafe request.
        for (method, path) in [
            ("POST", "/private/diaries"),
            ("PATCH", &*path),
            ("DELETE", &*path),
            ("DELETE", "/session"),
        ] {
            let mut req = request(method, path, &headers, "{}".to_owned());
            req.headers_mut().remove("x-csrf-token");
            let (status, _, _) = send(&fixture.app, req).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
        let (status, response_headers, _) = send(
            &fixture.app,
            request("DELETE", "/session", &headers, String::new()),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let cleared = response_headers["set-cookie"].to_str().unwrap();
        assert!(cleared.contains("Max-Age=0"));
        assert!(cleared.contains("HttpOnly"));
        assert!(cleared.contains("SameSite=Lax"));
        assert!(!cleared.contains("Secure"));
        let (status, _, _) = send(
            &fixture.app,
            request("GET", "/session", &[("cookie", &cookie)], String::new()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn production_rejects_ambiguous_origins_and_ignores_host() {
    let fixture = Fixture::new();
    let origin = ORIGIN;
    let host = "diary.example.test";
    let mut cases = vec![
        vec![],
        vec![("host", host)],
        vec![("origin", origin), ("origin", origin), ("host", host)],
    ];
    for invalid in [
        "null",
        "ftp://192.168.1.10:23457",
        "ws://192.168.1.10:23457",
        "http://other.test:23457",
        "http://192.168.1.10",
        "http://192.168.1.10:23457/",
        "http://user@192.168.1.10:23457",
        "http://192.168.1.10:23457?q",
        "http://192.168.1.10:23457#f",
        "http://192.168.1.10:23457 http://other.test",
    ] {
        cases.push(vec![("origin", invalid), ("host", host)]);
    }
    for mut headers in cases {
        headers.extend([
            ("content-type", "application/json"),
            ("x-forwarded-host", host),
            ("x-forwarded-proto", "http"),
            ("forwarded", "host=192.168.1.10:23457;proto=http"),
        ]);
        let (status, response_headers, _) = send(
            &fixture.app,
            request(
                "POST",
                "/session",
                &headers,
                json!({"username":USERNAME,"password":credentials().0}).to_string(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{headers:?}");
        assert!(!response_headers.contains_key("set-cookie"));
    }
    assert_eq!(fixture.session_count(), 0);
    let mut req = login_request(Some(ORIGIN), USERNAME, &credentials().0);
    req.headers_mut()
        .insert("host", "unrelated.internal:8080".parse().unwrap());
    assert_eq!(send(&fixture.app, req).await.0, StatusCode::OK);
}

#[tokio::test]
async fn duplicate_cookie_and_origin_headers_are_rejected() {
    let fixture = Fixture::new();
    let (cookie, _) = login(&fixture.app, ORIGIN).await;
    for headers in [
        vec![("cookie", cookie.as_str()), ("cookie", cookie.as_str())],
        vec![("cookie", cookie.as_str()), ("cookie", "memento_session=")],
    ] {
        let (status, _, _) = send(
            &fixture.app,
            request("GET", "/session", &headers, String::new()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let mut req = login_request(Some(ORIGIN), USERNAME, &credentials().0);
    req.headers_mut().append("origin", ORIGIN.parse().unwrap());
    let (status, headers, _) = send(&fixture.app, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!headers.contains_key("set-cookie"));
}

#[tokio::test]
async fn logout_delete_failure_does_not_clear_cookie_or_claim_success() {
    let fixture = Fixture::new();
    let (cookie, csrf) = login(&fixture.app, ORIGIN).await;
    fixture
        .pool
        .get()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_session_delete BEFORE DELETE ON browser_sessions
         BEGIN SELECT RAISE(ABORT, 'synthetic-delete-failure'); END;",
        )
        .unwrap();
    let (status, headers, bytes) = send(
        &fixture.app,
        request(
            "DELETE",
            "/session",
            &[
                ("cookie", &cookie),
                ("origin", ORIGIN),
                ("x-csrf-token", &csrf),
            ],
            String::new(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!headers.contains_key("set-cookie"));
    assert!(!String::from_utf8_lossy(&bytes).contains("synthetic-delete-failure"));
    assert_eq!(fixture.session_count(), 1);
    let (status, _, _) = send(
        &fixture.app,
        request("GET", "/session", &[("cookie", &cookie)], String::new()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn session_reads_do_not_write_even_when_session_is_expired() {
    let fixture = Fixture::new();
    let (cookie, _) = login(&fixture.app, ORIGIN).await;
    fixture
        .pool
        .get()
        .unwrap()
        .execute_batch(
            "UPDATE browser_sessions SET expires_at = 0;
         CREATE TRIGGER reject_session_delete BEFORE DELETE ON browser_sessions
         BEGIN SELECT RAISE(ABORT, 'unexpected-delete'); END;
         CREATE TRIGGER reject_session_update BEFORE UPDATE ON browser_sessions
         BEGIN SELECT RAISE(ABORT, 'unexpected-update'); END;",
        )
        .unwrap();
    let (status, _, _) = send(
        &fixture.app,
        request("GET", "/session", &[("cookie", &cookie)], String::new()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        fixture.session_count(),
        1,
        "read path must not perform cleanup"
    );
}

#[tokio::test]
async fn more_than_32_sessions_are_allowed_and_login_cleans_expired_rows() {
    let fixture = Fixture::new();
    let step = chrono::Utc::now().timestamp() as u64 / 30;
    let (cookie, _) = login_at_step(&fixture.app, ORIGIN, step).await;
    let conn = fixture.pool.get().unwrap();
    for index in 1..33 {
        let digest = format!("{:x}", Sha256::digest(format!("synthetic-session-{index}")));
        conn.execute(
            "INSERT INTO browser_sessions (token_hash, csrf_token, credential_hash, expires_at)
             SELECT ?1, csrf_token, credential_hash, expires_at FROM browser_sessions LIMIT 1",
            [&digest],
        )
        .unwrap();
    }
    drop(conn);
    assert_eq!(fixture.session_count(), 33);
    let token = cookie.strip_prefix("memento_session=").unwrap();
    let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
    fixture
        .pool
        .get()
        .unwrap()
        .execute(
            "UPDATE browser_sessions SET expires_at = 0 WHERE token_hash = ?1",
            [&digest],
        )
        .unwrap();
    login_at_step(&fixture.app, ORIGIN, step + 1).await;
    assert_eq!(fixture.session_count(), 33);
    let (status, _, _) = send(
        &fixture.app,
        request("GET", "/session", &[("cookie", &cookie)], String::new()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn disabling_browser_auth_rejects_existing_persistent_cookie() {
    let fixture = Fixture::new();
    let (cookie, _) = login(&fixture.app, ORIGIN).await;
    let disabled = memento::build_router(
        AppState::new(fixture.pool.clone(), API_KEY.to_owned())
            .with_browser(BrowserAuth::disabled()),
    );
    for path in ["/session", "/private/diaries"] {
        let (status, _, _) = send(
            &disabled,
            request("GET", path, &[("cookie", &cookie)], String::new()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
