//! Diary HTTP contracts. Schema creation is deliberately left to production code.

use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
    Router,
};
use chrono::{Days, FixedOffset, NaiveDate, Utc};
use http_body_util::BodyExt;
use memento::db::DbPool;
use serde_json::{json, Value};
use tower::ServiceExt;

fn fresh_app() -> (Router, DbPool, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = memento::db::build_pool(tmp.path().to_str().unwrap()).unwrap();
    memento::db::init_schema(&pool).unwrap();
    let state = memento::state::AppState::new(pool.clone(), "testkey".into());
    (memento::build_router(state), pool, tmp)
}

fn request(method: &str, uri: &str, body: Value, version: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-api-key", "testkey");
    if let Some(version) = version {
        builder = builder.header("if-match", version);
    }
    if body.is_null() {
        builder.body(Body::empty()).unwrap()
    } else {
        builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, body.to_vec())
}

async fn diary_response(
    app: &Router,
    req: Request<Body>,
    expected: StatusCode,
) -> (HeaderMap, Value) {
    let (status, headers, bytes) = send(app, req).await;
    assert_eq!(status, expected, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(headers.get("cache-control").unwrap(), "no-store");
    if status == StatusCode::NO_CONTENT {
        assert!(bytes.is_empty());
        return (headers, Value::Null);
    }
    let value = envelope(status, &headers, &bytes);
    (headers, value["data"].clone())
}

fn envelope(status: StatusCode, headers: &HeaderMap, bytes: &[u8]) -> Value {
    assert!(headers["content-type"]
        .to_str()
        .unwrap()
        .starts_with("application/json"));
    assert_eq!(headers.get("cache-control").unwrap(), "no-store");
    let value: Value = serde_json::from_slice(bytes).expect("JSON response envelope");
    assert_eq!(value["success"], status.is_success());
    assert!(value.get("data").is_some());
    assert!(value.get("error").is_some());
    if status.is_success() {
        assert_eq!(value["error"], Value::Null);
    } else {
        assert_eq!(value["data"], Value::Null);
        assert!(!value["error"].as_str().unwrap().is_empty());
    }
    value
}

async fn rejected(app: &Router, req: Request<Body>) {
    let (status, headers, bytes) = send(app, req).await;
    assert!(status.is_client_error(), "{status}: {bytes:?}");
    assert_ne!(status, StatusCode::NOT_FOUND);
    assert_ne!(status, StatusCode::UNAUTHORIZED);
    envelope(status, &headers, &bytes);
}

fn assert_dto(dto: &Value, content: &str, version: i64) {
    assert_eq!(dto.as_object().unwrap().len(), 6);
    assert!(dto["id"].as_i64().unwrap() > 0);
    assert_eq!(dto["content"], content);
    assert_eq!(dto["version"], version);
    let date = dto["create_date"].as_str().unwrap();
    assert_eq!(date.len(), 10);
    NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
    for field in ["created_at", "updated_at"] {
        assert!(dto.get(field).is_some());
        assert!(dto[field].is_null() || dto[field].is_string());
    }
}

async fn create(app: &Router, content: &str) -> Value {
    let (_, dto) = diary_response(
        app,
        request("POST", "/api/diaries", json!({"content": content}), None),
        StatusCode::CREATED,
    )
    .await;
    assert_dto(&dto, content.trim(), 1);
    dto
}

async fn list(app: &Router, query: &str) -> Value {
    diary_response(
        app,
        request("GET", &format!("/api/diaries{query}"), Value::Null, None),
        StatusCode::OK,
    )
    .await
    .1
}

async fn seed(pool: &DbPool, content: &str, date: &str) -> i64 {
    let pool = pool.clone();
    let content = content.to_string();
    let date = date.to_string();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get().unwrap();
        conn.execute(
            "INSERT INTO diaries (content, create_date) VALUES (?1, ?2)",
            rusqlite::params![content, date],
        )
        .expect("production init_schema must create diaries");
        conn.last_insert_rowid()
    })
    .await
    .unwrap()
}

fn ids(page: &Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_i64().unwrap())
        .collect()
}

async fn delete(app: &Router, id: i64) {
    diary_response(
        app,
        request(
            "DELETE",
            &format!("/api/diaries/{id}"),
            Value::Null,
            Some("\"1\""),
        ),
        StatusCode::NO_CONTENT,
    )
    .await;
}

#[tokio::test]
async fn all_diary_routes_require_key_even_for_reads() {
    let (app, _pool, _tmp) = fresh_app();
    for key in [None, Some("wrongkey")] {
        for (method, uri) in [
            ("GET", "/api/diaries"),
            ("POST", "/api/diaries"),
            ("GET", "/api/diaries/1"),
            ("PATCH", "/api/diaries/1"),
            ("DELETE", "/api/diaries/1"),
        ] {
            let mut req = request(method, uri, json!({"content": "private"}), Some("\"1\""));
            req.headers_mut().remove("x-api-key");
            if let Some(key) = key {
                req.headers_mut().insert("x-api-key", key.parse().unwrap());
            }
            diary_response(&app, req, StatusCode::UNAUTHORIZED).await;
        }
    }
}

#[tokio::test]
async fn create_trims_unicode_and_returns_full_dto() {
    let (app, _pool, _tmp) = fresh_app();
    let dto = create(&app, "\u{3000}\n\u{65e5}\u{8bb0}\u{1f600}\t ").await;
    let (headers, detail) = diary_response(
        &app,
        request(
            "GET",
            &format!("/api/diaries/{}", dto["id"]),
            Value::Null,
            None,
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(detail, dto);
    assert_eq!(headers["etag"], "\"1\"");
}

#[tokio::test]
async fn first_date_is_today_in_shanghai() {
    let (app, _pool, _tmp) = fresh_app();
    let timezone = FixedOffset::east_opt(8 * 3600).unwrap();
    let before = Utc::now().with_timezone(&timezone).date_naive();
    let dto = create(&app, "first").await;
    let after = Utc::now().with_timezone(&timezone).date_naive();
    let actual = dto["create_date"].as_str().unwrap();
    assert!(actual == before.to_string() || actual == after.to_string());
}

#[tokio::test]
async fn post_rejects_missing_empty_null_and_non_string_content() {
    let (app, _pool, _tmp) = fresh_app();
    for body in [
        json!({}),
        json!({"content": ""}),
        json!({"content": " \n\t\u{3000}"}),
        json!({"content": null}),
        json!({"content": 1}),
        json!({"content": []}),
    ] {
        rejected(&app, request("POST", "/api/diaries", body, None)).await;
    }
    assert_eq!(list(&app, "").await["total"], 0);
}

#[tokio::test]
async fn post_and_patch_deny_unknown_and_server_managed_fields() {
    let (app, _pool, _tmp) = fresh_app();
    let dto = create(&app, "original").await;
    let uri = format!("/api/diaries/{}", dto["id"]);
    for field in [
        "deleted_at",
        "id",
        "create_date",
        "created_at",
        "updated_at",
        "version",
        "extra",
    ] {
        let mut body = json!({"content": "changed"});
        body[field] = match field {
            "id" | "version" => json!(999),
            "create_date" => json!("2020-01-01"),
            "created_at" | "updated_at" => json!("2020-01-01T00:00:00Z"),
            _ => json!({"forbidden": true}),
        };
        rejected(&app, request("POST", "/api/diaries", body.clone(), None)).await;
        rejected(&app, request("PATCH", &uri, body, Some("\"1\""))).await;
    }
    let (_, unchanged) = diary_response(
        &app,
        request("GET", &uri, Value::Null, None),
        StatusCode::OK,
    )
    .await;
    assert_eq!(unchanged, dto);
    assert_eq!(list(&app, "").await["total"], 1);
}

#[tokio::test]
async fn content_limit_counts_unicode_scalars_not_bytes() {
    let (app, _pool, _tmp) = fresh_app();
    for unit in ["a", "\u{65e5}", "\u{1f600}"] {
        let content = unit.repeat(10_000);
        let dto = create(&app, &format!(" \n{content}\t ")).await;
        let uri = format!("/api/diaries/{}", dto["id"]);
        let too_long = unit.repeat(10_001);
        diary_response(
            &app,
            request("POST", "/api/diaries", json!({"content": too_long}), None),
            StatusCode::BAD_REQUEST,
        )
        .await;
        diary_response(
            &app,
            request("PATCH", &uri, json!({"content": too_long}), Some("\"1\"")),
            StatusCode::BAD_REQUEST,
        )
        .await;
        let (_, patched) = diary_response(
            &app,
            request("PATCH", &uri, json!({"content": content}), Some("\"1\"")),
            StatusCode::OK,
        )
        .await;
        assert_dto(&patched, &content, 2);
    }
}

#[tokio::test]
async fn patch_rejects_invalid_content_without_changing_version() {
    let (app, _pool, _tmp) = fresh_app();
    let dto = create(&app, "original").await;
    let uri = format!("/api/diaries/{}", dto["id"]);
    for body in [
        json!({}),
        json!({"content": null}),
        json!({"content": "\u{3000}\n"}),
        json!({"content": false}),
    ] {
        rejected(&app, request("PATCH", &uri, body, Some("\"1\""))).await;
    }
    let (_, unchanged) = diary_response(
        &app,
        request("GET", &uri, Value::Null, None),
        StatusCode::OK,
    )
    .await;
    assert_eq!(unchanged, dto);
}

#[tokio::test]
async fn patch_increments_version_but_preserves_creation_fields() {
    let (app, pool, _tmp) = fresh_app();
    let id = seed(&pool, "old", "2000-02-29").await;
    let uri = format!("/api/diaries/{id}");
    let (_, before) = diary_response(
        &app,
        request("GET", &uri, Value::Null, None),
        StatusCode::OK,
    )
    .await;
    let (_, patched) = diary_response(
        &app,
        request(
            "PATCH",
            &uri,
            json!({"content": "  edited\n"}),
            Some("\"1\""),
        ),
        StatusCode::OK,
    )
    .await;
    assert_dto(&patched, "edited", 2);
    assert_eq!(patched["id"], id);
    assert_eq!(patched["create_date"], before["create_date"]);
    assert_eq!(patched["created_at"], before["created_at"]);
    let (headers, detail) = diary_response(
        &app,
        request("GET", &uri, Value::Null, None),
        StatusCode::OK,
    )
    .await;
    assert_eq!(headers["etag"], "\"2\"");
    assert_eq!(detail, patched);
}

#[tokio::test]
async fn mutations_allow_omitting_if_match_but_still_advance_versions() {
    let (app, pool, _tmp) = fresh_app();
    let dto = create(&app, "original").await;
    let uri = format!("/api/diaries/{}", dto["id"]);
    let (_, updated) = diary_response(
        &app,
        request("PATCH", &uri, json!({"content":"changed"}), None),
        StatusCode::OK,
    )
    .await;
    assert_eq!(updated["content"], "changed");
    assert_eq!(updated["version"], 2);
    assert_eq!(updated["create_date"], dto["create_date"]);
    diary_response(
        &app,
        request("DELETE", &uri, Value::Null, None),
        StatusCode::NO_CONTENT,
    )
    .await;
    let retained = stored_diary(&pool, dto["id"].as_i64().unwrap()).await;
    assert_eq!(retained["content"], "changed");
    assert_eq!(retained["version"], 3);
    assert!(retained["deleted_at"].is_string());
    assert_eq!(list(&app, "").await["total"], 0);
    diary_response(
        &app,
        request("DELETE", &uri, Value::Null, None),
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn mutations_reject_malformed_if_match() {
    let (app, _pool, _tmp) = fresh_app();
    let dto = create(&app, "original").await;
    let uri = format!("/api/diaries/{}", dto["id"]);
    for version in [
        "",
        "1",
        "W/\"1\"",
        "*",
        "\"abc\"",
        "\"1\", \"2\"",
        "\"-1\"",
        "\"999999999999999999999999999999\"",
    ] {
        for method in ["PATCH", "DELETE"] {
            let body = if method == "PATCH" {
                json!({"content": "changed"})
            } else {
                Value::Null
            };
            diary_response(
                &app,
                request(method, &uri, body, Some(version)),
                StatusCode::BAD_REQUEST,
            )
            .await;
        }
    }
    assert_eq!(list(&app, "").await["items"][0], dto);
}

#[tokio::test]
async fn stale_patch_and_delete_fail_without_mutation() {
    let (app, _pool, _tmp) = fresh_app();
    let dto = create(&app, "original").await;
    let uri = format!("/api/diaries/{}", dto["id"]);
    let (_, updated) = diary_response(
        &app,
        request("PATCH", &uri, json!({"content": "winner"}), Some("\"1\"")),
        StatusCode::OK,
    )
    .await;
    for method in ["PATCH", "DELETE"] {
        let body = if method == "PATCH" {
            json!({"content": "loser"})
        } else {
            Value::Null
        };
        diary_response(
            &app,
            request(method, &uri, body, Some("\"1\"")),
            StatusCode::PRECONDITION_FAILED,
        )
        .await;
    }
    assert_eq!(list(&app, "").await["items"][0], updated);
    diary_response(
        &app,
        request("DELETE", &uri, Value::Null, Some("\"2\"")),
        StatusCode::NO_CONTENT,
    )
    .await;
    assert_eq!(list(&app, "").await["total"], 0);
    diary_response(
        &app,
        request("GET", &uri, Value::Null, None),
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn missing_diary_returns_404_for_detail_patch_delete() {
    let (app, _pool, _tmp) = fresh_app();
    for method in ["GET", "PATCH", "DELETE"] {
        let body = if method == "PATCH" {
            json!({"content": "missing"})
        } else {
            Value::Null
        };
        diary_response(
            &app,
            request(method, "/api/diaries/99999", body, Some("\"1\"")),
            StatusCode::NOT_FOUND,
        )
        .await;
    }
}

#[tokio::test]
async fn next_date_uses_max_date_not_id_or_today() {
    let (app, pool, _tmp) = fresh_app();
    seed(&pool, "maximum", "2000-02-28").await;
    seed(&pool, "newest id", "1999-12-31").await;
    assert_eq!(create(&app, "leap day").await["create_date"], "2000-02-29");
    assert_eq!(
        create(&app, "next month").await["create_date"],
        "2000-03-01"
    );
    seed(&pool, "future maximum", "2099-12-31").await;
    assert_eq!(
        create(&app, "future successor").await["create_date"],
        "2100-01-01"
    );
}

#[tokio::test]
async fn deleting_last_reuses_next_date_and_middle_holes_stay_empty() {
    let (app, pool, _tmp) = fresh_app();
    let first = seed(&pool, "first", "2000-12-30").await;
    let middle = create(&app, "middle").await;
    let last = create(&app, "last").await;
    assert_eq!(last["create_date"], "2001-01-01");
    delete(&app, last["id"].as_i64().unwrap()).await;
    let replacement = create(&app, "replacement").await;
    assert_eq!(replacement["create_date"], last["create_date"]);
    delete(&app, middle["id"].as_i64().unwrap()).await;
    assert_eq!(
        create(&app, "after hole").await["create_date"],
        "2001-01-02"
    );
    assert!(ids(&list(&app, "").await).contains(&first));
}

#[tokio::test]
async fn deleting_all_resets_date_to_business_today() {
    let (app, pool, _tmp) = fresh_app();
    let id = seed(&pool, "historical", "1900-01-01").await;
    delete(&app, id).await;
    let timezone = FixedOffset::east_opt(8 * 3600).unwrap();
    let before = Utc::now().with_timezone(&timezone).date_naive().to_string();
    let dto = create(&app, "restart").await;
    let after = Utc::now().with_timezone(&timezone).date_naive().to_string();
    assert!(dto["create_date"] == before || dto["create_date"] == after);
}

#[tokio::test]
async fn historical_duplicate_dates_and_null_timestamps_are_readable() {
    let (app, pool, _tmp) = fresh_app();
    let first = seed(&pool, "duplicate one", "2020-01-01").await;
    let second = seed(&pool, "duplicate two", "2020-01-01").await;
    let page = list(&app, "").await;
    let mut actual = ids(&page);
    actual.sort_unstable();
    assert_eq!(actual, vec![first, second]);
    for item in page["items"].as_array().unwrap() {
        assert!(item["created_at"].is_null());
        assert!(item["updated_at"].is_null());
        let (_, detail) = diary_response(
            &app,
            request(
                "GET",
                &format!("/api/diaries/{}", item["id"]),
                Value::Null,
                None,
            ),
            StatusCode::OK,
        )
        .await;
        assert_eq!(&detail, item);
    }
    assert_eq!(
        create(&app, "after duplicates").await["create_date"],
        "2020-01-02"
    );
}

#[tokio::test]
async fn six_concurrent_posts_allocate_distinct_consecutive_dates() {
    let (app, pool, _tmp) = fresh_app();
    seed(&pool, "anchor", "2024-02-26").await;
    let (a, b, c, d, e, f) = tokio::join!(
        create(&app, "a"),
        create(&app, "b"),
        create(&app, "c"),
        create(&app, "d"),
        create(&app, "e"),
        create(&app, "f")
    );
    let entries = [a, b, c, d, e, f];
    let mut dates: Vec<_> = entries
        .iter()
        .map(|v| v["create_date"].as_str().unwrap().to_string())
        .collect();
    dates.sort();
    assert_eq!(
        dates,
        [
            "2024-02-27",
            "2024-02-28",
            "2024-02-29",
            "2024-03-01",
            "2024-03-02",
            "2024-03-03"
        ]
    );
    let mut unique_ids: Vec<_> = entries.iter().map(|v| v["id"].as_i64().unwrap()).collect();
    unique_ids.sort_unstable();
    unique_ids.dedup();
    assert_eq!(unique_ids.len(), 6);
    let page = list(&app, "").await;
    assert_eq!(page["total"], 7);
    for entry in entries {
        assert!(page["items"].as_array().unwrap().contains(&entry));
    }
}

#[tokio::test]
async fn concurrent_patches_allow_only_one_matching_version() {
    let (app, _pool, _tmp) = fresh_app();
    let dto = create(&app, "original").await;
    let uri = format!("/api/diaries/{}", dto["id"]);
    let (a, b) = tokio::join!(
        send(
            &app,
            request("PATCH", &uri, json!({"content": "a"}), Some("\"1\""))
        ),
        send(
            &app,
            request("PATCH", &uri, json!({"content": "b"}), Some("\"1\""))
        )
    );
    let mut statuses = vec![a.0.as_u16(), b.0.as_u16()];
    statuses.sort_unstable();
    assert_eq!(statuses, vec![200, 412]);
    let mut winner = Value::Null;
    for (status, headers, body) in [a, b] {
        let value = envelope(status, &headers, &body);
        if status == StatusCode::OK {
            winner = value["data"].clone();
        }
    }
    assert_eq!(winner["version"], 2);
    assert_eq!(list(&app, "").await["items"][0], winner);
}

#[tokio::test]
async fn list_defaults_sort_and_pagination_preserve_total() {
    let (app, pool, _tmp) = fresh_app();
    assert_eq!(
        list(&app, "").await,
        json!({"items": [], "total": 0, "page": 1, "per_page": 24})
    );
    let start = NaiveDate::from_ymd_opt(2020, 1, 1).unwrap();
    let mut expected = Vec::new();
    for offset in 0..26 {
        let date = start.checked_add_days(Days::new(offset)).unwrap();
        expected.push(seed(&pool, &format!("entry {offset}"), &date.to_string()).await);
    }
    expected.reverse();
    let default = list(&app, "").await;
    assert_eq!(default["page"], 1);
    assert_eq!(default["per_page"], 24);
    assert_eq!(default["total"], 26);
    assert_eq!(ids(&default), expected[..24]);
    assert_eq!(list(&app, "?sort=desc").await, default);
    let second = list(&app, "?page=2").await;
    assert_eq!(second["page"], 2);
    assert_eq!(second["total"], 26);
    assert_eq!(ids(&second), expected[24..]);
    expected.reverse();
    assert_eq!(
        ids(&list(&app, "?sort=asc&per_page=3&page=2").await),
        expected[3..6]
    );
    let empty = list(&app, "?page=99").await;
    assert!(ids(&empty).is_empty());
    assert_eq!(empty["total"], 26);
}

#[tokio::test]
async fn list_per_page_is_capped_at_100() {
    let (app, pool, _tmp) = fresh_app();
    for _ in 0..101 {
        seed(&pool, "entry", "2020-01-01").await;
    }
    for query in ["?per_page=100", "?per_page=101"] {
        let page = list(&app, query).await;
        assert_eq!(page["per_page"], 100);
        assert_eq!(page["total"], 101);
        assert_eq!(ids(&page).len(), 100);
    }
}

#[tokio::test]
async fn list_filters_content_and_inclusive_date_range_before_pagination() {
    let (app, pool, _tmp) = fresh_app();
    seed(&pool, "needle outside", "2020-01-01").await;
    let first = seed(&pool, "needle first", "2020-01-02").await;
    seed(&pool, "unrelated", "2020-01-03").await;
    let last = seed(&pool, "needle last", "2020-01-04").await;
    seed(&pool, "needle outside", "2020-01-05").await;
    let page = list(
        &app,
        "?q=needle&start_date=2020-01-02&end_date=2020-01-04&sort=asc&per_page=1&page=2",
    )
    .await;
    assert_eq!(page["total"], 2);
    assert_eq!(page["page"], 2);
    assert_eq!(page["per_page"], 1);
    assert_eq!(ids(&page), vec![last]);
    assert_eq!(
        ids(&list(&app, "?start_date=2020-01-02&end_date=2020-01-02").await),
        vec![first]
    );
    assert_eq!(list(&app, "?start_date=2020-01-04").await["total"], 2);
    assert_eq!(list(&app, "?end_date=2020-01-02").await["total"], 2);
    assert_eq!(list(&app, "?q=absent").await["total"], 0);
}

#[tokio::test]
async fn search_treats_like_metacharacters_and_sql_as_literal_text() {
    let (app, pool, _tmp) = fresh_app();
    let cases = [
        ("100% complete", "%25"),
        ("under_score", "%5F"),
        (r"back\slash", "%5C"),
        ("' OR 1=1 --", "%27%20OR%201%3D1%20--"),
        (
            "\u{65e5}\u{8bb0}\u{1f600}",
            "%E6%97%A5%E8%AE%B0%F0%9F%98%80",
        ),
    ];
    seed(
        &pool,
        "100X complete underXscore backslash unrelated",
        "2020-01-01",
    )
    .await;
    for (text, _) in cases {
        seed(&pool, text, "2020-01-01").await;
    }
    for (text, query) in cases {
        let page = list(&app, &format!("?q={query}")).await;
        assert_eq!(page["total"], 1, "literal query {query}");
        assert_eq!(page["items"][0]["content"], text);
    }
}

#[tokio::test]
async fn list_rejects_noncanonical_or_impossible_dates_and_reversed_range() {
    let (app, _pool, _tmp) = fresh_app();
    for date in [
        "2020-1-01",
        "2020-01-1",
        "20200101",
        "2023-02-29",
        "2020-02-30",
        "2020-13-01",
        "2020-00-01",
        "2020-01-00",
        "2020-01-01T00:00:00Z",
        "",
        "%202020-01-01",
    ] {
        for field in ["start_date", "end_date"] {
            diary_response(
                &app,
                request(
                    "GET",
                    &format!("/api/diaries?{field}={date}"),
                    Value::Null,
                    None,
                ),
                StatusCode::BAD_REQUEST,
            )
            .await;
        }
    }
    diary_response(
        &app,
        request(
            "GET",
            "/api/diaries?start_date=2020-02-02&end_date=2020-02-01",
            Value::Null,
            None,
        ),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(
        list(&app, "?start_date=2024-02-29&end_date=2024-02-29").await["total"],
        0
    );
}

#[tokio::test]
async fn query_and_path_extractor_rejections_use_private_json_envelopes() {
    let (app, _pool, _tmp) = fresh_app();
    for query in [
        "page=abc",
        "per_page=abc",
        "page=999999999999999999999999999",
        "sort=sideways",
    ] {
        rejected(
            &app,
            request("GET", &format!("/api/diaries?{query}"), Value::Null, None),
        )
        .await;
    }
    for id in ["abc", "99999999999999999999999999"] {
        for method in ["GET", "PATCH", "DELETE"] {
            let body = if method == "PATCH" {
                json!({"content": "valid"})
            } else {
                Value::Null
            };
            rejected(
                &app,
                request(method, &format!("/api/diaries/{id}"), body, Some("\"1\"")),
            )
            .await;
        }
    }
}

#[tokio::test]
async fn json_extractor_rejections_use_private_json_envelopes() {
    let (app, _pool, _tmp) = fresh_app();
    let dto = create(&app, "original").await;
    let detail = format!("/api/diaries/{}", dto["id"]);
    for (method, uri) in [("POST", "/api/diaries"), ("PATCH", detail.as_str())] {
        for (content_type, body) in [
            (Some("application/json"), "{"),
            (Some("application/json"), ""),
            (Some("application/json"), "[]"),
            (Some("text/plain"), "{\"content\":\"valid\"}"),
            (None, "{\"content\":\"valid\"}"),
        ] {
            let mut req = request(method, uri, Value::Null, Some("\"1\""));
            if let Some(content_type) = content_type {
                req.headers_mut()
                    .insert("content-type", content_type.parse().unwrap());
            }
            *req.body_mut() = Body::from(body);
            rejected(&app, req).await;
        }
    }
    assert_eq!(list(&app, "").await["items"][0], dto);
}

#[tokio::test]
async fn body_limit_accepts_64kib_and_rejects_one_more_with_413_envelope() {
    let (app, _pool, _tmp) = fresh_app();
    let dto = create(&app, "original").await;
    let uri = format!("/api/diaries/{}", dto["id"]);
    for (method, target, success) in [
        ("POST", "/api/diaries", StatusCode::CREATED),
        ("PATCH", uri.as_str(), StatusCode::OK),
    ] {
        for size in [65_537, 65_536] {
            // JSON whitespace isolates the byte limit from the content character limit.
            let mut body = json!({"content": "valid"}).to_string();
            body.push_str(&" ".repeat(size - body.len()));
            let mut req = request(method, target, json!({}), Some("\"1\""));
            *req.body_mut() = Body::from(body);
            let expected = if size > 65_536 {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                success
            };
            diary_response(&app, req, expected).await;
        }
    }
    assert_eq!(list(&app, "").await["total"], 2);
}

#[tokio::test]
async fn database_failures_are_sanitized_for_every_diary_route() {
    let (app, pool, _tmp) = fresh_app();
    let id = seed(&pool, "private content marker", "2020-01-01").await;
    tokio::task::spawn_blocking(move || {
        pool.get()
            .unwrap()
            .execute_batch("ALTER TABLE diaries RENAME TO private_diary_storage_marker")
            .unwrap();
    })
    .await
    .unwrap();
    let detail = format!("/api/diaries/{id}");
    for (method, uri) in [
        ("GET", "/api/diaries"),
        ("POST", "/api/diaries"),
        ("GET", detail.as_str()),
        ("PATCH", detail.as_str()),
        ("DELETE", detail.as_str()),
    ] {
        let body = if method == "POST" || method == "PATCH" {
            json!({"content": "private content marker"})
        } else {
            Value::Null
        };
        let (status, headers, bytes) = send(&app, request(method, uri, body, Some("\"1\""))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        envelope(status, &headers, &bytes);
        let text = String::from_utf8(bytes).unwrap().to_lowercase();
        for secret in [
            "private content marker",
            "private_diary_storage_marker",
            "no such table",
            "select ",
            "insert ",
            "sqlite",
            "testkey",
        ] {
            assert!(!text.contains(secret), "leaked internal detail: {text}");
        }
    }
}

#[tokio::test]
async fn diary_and_favorites_are_isolated_and_favorites_remain_public() {
    let (app, _pool, _tmp) = fresh_app();
    let favorite_body = json!({
        "type": "book", "name": "favorite-only",
        "image_base64": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M8AAAMBAQDJ/pLvAAAAAElFTkSuQmCC"
    });
    let (status, _, bytes) =
        send(&app, request("POST", "/api/favorites", favorite_body, None)).await;
    assert_eq!(status, StatusCode::CREATED);
    let favorite: Value = serde_json::from_slice(&bytes).unwrap();
    let id = favorite["data"]["id"].as_i64().unwrap();
    diary_response(
        &app,
        request("GET", &format!("/api/diaries/{id}"), Value::Null, None),
        StatusCode::NOT_FOUND,
    )
    .await;
    let diary = create(&app, "diary-only").await;
    assert_eq!(diary["id"], id, "independent ID sequences should overlap");
    let uri = format!("/api/diaries/{id}");
    diary_response(
        &app,
        request(
            "PATCH",
            &uri,
            json!({"content": "edited diary"}),
            Some("\"1\""),
        ),
        StatusCode::OK,
    )
    .await;
    diary_response(
        &app,
        request("DELETE", &uri, Value::Null, Some("\"2\"")),
        StatusCode::NO_CONTENT,
    )
    .await;
    for uri in ["/api/favorites".to_string(), format!("/api/favorites/{id}")] {
        let mut req = request("GET", &uri, Value::Null, None);
        req.headers_mut().remove("x-api-key");
        let (status, _, bytes) = send(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let actual = if uri == "/api/favorites" {
            &value["data"]["items"][0]
        } else {
            &value["data"]
        };
        assert_eq!(actual, &favorite["data"]);
    }
    let (status, _, _) = send(
        &app,
        request(
            "PUT",
            &format!("/api/favorites/{id}"),
            json!({"summary": "no diary precondition"}),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list(&app, "").await["total"], 0);
    let survivor = create(&app, "survives favorite deletion").await;
    let (status, _, bytes) = send(
        &app,
        request("DELETE", &format!("/api/favorites/{id}"), Value::Null, None),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(bytes.is_empty());
    assert_eq!(list(&app, "").await["items"], json!([survivor]));
}

#[tokio::test]
async fn date_overflow_returns_conflict_without_inserting() {
    let (app, pool, _tmp) = fresh_app();
    seed(&pool, "last supported date", "9999-12-31").await;
    diary_response(
        &app,
        request("POST", "/api/diaries", json!({"content": "overflow"}), None),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(list(&app, "").await["total"], 1);
}

#[tokio::test]
async fn invalid_historical_date_fails_closed_without_leaking_stored_text() {
    let (app, pool, _tmp) = fresh_app();
    seed(
        &pool,
        "private historical content",
        "private-invalid-date-marker",
    )
    .await;
    let (status, headers, bytes) = send(
        &app,
        request("POST", "/api/diaries", json!({"content": "new"}), None),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    envelope(status, &headers, &bytes);
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("private-invalid-date-marker"));
    assert!(!text.contains("private historical content"));
    assert_eq!(list(&app, "").await["total"], 1);
}

#[tokio::test]
async fn version_overflow_returns_conflict_without_mutating() {
    let (app, pool, _tmp) = fresh_app();
    let id = seed(&pool, "unchanged", "2020-01-01").await;
    tokio::task::spawn_blocking(move || {
        pool.get()
            .unwrap()
            .execute(
                "UPDATE diaries SET version = ?1 WHERE id = ?2",
                rusqlite::params![i64::MAX, id],
            )
            .unwrap();
    })
    .await
    .unwrap();
    let uri = format!("/api/diaries/{id}");
    diary_response(
        &app,
        request(
            "PATCH",
            &uri,
            json!({"content": "overflow"}),
            Some("\"9223372036854775807\""),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    let (_, dto) = diary_response(
        &app,
        request("GET", &uri, Value::Null, None),
        StatusCode::OK,
    )
    .await;
    assert_dto(&dto, "unchanged", i64::MAX);
    diary_response(
        &app,
        request("DELETE", &uri, Value::Null, Some("\"9223372036854775807\"")),
        StatusCode::CONFLICT,
    )
    .await;
    let (_, after) = diary_response(
        &app,
        request("GET", &uri, Value::Null, None),
        StatusCode::OK,
    )
    .await;
    assert_eq!(dto, after);
}

async fn stored_diary(pool: &DbPool, id: i64) -> Value {
    let pool = pool.clone();
    tokio::task::spawn_blocking(move || {
        pool.get().unwrap().query_row(
            "SELECT content,create_date,created_at,updated_at,version,deleted_at FROM diaries WHERE id=?1",
            [id], |row| Ok(json!({
                "content": row.get::<_, String>(0)?, "create_date": row.get::<_, String>(1)?,
                "created_at": row.get::<_, Option<String>>(2)?, "updated_at": row.get::<_, Option<String>>(3)?,
                "version": row.get::<_, i64>(4)?, "deleted_at": row.get::<_, Option<String>>(5)?
            })),
        ).expect("soft deletion must retain the original row")
    }).await.unwrap()
}

#[tokio::test]
async fn soft_delete_preserves_content_and_hides_every_normal_access() {
    let (app, pool, _tmp) = fresh_app();
    let original = create(&app, "retained private diary").await;
    let id = original["id"].as_i64().unwrap();
    let before = Utc::now();
    delete(&app, id).await;
    let after_time = Utc::now();
    let stored = stored_diary(&pool, id).await;
    for field in ["content", "create_date", "created_at"] {
        assert_eq!(stored[field], original[field]);
    }
    assert_eq!(stored["version"], 2);
    assert_eq!(stored["deleted_at"], stored["updated_at"]);
    let deleted_at =
        chrono::DateTime::parse_from_rfc3339(stored["deleted_at"].as_str().unwrap()).unwrap();
    assert!(deleted_at.timestamp_millis() >= before.timestamp_millis());
    assert!(deleted_at.timestamp_millis() <= after_time.timestamp_millis());
    let uri = format!("/api/diaries/{id}");
    for method in ["GET", "PATCH", "DELETE"] {
        for version in ["\"1\"", "\"2\""] {
            let body = if method == "PATCH" {
                json!({"content":"must not resurrect"})
            } else {
                Value::Null
            };
            diary_response(
                &app,
                request(method, &uri, body, Some(version)),
                StatusCode::NOT_FOUND,
            )
            .await;
        }
    }
    for query in [
        "",
        "?q=retained",
        "?start_date=0001-01-01&end_date=9999-12-31",
        "?include_deleted=true",
    ] {
        let page = list(&app, query).await;
        assert_eq!(page["total"], 0);
        assert!(ids(&page).is_empty());
    }
    assert_eq!(stored_diary(&pool, id).await, stored);
}

#[tokio::test]
async fn soft_deleted_rows_do_not_affect_filtered_counts_pagination_or_next_date() {
    let (app, pool, _tmp) = fresh_app();
    let first = seed(&pool, "same keyword", "2020-01-01").await;
    let hidden = seed(&pool, "same keyword", "2020-01-02").await;
    let last = seed(&pool, "same keyword", "2020-01-03").await;
    let future = seed(&pool, "same keyword", "9999-12-31").await;
    delete(&app, hidden).await;
    delete(&app, future).await;
    for (page, expected) in [(1, first), (2, last)] {
        let result = list(&app, &format!("?q=keyword&start_date=2020-01-01&end_date=9999-12-31&sort=asc&per_page=1&page={page}")).await;
        assert_eq!(result["total"], 2);
        assert_eq!(ids(&result), vec![expected]);
    }
    let next = create(&app, "new active entry").await;
    assert_eq!(next["create_date"], "2020-01-04");
    assert!(next["id"].as_i64().unwrap() > future);
    assert!(stored_diary(&pool, future).await["deleted_at"].is_string());
    delete(&app, first).await;
    delete(&app, last).await;
    delete(&app, next["id"].as_i64().unwrap()).await;
    let timezone = FixedOffset::east_opt(8 * 3600).unwrap();
    let before = Utc::now().with_timezone(&timezone).date_naive().to_string();
    let reset = create(&app, "all previous entries are retained but invisible").await;
    let after = Utc::now().with_timezone(&timezone).date_naive().to_string();
    assert!(reset["create_date"] == before || reset["create_date"] == after);
    assert_eq!(list(&app, "").await["total"], 1);
}

#[tokio::test]
async fn concurrent_soft_deletes_have_one_success_without_redeleting() {
    let (app, pool, _tmp) = fresh_app();
    let dto = create(&app, "concurrent deletion").await;
    let id = dto["id"].as_i64().unwrap();
    let uri = format!("/api/diaries/{id}");
    let (a, b) = tokio::join!(
        send(&app, request("DELETE", &uri, Value::Null, Some("\"1\""))),
        send(&app, request("DELETE", &uri, Value::Null, Some("\"1\"")))
    );
    let mut statuses = [a.0.as_u16(), b.0.as_u16()];
    statuses.sort();
    assert_eq!(statuses, [204, 404]);
    let stored = stored_diary(&pool, id).await;
    assert_eq!(stored["version"], 2);
    assert!(stored["deleted_at"].is_string());
}

#[tokio::test]
async fn concurrent_edit_and_soft_delete_never_resurrect_or_lose_version_check() {
    let (app, pool, _tmp) = fresh_app();
    let dto = create(&app, "before race").await;
    let id = dto["id"].as_i64().unwrap();
    let uri = format!("/api/diaries/{id}");
    let (edit, deletion) = tokio::join!(
        send(
            &app,
            request(
                "PATCH",
                &uri,
                json!({"content":"after race"}),
                Some("\"1\"")
            )
        ),
        send(&app, request("DELETE", &uri, Value::Null, Some("\"1\"")))
    );
    let stored = stored_diary(&pool, id).await;
    assert_eq!(stored["version"], 2);
    if edit.0 == StatusCode::OK {
        assert_eq!(deletion.0, StatusCode::PRECONDITION_FAILED);
        assert_eq!(stored["content"], "after race");
        assert!(stored["deleted_at"].is_null());
    } else {
        assert_eq!(edit.0, StatusCode::NOT_FOUND);
        assert_eq!(deletion.0, StatusCode::NO_CONTENT);
        assert_eq!(stored["content"], "before race");
        assert!(stored["deleted_at"].is_string());
    }
}

#[tokio::test]
async fn failed_soft_delete_rolls_back_all_fields_and_redacts_error() {
    let (app, pool, _tmp) = fresh_app();
    let dto = create(&app, "original row").await;
    let id = dto["id"].as_i64().unwrap();
    let before = stored_diary(&pool, id).await;
    let cloned = pool.clone();
    tokio::task::spawn_blocking(move || {
        cloned.get().unwrap().execute_batch("CREATE TRIGGER fail_soft_delete BEFORE UPDATE OF deleted_at ON diaries BEGIN SELECT RAISE(ABORT, 'private failure marker'); END;").unwrap();
    }).await.unwrap();
    let (status, headers, bytes) = send(
        &app,
        request(
            "DELETE",
            &format!("/api/diaries/{id}"),
            Value::Null,
            Some("\"1\""),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    envelope(status, &headers, &bytes);
    assert!(!String::from_utf8(bytes)
        .unwrap()
        .contains("private failure marker"));
    assert_eq!(stored_diary(&pool, id).await, before);
    assert_eq!(list(&app, "").await["items"], json!([dto]));
}

#[tokio::test]
async fn pagination_rejects_zero_and_offsets_outside_sqlite_integer_range() {
    let (app, _pool, _tmp) = fresh_app();
    for query in [
        "page=0",
        "per_page=0",
        "page=18446744073709551615&per_page=100",
        "page=9223372036854775809&per_page=1",
    ] {
        diary_response(
            &app,
            request("GET", &format!("/api/diaries?{query}"), Value::Null, None),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
}

#[tokio::test]
async fn json_rejection_statuses_are_fixed_and_do_not_echo_input() {
    let (app, _pool, _tmp) = fresh_app();
    for (content_type, body, expected) in [
        (
            "text/plain",
            "private-input-marker",
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            "application/json",
            "{\"content\": private-input-marker",
            StatusCode::BAD_REQUEST,
        ),
        (
            "application/json",
            "{\"content\": {\"private-input-marker\": true}}",
            StatusCode::BAD_REQUEST,
        ),
        (
            "application/json",
            "[\"private-input-marker\"]",
            StatusCode::BAD_REQUEST,
        ),
        (
            "application/json",
            "{\"content\": \"ok\", \"private-input-marker\": true}",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let req = Request::builder()
            .method("POST")
            .uri("/api/diaries")
            .header("x-api-key", "testkey")
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap();
        let (status, headers, bytes) = send(&app, req).await;
        assert_eq!(status, expected);
        envelope(status, &headers, &bytes);
        assert!(!String::from_utf8(bytes)
            .unwrap()
            .contains("private-input-marker"));
    }
}
