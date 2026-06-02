//! SQLite connection pool and idempotent schema initialisation.

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;

use crate::error::{AppError, AppResult};

/// Type alias for the connection pool used across the app.
pub type DbPool = Pool<SqliteConnectionManager>;

/// Schema DDL — idempotent (`IF NOT EXISTS`), safe to run on every startup.
const SCHEMA: &str = r#"
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
"#;

/// Build the connection pool for the given SQLite file path.
///
/// Enables WAL journaling and foreign-key enforcement on each connection.
pub fn build_pool(db_path: &str) -> AppResult<DbPool> {
    let manager = SqliteConnectionManager::file(db_path).with_init(|conn| {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;\
             PRAGMA foreign_keys = ON;\
             PRAGMA busy_timeout = 5000;",
        )
    });

    let pool = Pool::builder()
        .max_size(8)
        .build(manager)
        .map_err(AppError::from)?;

    Ok(pool)
}

/// Run the schema DDL. Idempotent — call once at startup.
pub fn init_schema(pool: &DbPool) -> AppResult<()> {
    let conn = pool.get().map_err(AppError::from)?;
    conn.execute_batch(SCHEMA).map_err(AppError::from)?;
    Ok(())
}
