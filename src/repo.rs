//! Pure data access for the `favorites` table. All functions take a pooled
//! connection and perform synchronous rusqlite work; callers run them inside
//! `spawn_blocking`.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{Map, Value};

use crate::error::{AppError, AppResult};
use crate::image::DecodedImage;
use crate::models::{now_iso, Favorite, ItemType, PreparedCreate, PreparedUpdate};

/// Columns selected for the DTO projection (no blob). Source of truth for the
/// projection's column list and ordering.
const SELECT_COLS: &str = "id, type, name, url, aka, genres, release_date, rating, summary, \
     extra, (image IS NOT NULL) AS has_image, sort_date, created_at, updated_at";

/// Pre-built single-row SELECT, computed once at compile time from the same
/// column list as [`SELECT_COLS`] (avoids a per-call `format!` allocation).
const SELECT_ONE: &str = concat!(
    "SELECT id, type, name, url, aka, genres, release_date, rating, summary, ",
    "extra, (image IS NOT NULL) AS has_image, sort_date, created_at, updated_at ",
    "FROM favorites WHERE id = ?"
);

/// Map a row (using [`SELECT_COLS`] ordering) into a [`Favorite`].
fn row_to_favorite(row: &Row) -> rusqlite::Result<Favorite> {
    Ok(Favorite {
        id: row.get(0)?,
        item_type: row.get(1)?,
        name: row.get(2)?,
        url: row.get(3)?,
        aka: row.get(4)?,
        genres: row.get(5)?,
        release_date: row.get(6)?,
        rating: row.get(7)?,
        summary: row.get(8)?,
        extra: row.get(9)?,
        has_image: row.get::<_, i64>(10)? != 0,
        sort_date: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

/// Filter + pagination parameters for [`list`].
#[derive(Debug, Clone)]
pub struct ListQuery {
    pub item_type: Option<ItemType>,
    pub page: u32,
    pub per_page: u32,
    pub q: Option<String>,
}

/// List favorites ordered by `sort_date DESC, id DESC`. Returns `(rows, total)`.
pub fn list(conn: &Connection, query: &ListQuery) -> AppResult<(Vec<Favorite>, i64)> {
    let mut where_clauses: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(t) = query.item_type {
        where_clauses.push("type = ?".to_string());
        binds.push(Box::new(t.as_str().to_string()));
    }
    if let Some(q) = &query.q {
        // Case-insensitive substring match on name.
        where_clauses.push("name LIKE ? ESCAPE '\\'".to_string());
        binds.push(Box::new(format!("%{}%", escape_like(q))));
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };

    // Total count.
    let count_sql = format!("SELECT COUNT(*) FROM favorites {where_sql}");
    let bind_refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let total: i64 = conn
        .query_row(&count_sql, bind_refs.as_slice(), |r| r.get(0))
        .map_err(AppError::from)?;

    // Page.
    let offset = (query.page.saturating_sub(1) as i64) * query.per_page as i64;
    let list_sql = format!(
        "SELECT {SELECT_COLS} FROM favorites {where_sql} \
         ORDER BY sort_date DESC, id DESC LIMIT ? OFFSET ?"
    );

    let mut page_binds = binds;
    page_binds.push(Box::new(query.per_page as i64));
    page_binds.push(Box::new(offset));
    let page_refs: Vec<&dyn rusqlite::ToSql> = page_binds.iter().map(|b| b.as_ref()).collect();

    let mut stmt = conn.prepare(&list_sql).map_err(AppError::from)?;
    let rows = stmt
        .query_map(page_refs.as_slice(), row_to_favorite)
        .map_err(AppError::from)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(AppError::from)?;

    Ok((rows, total))
}

/// Escape `%` and `_` for a LIKE pattern (using `\` as escape char).
fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Fetch a single favorite by id, or `None`.
pub fn get(conn: &Connection, id: i64) -> AppResult<Option<Favorite>> {
    conn.query_row(SELECT_ONE, params![id], row_to_favorite)
        .optional()
        .map_err(AppError::from)
}

/// Insert a new favorite. Image bytes (if any) are passed pre-decoded.
pub fn create(
    conn: &Connection,
    p: &PreparedCreate,
    image: Option<&DecodedImage>,
) -> AppResult<i64> {
    let (img_bytes, img_mime): (Option<&[u8]>, Option<&str>) = match image {
        Some(d) => (Some(&d.bytes), Some(d.mime.as_str())),
        None => (None, None),
    };

    conn.execute(
        "INSERT INTO favorites \
         (type, name, url, aka, genres, release_date, rating, summary, extra, \
          image, image_mime, sort_date, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            p.item_type.as_str(),
            p.name,
            p.url,
            p.aka,
            p.genres,
            p.release_date,
            p.rating,
            p.summary,
            p.extra,
            img_bytes,
            img_mime,
            p.sort_date,
            p.now,
            p.now,
        ],
    )
    .map_err(AppError::from)?;

    Ok(conn.last_insert_rowid())
}

/// Apply a partial update. Returns `false` if the row does not exist.
///
/// The `extra` patch (if present) is merged with the existing stored `extra`.
/// Image bytes (if any) are passed pre-decoded and replace the stored image.
pub fn update(
    conn: &mut Connection,
    id: i64,
    up: &PreparedUpdate,
    image: Option<&DecodedImage>,
) -> AppResult<bool> {
    // Read-then-write must be atomic under concurrent writers: wrap the
    // existing-extra SELECT and the UPDATE in a single transaction.
    let tx = conn.transaction().map_err(AppError::from)?;

    // Ensure the row exists and grab current extra for merging.
    let existing_extra: Option<String> = tx
        .query_row(
            "SELECT extra FROM favorites WHERE id = ?",
            params![id],
            |r| r.get(0),
        )
        .optional()
        .map_err(AppError::from)?;

    let Some(existing_extra) = existing_extra else {
        return Ok(false);
    };

    let merged_extra = match &up.extra {
        Some(patch) => Some(merge_extra(&existing_extra, patch)?),
        None => None,
    };

    let mut sets: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(t) = up.item_type {
        sets.push("type = ?".into());
        binds.push(Box::new(t.as_str().to_string()));
    }
    if let Some(name) = &up.name {
        sets.push("name = ?".into());
        binds.push(Box::new(name.clone()));
    }
    if let Some(url) = &up.url {
        sets.push("url = ?".into());
        binds.push(Box::new(url.clone()));
    }
    if let Some(aka) = &up.aka {
        sets.push("aka = ?".into());
        binds.push(Box::new(aka.clone()));
    }
    if let Some(genres) = &up.genres {
        sets.push("genres = ?".into());
        binds.push(Box::new(genres.clone()));
    }
    if let Some(rd) = &up.release_date {
        sets.push("release_date = ?".into());
        binds.push(Box::new(rd.clone()));
    }
    if let Some(rating) = &up.rating {
        sets.push("rating = ?".into());
        binds.push(Box::new(*rating));
    }
    if let Some(summary) = &up.summary {
        sets.push("summary = ?".into());
        binds.push(Box::new(summary.clone()));
    }
    if let Some(extra) = &merged_extra {
        sets.push("extra = ?".into());
        binds.push(Box::new(extra.clone()));
    }
    if let Some(sd) = &up.sort_date {
        sets.push("sort_date = ?".into());
        binds.push(Box::new(sd.clone()));
    }
    if let Some(d) = image {
        sets.push("image = ?".into());
        binds.push(Box::new(d.bytes.clone()));
        sets.push("image_mime = ?".into());
        binds.push(Box::new(d.mime.clone()));
    }

    // Always bump updated_at.
    sets.push("updated_at = ?".into());
    binds.push(Box::new(now_iso()));

    binds.push(Box::new(id));

    let sql = format!("UPDATE favorites SET {} WHERE id = ?", sets.join(", "));
    let bind_refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let changed = tx
        .execute(&sql, bind_refs.as_slice())
        .map_err(AppError::from)?;

    tx.commit().map_err(AppError::from)?;

    Ok(changed > 0)
}

/// Merge a JSON `extra` patch object into the existing stored `extra` object.
fn merge_extra(existing: &str, patch: &str) -> AppResult<String> {
    let mut base: Map<String, Value> = serde_json::from_str(existing)
        .ok()
        .and_then(|v: Value| v.as_object().cloned())
        .unwrap_or_default();
    let patch_map: Map<String, Value> = serde_json::from_str(patch)
        .ok()
        .and_then(|v: Value| v.as_object().cloned())
        .unwrap_or_default();
    for (k, v) in patch_map {
        base.insert(k, v);
    }
    Ok(serde_json::to_string(&Value::Object(base))?)
}

/// Delete by id. Returns `false` if the row did not exist.
pub fn delete(conn: &Connection, id: i64) -> AppResult<bool> {
    let changed = conn
        .execute("DELETE FROM favorites WHERE id = ?", params![id])
        .map_err(AppError::from)?;
    Ok(changed > 0)
}

#[cfg(test)]
mod escape_like_tests {
    use super::escape_like;

    #[test]
    fn escapes_percent_and_underscore_and_backslash() {
        assert_eq!(escape_like("100%_done\\"), "100\\%\\_done\\\\");
        assert_eq!(escape_like("plain"), "plain");
    }
}

/// Fetch image bytes + mime for a favorite. Returns `None` if the row is
/// missing or has no image.
pub fn get_image(conn: &Connection, id: i64) -> AppResult<Option<(Vec<u8>, String)>> {
    let row: Option<(Option<Vec<u8>>, Option<String>)> = conn
        .query_row(
            "SELECT image, image_mime FROM favorites WHERE id = ?",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(AppError::from)?;

    match row {
        Some((Some(bytes), mime)) if !bytes.is_empty() => {
            Ok(Some((bytes, mime.unwrap_or_else(|| "image/jpeg".into()))))
        }
        _ => Ok(None),
    }
}

/// Replace the image for an existing favorite. Returns `false` if missing.
pub fn set_image(conn: &Connection, id: i64, image: &DecodedImage) -> AppResult<bool> {
    let changed = conn
        .execute(
            "UPDATE favorites SET image = ?, image_mime = ?, updated_at = ? WHERE id = ?",
            params![image.bytes, image.mime, now_iso(), id],
        )
        .map_err(AppError::from)?;
    Ok(changed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::DecodedImage;
    use crate::models::{ItemType, PreparedCreate, PreparedUpdate};
    use rusqlite::Connection;
    use serde_json::{json, Map, Value};

    /// Same schema DDL as db.rs, run against an in-memory connection so tests
    /// are fully self-contained.
    fn open_test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            r#"
CREATE TABLE IF NOT EXISTS favorites (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    type          TEXT NOT NULL CHECK (type IN ('game','movie','book')),
    name          TEXT NOT NULL,
    url           TEXT,
    aka           TEXT,
    genres        TEXT,
    release_date  TEXT,
    rating        REAL,
    summary       TEXT,
    extra         TEXT NOT NULL DEFAULT '{}',
    image         BLOB,
    image_mime    TEXT,
    sort_date     TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_favorites_type ON favorites(type);
CREATE INDEX IF NOT EXISTS idx_favorites_sort ON favorites(sort_date DESC, id DESC);
"#,
        )
        .expect("init schema");
        conn
    }

    /// Build a JSON body map from a serde_json object literal.
    fn body(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => panic!("body must be a JSON object"),
        }
    }

    fn make_create(v: Value) -> PreparedCreate {
        PreparedCreate::from_body(&body(v)).expect("prepared create")
    }

    fn make_update(v: Value) -> PreparedUpdate {
        PreparedUpdate::from_body(&body(v)).expect("prepared update")
    }

    fn png() -> DecodedImage {
        DecodedImage {
            bytes: vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3],
            mime: "image/png".into(),
        }
    }

    // ---- create + get ---------------------------------------------------

    #[test]
    fn create_then_get_round_trips_fields() {
        let conn = open_test_conn();
        let p = make_create(json!({
            "type": "movie",
            "name": "Inception",
            "summary": "A heist in dreams",
            "director": "Christopher Nolan",
            "rating": 8.8,
            "sort_date": "2010-07-16",
            "image_url": "https://example.com/poster.jpg",
        }));

        let id = create(&conn, &p, None).expect("create");
        assert!(id > 0);

        let got = get(&conn, id).expect("get").expect("row exists");
        assert_eq!(got.id, id);
        assert_eq!(got.item_type, "movie");
        assert_eq!(got.name, "Inception");
        assert_eq!(got.summary.as_deref(), Some("A heist in dreams"));
        assert_eq!(got.rating, Some(8.8));
        assert_eq!(got.sort_date, "2010-07-16");
        assert!(!got.has_image, "no image passed to create");

        // extra contents (folded top-level field).
        let extra: Value = serde_json::from_str(&got.extra).unwrap();
        assert_eq!(extra["director"], json!("Christopher Nolan"));
    }

    #[test]
    fn get_missing_returns_none() {
        let conn = open_test_conn();
        assert!(get(&conn, 999).expect("get").is_none());
    }

    // ---- list: filtering, search, pagination, ordering ------------------

    fn seed_mixed(conn: &Connection) {
        // sort_date chosen so ordering is deterministic.
        create(
            conn,
            &make_create(json!({
                "type": "game", "name": "Hades",
                "sort_date": "2020-09-17", "image_url": "https://e.com/a.png"
            })),
            None,
        )
        .unwrap();
        create(
            conn,
            &make_create(json!({
                "type": "movie", "name": "The Matrix",
                "sort_date": "1999-03-31", "image_url": "https://e.com/b.png"
            })),
            None,
        )
        .unwrap();
        create(
            conn,
            &make_create(json!({
                "type": "book", "name": "Matrix Reloaded Notes",
                "sort_date": "2021-01-01", "image_url": "https://e.com/c.png"
            })),
            None,
        )
        .unwrap();
        create(
            conn,
            &make_create(json!({
                "type": "movie", "name": "Interstellar",
                "sort_date": "2014-11-07", "image_url": "https://e.com/d.png"
            })),
            None,
        )
        .unwrap();
    }

    fn query(item_type: Option<ItemType>, q: Option<&str>, page: u32, per_page: u32) -> ListQuery {
        ListQuery {
            item_type,
            page,
            per_page,
            q: q.map(|s| s.to_string()),
        }
    }

    #[test]
    fn list_filters_by_item_type() {
        let conn = open_test_conn();
        seed_mixed(&conn);

        let (rows, total) = list(&conn, &query(Some(ItemType::Movie), None, 1, 50)).unwrap();
        assert_eq!(total, 2);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.item_type == "movie"));
    }

    #[test]
    fn list_q_is_case_insensitive_substring_on_name() {
        let conn = open_test_conn();
        seed_mixed(&conn);

        // lowercase query should match "The Matrix" and "Matrix Reloaded Notes".
        let (rows, total) = list(&conn, &query(None, Some("matrix"), 1, 50)).unwrap();
        assert_eq!(total, 2);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"The Matrix"));
        assert!(names.contains(&"Matrix Reloaded Notes"));
    }

    #[test]
    fn list_orders_by_sort_date_desc_then_id_desc() {
        let conn = open_test_conn();
        // Two rows with identical sort_date to exercise the id DESC tiebreaker.
        let id1 = create(
            &conn,
            &make_create(json!({
                "type": "game", "name": "First", "sort_date": "2022-01-01",
                "image_url": "https://e.com/a.png"
            })),
            None,
        )
        .unwrap();
        let id2 = create(
            &conn,
            &make_create(json!({
                "type": "game", "name": "Second", "sort_date": "2022-01-01",
                "image_url": "https://e.com/b.png"
            })),
            None,
        )
        .unwrap();
        let id3 = create(
            &conn,
            &make_create(json!({
                "type": "game", "name": "Newer", "sort_date": "2023-05-05",
                "image_url": "https://e.com/c.png"
            })),
            None,
        )
        .unwrap();

        let (rows, _) = list(&conn, &query(None, None, 1, 50)).unwrap();
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        // Newest sort_date first; then same sort_date by id DESC.
        assert_eq!(ids, vec![id3, id2, id1]);
    }

    #[test]
    fn list_paginates_with_correct_total() {
        let conn = open_test_conn();
        seed_mixed(&conn); // 4 rows total

        let (page1, total) = list(&conn, &query(None, None, 1, 2)).unwrap();
        assert_eq!(total, 4, "total counts all matching rows, not the page");
        assert_eq!(page1.len(), 2);

        let (page2, total2) = list(&conn, &query(None, None, 2, 2)).unwrap();
        assert_eq!(total2, 4);
        assert_eq!(page2.len(), 2);

        // No overlap between pages.
        let p1: Vec<i64> = page1.iter().map(|r| r.id).collect();
        let p2: Vec<i64> = page2.iter().map(|r| r.id).collect();
        assert!(p1.iter().all(|id| !p2.contains(id)));
    }

    #[test]
    fn list_q_escapes_like_wildcards() {
        let conn = open_test_conn();
        // Insert a name with a literal underscore plus a decoy that would match
        // if `_` were treated as a single-char wildcard.
        create(
            &conn,
            &make_create(json!({
                "type": "book", "name": "a_b", "sort_date": "2020-01-01",
                "image_url": "https://e.com/a.png"
            })),
            None,
        )
        .unwrap();
        create(
            &conn,
            &make_create(json!({
                "type": "book", "name": "aXb", "sort_date": "2020-01-02",
                "image_url": "https://e.com/b.png"
            })),
            None,
        )
        .unwrap();

        // Searching "a_b" must match only the literal "a_b", NOT "aXb".
        let (rows, total) = list(&conn, &query(None, Some("a_b"), 1, 50)).unwrap();
        assert_eq!(total, 1, "underscore must be escaped, not a wildcard");
        assert_eq!(rows[0].name, "a_b");

        // A literal percent likewise must not act as a wildcard.
        create(
            &conn,
            &make_create(json!({
                "type": "book", "name": "50% off", "sort_date": "2020-01-03",
                "image_url": "https://e.com/c.png"
            })),
            None,
        )
        .unwrap();
        let (rows, total) = list(&conn, &query(None, Some("50%"), 1, 50)).unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0].name, "50% off");
    }

    // ---- update ---------------------------------------------------------

    #[test]
    fn update_changes_only_given_columns() {
        let mut conn = open_test_conn();
        let id = create(
            &conn,
            &make_create(json!({
                "type": "game", "name": "Original", "summary": "keep me",
                "sort_date": "2020-01-01", "image_url": "https://e.com/a.png"
            })),
            None,
        )
        .unwrap();

        let up = make_update(json!({ "name": "Renamed" }));
        let ok = update(&mut conn, id, &up, None).unwrap();
        assert!(ok);

        let got = get(&conn, id).unwrap().unwrap();
        assert_eq!(got.name, "Renamed");
        assert_eq!(got.summary.as_deref(), Some("keep me"), "untouched column");
        assert_eq!(got.item_type, "game");
    }

    #[test]
    fn update_merges_extra_with_existing() {
        let mut conn = open_test_conn();
        let id = create(
            &conn,
            &make_create(json!({
                "type": "book", "name": "Lib", "extra": { "a": 1 },
                "sort_date": "2020-01-01", "image_url": "https://e.com/a.png"
            })),
            None,
        )
        .unwrap();

        let up = make_update(json!({ "extra": { "b": 2 } }));
        assert!(update(&mut conn, id, &up, None).unwrap());

        let got = get(&conn, id).unwrap().unwrap();
        let extra: Value = serde_json::from_str(&got.extra).unwrap();
        assert_eq!(extra["a"], json!(1), "existing extra key preserved");
        assert_eq!(extra["b"], json!(2), "patched extra key merged in");
    }

    #[test]
    fn update_bumps_updated_at() {
        let mut conn = open_test_conn();
        let id = create(
            &conn,
            &make_create(json!({
                "type": "game", "name": "X", "sort_date": "2020-01-01",
                "image_url": "https://e.com/a.png"
            })),
            None,
        )
        .unwrap();
        let before = get(&conn, id).unwrap().unwrap().updated_at;

        // Force a measurable difference in the timestamp.
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(update(&mut conn, id, &make_update(json!({ "name": "Y" })), None).unwrap());

        let after = get(&conn, id).unwrap().unwrap().updated_at;
        assert_ne!(before, after, "updated_at must change");
    }

    #[test]
    fn update_missing_id_returns_false() {
        let mut conn = open_test_conn();
        let ok = update(&mut conn, 4242, &make_update(json!({ "name": "Z" })), None).unwrap();
        assert!(!ok);
    }

    // ---- delete ---------------------------------------------------------

    #[test]
    fn delete_existing_then_gone() {
        let conn = open_test_conn();
        let id = create(
            &conn,
            &make_create(json!({
                "type": "movie", "name": "Gone", "sort_date": "2020-01-01",
                "image_url": "https://e.com/a.png"
            })),
            None,
        )
        .unwrap();

        assert!(delete(&conn, id).unwrap());
        assert!(get(&conn, id).unwrap().is_none());
    }

    #[test]
    fn delete_missing_returns_false() {
        let conn = open_test_conn();
        assert!(!delete(&conn, 12345).unwrap());
    }

    // ---- image get/set --------------------------------------------------

    #[test]
    fn set_image_then_get_image_round_trips() {
        let conn = open_test_conn();
        let id = create(
            &conn,
            &make_create(json!({
                "type": "game", "name": "Pic", "sort_date": "2020-01-01",
                "image_url": "https://e.com/a.png"
            })),
            None,
        )
        .unwrap();

        // No image stored yet.
        assert!(get_image(&conn, id).unwrap().is_none());

        let img = png();
        assert!(set_image(&conn, id, &img).unwrap());

        let (bytes, mime) = get_image(&conn, id).unwrap().expect("image present");
        assert_eq!(bytes, img.bytes);
        assert_eq!(mime, "image/png");

        // has_image now reflected in the DTO projection.
        assert!(get(&conn, id).unwrap().unwrap().has_image);
    }

    #[test]
    fn set_image_missing_row_returns_false() {
        let conn = open_test_conn();
        assert!(!set_image(&conn, 777, &png()).unwrap());
    }

    #[test]
    fn get_image_missing_row_returns_none() {
        let conn = open_test_conn();
        assert!(get_image(&conn, 777).unwrap().is_none());
    }
}
