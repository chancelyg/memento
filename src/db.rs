//! SQLite connection pool and idempotent schema initialisation.

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;

use crate::error::{AppError, AppResult};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};

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
    // Check foreign/future schemas before any persistent PRAGMA can touch them.
    if db_path == ":memory:" {
        // SQLite's special connection-local database name is not a filesystem path.
    } else if std::path::Path::new(db_path).exists() {
        let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        schema_version(&conn)?;
    } else {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(db_path)
            .map_err(|e| AppError::Database(Box::new(e)))?;
    }
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
    let mut conn = pool.get().map_err(AppError::from)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = schema_version(&tx)?;
    if version == 0 {
        tx.execute_batch(SCHEMA)?;
        tx.execute_batch(
            r#"
CREATE TABLE IF NOT EXISTS memento_migrations(version INTEGER PRIMARY KEY);
CREATE TABLE diaries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content TEXT NOT NULL,
    create_date TEXT NOT NULL,
    created_at TEXT,
    updated_at TEXT,
    version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0)
);
CREATE INDEX idx_diaries_date ON diaries(create_date DESC, id DESC);
CREATE TABLE browser_sessions (
    token_hash TEXT PRIMARY KEY,
    csrf_token TEXT NOT NULL,
    credential_hash TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX idx_sessions_expiry ON browser_sessions(expires_at);
CREATE TABLE diary_imports (
    source_id INTEGER PRIMARY KEY,
    diary_id INTEGER REFERENCES diaries(id) ON DELETE SET NULL,
    fingerprint TEXT NOT NULL
);
INSERT INTO memento_migrations(version) VALUES(1);
"#,
        )?;
    }
    if version < 2 {
        // Preserve all v1 data, including old import tombstones and ID sequences.
        // DDL and the version record must commit together or roll back together.
        tx.execute_batch(
            "ALTER TABLE diaries ADD COLUMN deleted_at TEXT;
             DROP INDEX IF EXISTS idx_diaries_date;
             CREATE INDEX idx_diaries_active_date ON diaries(create_date DESC, id DESC)
                 WHERE deleted_at IS NULL;
             INSERT INTO memento_migrations(version) VALUES(2);",
        )?;
    }
    if version < 3 {
        tx.execute_batch(
            "CREATE TABLE browser_totp_state (
                 credential_hash TEXT PRIMARY KEY,
                 last_used_step INTEGER NOT NULL
             );
             INSERT INTO memento_migrations(version) VALUES(3);",
        )?;
    }
    schema_version(&tx)?;
    tx.commit()?;
    Ok(())
}

fn invalid_schema() -> AppError {
    AppError::BadRequest("incompatible database schema; use a separate initialized memento database for diary import".into())
}

fn validate_favorites(conn: &Connection) -> AppResult<()> {
    // The legacy collection schema is unchanged; a familiar table name alone
    // must not authorize upgrading an unrelated or incomplete database.
    let expected = [
        ("id", "INTEGER", 0, 1),
        ("type", "TEXT", 1, 0),
        ("name", "TEXT", 1, 0),
        ("url", "TEXT", 0, 0),
        ("aka", "TEXT", 0, 0),
        ("genres", "TEXT", 0, 0),
        ("release_date", "TEXT", 0, 0),
        ("rating", "REAL", 0, 0),
        ("summary", "TEXT", 0, 0),
        ("extra", "TEXT", 1, 0),
        ("image", "BLOB", 0, 0),
        ("image_mime", "TEXT", 0, 0),
        ("sort_date", "TEXT", 1, 0),
        ("created_at", "TEXT", 1, 0),
        ("updated_at", "TEXT", 1, 0),
    ];
    let actual: Vec<(String, String, i64, i64)> = conn
        .prepare(
            "SELECT name,type,\"notnull\",pk FROM pragma_table_info('favorites') ORDER BY cid",
        )?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    if actual.len() != expected.len()
        || actual.iter().zip(expected).any(|(actual, expected)| {
            actual.0 != expected.0
                || actual.1.to_uppercase() != expected.1
                || actual.2 != expected.2
                || actual.3 != expected.3
        })
    {
        return Err(invalid_schema());
    }
    Ok(())
}

fn schema_version(conn: &Connection) -> AppResult<i64> {
    let has_migrations: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='memento_migrations' AND type='table')",
        [], |row| row.get(0),
    )?;
    let versions: Vec<i64> = if has_migrations {
        conn.prepare("SELECT version FROM memento_migrations ORDER BY version")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    } else {
        Vec::new()
    };
    let version = match versions.as_slice() {
        [] => 0,
        [1] => 1,
        [1, 2] => 2,
        [1, 2, 3] => 3,
        _ => return Err(invalid_schema()),
    };
    if version == 0 {
        let tables: Vec<(String,String)> = conn
            .prepare("SELECT name,type FROM sqlite_master WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%'")?
            .query_map([], |row| Ok((row.get(0)?,row.get(1)?)))?
            .collect::<Result<_,_>>()?;
        for (name, kind) in tables {
            if kind != "table" {
                return Err(invalid_schema());
            }
            match name.as_str() {
                "favorites" => validate_favorites(conn)?,
                "memento_migrations" => {}
                _ => return Err(invalid_schema()),
            }
        }
        return Ok(0);
    }
    validate_favorites(conn)?;
    // A version marker is not proof that a restored database is complete.
    let diary_columns = [
        "id",
        "content",
        "create_date",
        "created_at",
        "updated_at",
        "version",
        "deleted_at",
    ];
    for (table, columns) in [
        (
            "diaries",
            &diary_columns[..if version == 1 { 6 } else { 7 }],
        ),
        (
            "browser_sessions",
            &["token_hash", "csrf_token", "credential_hash", "expires_at"][..],
        ),
        (
            "diary_imports",
            &["source_id", "diary_id", "fingerprint"][..],
        ),
    ] {
        let actual: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")?
            .query_map([table], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        if actual != columns {
            return Err(invalid_schema());
        }
    }
    if version >= 2 {
        let (kind, required, default, primary): (String, i64, Option<String>, i64) = conn.query_row(
            "SELECT type, \"notnull\", dflt_value, pk FROM pragma_table_info('diaries') WHERE name='deleted_at'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        if kind.to_uppercase() != "TEXT" || required != 0 || default.is_some() || primary != 0 {
            return Err(invalid_schema());
        }
    }
    if version >= 3 {
        let is_table: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='browser_totp_state')",
            [], |row| row.get(0),
        )?;
        let actual: Vec<(String, String, i64, i64)> = conn
            .prepare("SELECT name,type,\"notnull\",pk FROM pragma_table_info('browser_totp_state') ORDER BY cid")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
            .collect::<Result<_, _>>()?;
        let expected = [
            ("credential_hash", "TEXT", 0, 1),
            ("last_used_step", "INTEGER", 1, 0),
        ];
        if !is_table
            || actual.len() != expected.len()
            || actual.iter().zip(expected).any(|(actual, expected)| {
                actual.0 != expected.0
                    || actual.1.to_uppercase() != expected.1
                    || actual.2 != expected.2
                    || actual.3 != expected.3
            })
        {
            return Err(invalid_schema());
        }
    }
    let favorites: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='favorites')",
        [],
        |row| row.get(0),
    )?;
    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='diaries'",
        [],
        |row| row.get(0),
    )?;
    let normalized: String = sql
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_uppercase)
        .collect();
    let foreign_key: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list('diary_imports') WHERE \"table\"='diaries' AND \"from\"='diary_id' AND \"to\"='id' AND on_delete='SET NULL')", [], |row| row.get(0))?;
    if !favorites
        || !foreign_key
        || !normalized.contains("AUTOINCREMENT")
        || !normalized.contains("CHECK(VERSION>0)")
    {
        return Err(invalid_schema());
    }
    Ok(version)
}
