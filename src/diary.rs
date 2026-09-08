//! Diary validation and transactional SQLite operations, independent of HTTP.

use chrono::{Datelike, Days, NaiveDate, SecondsFormat, Utc};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

const MAX_CONTENT_CHARS: usize = 10_000;
const COLUMNS: &str = "id, content, create_date, created_at, updated_at, version";
const FILTER: &str = "deleted_at IS NULL AND (?1 IS NULL OR content LIKE ?1 ESCAPE '\\') \
    AND (?2 IS NULL OR create_date >= ?2) AND (?3 IS NULL OR create_date <= ?3)";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentPayload {
    pub content: String,
}

pub struct PreparedContent(String);

impl PreparedContent {
    pub fn from_payload(payload: ContentPayload) -> AppResult<Self> {
        let content = payload.content.trim();
        if content.is_empty() || content.chars().count() > MAX_CONTENT_CHARS {
            return Err(AppError::BadRequest(
                "content must contain between 1 and 10000 characters".into(),
            ));
        }
        Ok(Self(content.to_string()))
    }
}

#[derive(Serialize)]
pub struct DiaryDto {
    pub id: i64,
    pub content: String,
    pub create_date: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub version: i64,
}

#[derive(Default, Deserialize)]
pub struct ListParams {
    pub page: Option<u64>,
    pub per_page: Option<u64>,
    pub q: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub sort: Option<String>,
}

pub struct ListQuery {
    page: u64,
    per_page: u64,
    offset: i64,
    pattern: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
    ascending: bool,
}

impl ListQuery {
    pub fn from_params(params: ListParams) -> AppResult<Self> {
        let page = params.page.unwrap_or(1);
        let per_page = params.per_page.unwrap_or(24).min(100);
        if page == 0 || per_page == 0 {
            return Err(AppError::BadRequest(
                "page and per_page must be positive".into(),
            ));
        }
        let offset = (page - 1)
            .checked_mul(per_page)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| AppError::BadRequest("pagination offset is too large".into()))?;
        let ascending = match params.sort.as_deref() {
            None | Some("desc") => false,
            Some("asc") => true,
            _ => return Err(AppError::BadRequest("sort must be asc or desc".into())),
        };
        for date in [params.start_date.as_deref(), params.end_date.as_deref()]
            .into_iter()
            .flatten()
        {
            if parse_date(date).is_none() {
                return Err(AppError::BadRequest(
                    "dates must use valid YYYY-MM-DD values".into(),
                ));
            }
        }
        if let (Some(start), Some(end)) = (&params.start_date, &params.end_date) {
            if start > end {
                return Err(AppError::BadRequest(
                    "start_date must not follow end_date".into(),
                ));
            }
        }
        let pattern = match params.q {
            Some(q) if !q.is_empty() => {
                if q.chars().count() > MAX_CONTENT_CHARS {
                    return Err(AppError::BadRequest(
                        "q must not exceed 10000 characters".into(),
                    ));
                }
                let escaped = q
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                Some(format!("%{escaped}%"))
            }
            _ => None,
        };
        Ok(Self {
            page,
            per_page,
            offset,
            pattern,
            start_date: params.start_date,
            end_date: params.end_date,
            ascending,
        })
    }
}

#[derive(Serialize)]
pub struct ListResponse {
    pub items: Vec<DiaryDto>,
    pub total: i64,
    pub page: u64,
    pub per_page: u64,
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    if value.len() != 10
        || !value.bytes().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
    {
        return None;
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .filter(|date| (1..=9999).contains(&date.year()))
}

fn next_date(maximum: Option<&str>, today: NaiveDate) -> AppResult<NaiveDate> {
    let date = match maximum {
        Some(value) => parse_date(value)
            .ok_or_else(|| {
                AppError::Internal(Box::new(std::io::Error::other("invalid stored diary date")))
            })?
            .checked_add_days(Days::new(1)),
        None => Some(today),
    };
    date.filter(|date| (1..=9999).contains(&date.year()))
        .ok_or_else(|| AppError::Conflict("next diary date is outside the supported range".into()))
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<DiaryDto> {
    Ok(DiaryDto {
        id: row.get(0)?,
        content: row.get(1)?,
        create_date: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        version: row.get(5)?,
    })
}

pub fn get(conn: &Connection, id: i64) -> AppResult<DiaryDto> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM diaries WHERE id = ?1 AND deleted_at IS NULL"),
        [id],
        from_row,
    )
    .optional()?
    .ok_or(AppError::NotFound)
}

pub fn list(conn: &mut Connection, query: &ListQuery) -> AppResult<ListResponse> {
    // The count establishes a read snapshot reused by the paginated query.
    let tx = conn.transaction()?;
    let total = tx.query_row(
        &format!("SELECT COUNT(*) FROM diaries WHERE {FILTER}"),
        params![query.pattern, query.start_date, query.end_date],
        |row| row.get(0),
    )?;
    let direction = if query.ascending { "ASC" } else { "DESC" };
    let items = {
        let mut statement = tx.prepare(&format!(
            "SELECT {COLUMNS} FROM diaries WHERE {FILTER} \
             ORDER BY create_date {direction}, id {direction} LIMIT ?4 OFFSET ?5"
        ))?;
        let rows = statement.query_map(
            params![
                query.pattern,
                query.start_date,
                query.end_date,
                query.per_page as i64,
                query.offset
            ],
            from_row,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    tx.commit()?;
    Ok(ListResponse {
        items,
        total,
        page: query.page,
        per_page: query.per_page,
    })
}

pub fn create(
    conn: &mut Connection,
    content: &PreparedContent,
    timezone: Tz,
) -> AppResult<DiaryDto> {
    // Lock before reading MAX to avoid deferred-transaction snapshot upgrades.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let maximum: Option<String> = tx.query_row(
        "SELECT MAX(create_date) FROM diaries WHERE deleted_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    let now = Utc::now();
    let date = next_date(
        maximum.as_deref(),
        now.with_timezone(&timezone).date_naive(),
    )?;
    let timestamp = now.to_rfc3339_opts(SecondsFormat::Millis, true);
    tx.execute(
        "INSERT INTO diaries (content, create_date, created_at, updated_at, version) \
         VALUES (?1, ?2, ?3, ?3, 1)",
        params![content.0, date.to_string(), timestamp],
    )?;
    let dto = get(&tx, tx.last_insert_rowid())?;
    tx.commit()?;
    Ok(dto)
}

fn check_version(conn: &Connection, id: i64, expected: Option<i64>) -> AppResult<i64> {
    let actual: i64 = conn
        .query_row(
            "SELECT version FROM diaries WHERE id = ?1 AND deleted_at IS NULL",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AppError::NotFound)?;
    if expected.is_some_and(|expected| actual != expected) {
        return Err(AppError::PreconditionFailed);
    }
    Ok(actual)
}

pub fn update(
    conn: &mut Connection,
    id: i64,
    expected: Option<i64>,
    content: &PreparedContent,
) -> AppResult<DiaryDto> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let expected = check_version(&tx, id, expected)?;
    let next_version = expected
        .checked_add(1)
        .ok_or_else(|| AppError::Conflict("diary version is outside the supported range".into()))?;
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let changed = tx.execute(
        "UPDATE diaries SET content = ?1, updated_at = ?2, version = ?3 WHERE id = ?4 AND version = ?5 AND deleted_at IS NULL",
        params![content.0, timestamp, next_version, id, expected],
    )?;
    if changed == 0 {
        return Err(AppError::PreconditionFailed);
    }
    let dto = get(&tx, id)?;
    tx.commit()?;
    Ok(dto)
}

pub fn delete(conn: &mut Connection, id: i64, expected: Option<i64>) -> AppResult<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let expected = check_version(&tx, id, expected)?;
    let next_version = expected
        .checked_add(1)
        .ok_or_else(|| AppError::Conflict("diary version is outside the supported range".into()))?;
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    // Preserve the row and import association; normal reads never expose tombstones.
    if tx.execute(
        "UPDATE diaries SET deleted_at = ?1, updated_at = ?1, version = ?2 \
         WHERE id = ?3 AND version = ?4 AND deleted_at IS NULL",
        params![timestamp, next_version, id, expected],
    )? == 0
    {
        return Err(AppError::PreconditionFailed);
    }
    tx.commit()?;
    Ok(())
}
