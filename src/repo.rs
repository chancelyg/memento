//! Pure data access for the `favorites` table. All functions take a pooled
//! connection and perform synchronous rusqlite work; callers run them inside
//! `spawn_blocking`.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{Map, Value};

use crate::error::{AppError, AppResult};
use crate::image::DecodedImage;
use crate::models::{now_iso, Favorite, ItemType, PreparedCreate, PreparedUpdate};

/// Columns selected for the DTO projection (no blob).
const SELECT_COLS: &str = "id, type, name, url, aka, genres, release_date, rating, summary, \
     extra, (image IS NOT NULL) AS has_image, sort_date, created_at, updated_at";

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
    let sql = format!("SELECT {SELECT_COLS} FROM favorites WHERE id = ?");
    conn.query_row(&sql, params![id], row_to_favorite)
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
    conn: &Connection,
    id: i64,
    up: &PreparedUpdate,
    image: Option<&DecodedImage>,
) -> AppResult<bool> {
    // Ensure the row exists and grab current extra for merging.
    let existing_extra: Option<String> = conn
        .query_row("SELECT extra FROM favorites WHERE id = ?", params![id], |r| {
            r.get(0)
        })
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
    let changed = conn
        .execute(&sql, bind_refs.as_slice())
        .map_err(AppError::from)?;

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
