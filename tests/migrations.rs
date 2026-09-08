//! Schema upgrades must preserve existing collections and refuse foreign databases.
use memento::db;

#[test]
fn fresh_schema_has_full_v3_history_and_rejects_future_version() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let pool = db::build_pool(file.path().to_str().unwrap()).unwrap();
    db::init_schema(&pool).unwrap();
    let conn = pool.get().unwrap();
    assert_eq!(
        table_values(&conn, "memento_migrations"),
        (1..=3)
            .map(|version| vec![rusqlite::types::Value::Integer(version)])
            .collect::<Vec<_>>()
    );
    assert!(table_values(&conn, "browser_totp_state").is_empty());
    conn.execute("INSERT INTO memento_migrations VALUES(4)", [])
        .unwrap();
    let before = table_values(&conn, "memento_migrations");
    assert!(db::init_schema(&pool).is_err());
    assert_eq!(table_values(&conn, "memento_migrations"), before);
    drop(conn);
    drop(pool);
    assert!(db::build_pool(file.path().to_str().unwrap()).is_err());
}

#[test]
fn upgrades_legacy_and_is_idempotent() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let pool = db::build_pool(file.path().to_str().unwrap()).unwrap();
    let conn = pool.get().unwrap();
    conn.execute_batch("CREATE TABLE favorites (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        type TEXT NOT NULL CHECK(type IN ('game','movie','book')),
        name TEXT NOT NULL,url TEXT,aka TEXT,genres TEXT,release_date TEXT,rating REAL,
        summary TEXT,extra TEXT NOT NULL DEFAULT '{}',image BLOB,image_mime TEXT,
        sort_date TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
        INSERT INTO favorites VALUES(7,'book','keep','https://example.test','[]','[]','2024',9.5,
        'keep summary','{\"author\":\"synthetic\"}',X'010203','image/png','2024-01-01','created','updated');").unwrap();
    let before: Vec<rusqlite::types::Value> = conn
        .query_row("SELECT * FROM favorites WHERE id=7", [], |row| {
            (0..15).map(|i| row.get(i)).collect()
        })
        .unwrap();
    db::init_schema(&pool).unwrap();
    db::init_schema(&pool).unwrap();
    let after: Vec<rusqlite::types::Value> = conn
        .query_row("SELECT * FROM favorites WHERE id=7", [], |row| {
            (0..15).map(|i| row.get(i)).collect()
        })
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        conn.query_row("SELECT name FROM favorites WHERE id=7", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "keep"
    );
    assert_eq!(
        conn.query_row("SELECT max(version) FROM memento_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        3
    );
    for table in [
        "diaries",
        "browser_sessions",
        "diary_imports",
        "browser_totp_state",
    ] {
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name=?1 AND type='table'",
                [table],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
}

#[test]
fn refuses_old_diary_database_without_modifying_schema() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let pool = db::build_pool(file.path().to_str().unwrap()).unwrap();
    let conn = pool.get().unwrap();
    conn.execute_batch(
        "CREATE TABLE diaries(id INTEGER PRIMARY KEY, content TEXT, create_date TEXT);",
    )
    .unwrap();
    assert!(db::init_schema(&pool).is_err());
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn refuses_future_schema() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let pool = db::build_pool(file.path().to_str().unwrap()).unwrap();
    let conn = pool.get().unwrap();
    conn.execute_batch("CREATE TABLE memento_migrations(version INTEGER PRIMARY KEY); INSERT INTO memento_migrations VALUES(4);").unwrap();
    assert!(db::init_schema(&pool).is_err());
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='favorites'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn rejects_incomplete_and_invalid_version_states() {
    for versions in [
        "1",
        "2",
        "3",
        "1),(2",
        "1),(3",
        "1),(2),(3",
        "1),(2),(3),(4",
        "-1",
        "0),(1",
    ] {
        let file = tempfile::NamedTempFile::new().unwrap();
        let pool = db::build_pool(file.path().to_str().unwrap()).unwrap();
        let conn = pool.get().unwrap();
        conn.execute_batch(&format!("CREATE TABLE memento_migrations(version INTEGER PRIMARY KEY); INSERT INTO memento_migrations VALUES({versions});")).unwrap();
        assert!(db::init_schema(&pool).is_err());
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='favorites'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}

// A real v1 fixture, not a v2 schema mislabeled by editing its version marker.
fn create_v1(conn: &rusqlite::Connection) {
    conn.execute_batch("PRAGMA foreign_keys=ON;
        CREATE TABLE favorites (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            type TEXT NOT NULL CHECK(type IN ('game','movie','book')),
            name TEXT NOT NULL,url TEXT,aka TEXT,genres TEXT,release_date TEXT,rating REAL,
            summary TEXT,extra TEXT NOT NULL DEFAULT '{}',image BLOB,image_mime TEXT,
            sort_date TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
        INSERT INTO favorites(id,type,name,sort_date,created_at,updated_at,image) VALUES(7,'book','keep','2024-01-01','old','old',X'0102');
        CREATE TABLE memento_migrations(version INTEGER PRIMARY KEY);
        INSERT INTO memento_migrations VALUES(1);
        CREATE TABLE diaries(id INTEGER PRIMARY KEY AUTOINCREMENT,content TEXT NOT NULL,create_date TEXT NOT NULL,created_at TEXT,updated_at TEXT,version INTEGER NOT NULL DEFAULT 1 CHECK(version>0));
        CREATE INDEX idx_diaries_date ON diaries(create_date DESC,id DESC);
        CREATE TABLE browser_sessions(token_hash TEXT PRIMARY KEY,csrf_token TEXT NOT NULL,credential_hash TEXT NOT NULL,expires_at INTEGER NOT NULL);
        INSERT INTO browser_sessions VALUES('synthetic token digest','synthetic csrf','synthetic credential digest',123456);
        CREATE INDEX idx_sessions_expiry ON browser_sessions(expires_at);
        CREATE TABLE diary_imports(source_id INTEGER PRIMARY KEY,diary_id INTEGER REFERENCES diaries(id) ON DELETE SET NULL,fingerprint TEXT NOT NULL);
        INSERT INTO diaries VALUES(1,'retained','2020-01-01','opaque legacy time',NULL,9);
        INSERT INTO diaries VALUES(2,'','2020-01-01',NULL,NULL,1);
        INSERT INTO diaries VALUES(50,'historical deletion','2020-01-02',NULL,NULL,1);
        INSERT INTO diary_imports VALUES(1,1,'first fingerprint'),(50,50,'deleted fingerprint');
        DELETE FROM diaries WHERE id=50;").unwrap();
}

fn table_values(conn: &rusqlite::Connection, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
        .unwrap();
    let count = statement.column_count();
    statement
        .query_map([], |row| (0..count).map(|i| row.get(i)).collect())
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn create_v2(conn: &rusqlite::Connection) {
    create_v1(conn);
    conn.execute_batch("ALTER TABLE diaries ADD COLUMN deleted_at TEXT;
        DROP INDEX idx_diaries_date;
        CREATE INDEX idx_diaries_active_date ON diaries(create_date DESC,id DESC) WHERE deleted_at IS NULL;
        INSERT INTO memento_migrations VALUES(2);
        UPDATE diaries SET deleted_at='2026-09-08T00:00:00.000Z',updated_at='2026-09-08T00:00:00.000Z',version=10 WHERE id=1;").unwrap();
}

#[test]
fn upgrades_real_v2_and_preserves_totp_marker_across_reinitialization() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    create_v2(&conn);
    let before = [
        "diaries",
        "favorites",
        "browser_sessions",
        "diary_imports",
        "sqlite_sequence",
    ]
    .map(|table| (table, table_values(&conn, table)));
    drop(conn);
    let pool = db::build_pool(file.path().to_str().unwrap()).unwrap();
    db::init_schema(&pool).unwrap();
    let conn = pool.get().unwrap();
    for (table, rows) in &before {
        assert_eq!(table_values(&conn, table), *rows, "changed {table}");
    }
    assert_eq!(
        table_values(&conn, "memento_migrations"),
        (1..=3)
            .map(|version| vec![rusqlite::types::Value::Integer(version)])
            .collect::<Vec<_>>()
    );
    assert!(table_values(&conn, "browser_totp_state").is_empty());
    conn.execute(
        "INSERT INTO browser_totp_state VALUES('synthetic digest',12345)",
        [],
    )
    .unwrap();
    let marker = table_values(&conn, "browser_totp_state");
    db::init_schema(&pool).unwrap();
    assert_eq!(table_values(&conn, "browser_totp_state"), marker);
    drop(conn);
    drop(pool);
    let pool = db::build_pool(file.path().to_str().unwrap()).unwrap();
    db::init_schema(&pool).unwrap();
    let conn = pool.get().unwrap();
    assert_eq!(table_values(&conn, "browser_totp_state"), marker);
    for (table, rows) in before {
        assert_eq!(table_values(&conn, table), rows, "changed {table}");
    }
}

#[test]
fn v3_migration_failure_rolls_back_table_and_version() {
    for version in [1, 2] {
        let file = tempfile::NamedTempFile::new().unwrap();
        let conn = rusqlite::Connection::open(file.path()).unwrap();
        if version == 1 {
            create_v1(&conn);
        } else {
            create_v2(&conn);
        }
        conn.execute_batch("CREATE TRIGGER reject_v3 BEFORE INSERT ON memento_migrations WHEN NEW.version=3 BEGIN SELECT RAISE(ABORT,'controlled migration failure'); END;").unwrap();
        let before = [
            "diaries",
            "favorites",
            "browser_sessions",
            "diary_imports",
            "sqlite_sequence",
            "memento_migrations",
        ]
        .map(|table| (table, table_values(&conn, table)));
        drop(conn);
        let pool = db::build_pool(file.path().to_str().unwrap()).unwrap();
        assert!(db::init_schema(&pool).is_err());
        let conn = pool.get().unwrap();
        for (table, rows) in before {
            assert_eq!(table_values(&conn, table), rows, "changed {table}");
        }
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='browser_totp_state'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}

#[test]
fn rejects_incomplete_v3_totp_metadata_before_wal() {
    for definition in [
        "",
        "CREATE TABLE browser_totp_state(credential_hash TEXT,last_used_step INTEGER NOT NULL)",
        "CREATE TABLE browser_totp_state(credential_hash INTEGER PRIMARY KEY,last_used_step INTEGER NOT NULL)",
        "CREATE TABLE browser_totp_state(credential_hash TEXT PRIMARY KEY NOT NULL,last_used_step INTEGER NOT NULL)",
        "CREATE TABLE browser_totp_state(credential_hash TEXT PRIMARY KEY,last_used_step INTEGER)",
        "CREATE TABLE browser_totp_state(credential_hash TEXT PRIMARY KEY,last_used_step TEXT NOT NULL)",
        "CREATE TABLE browser_totp_state(credential_hash TEXT PRIMARY KEY,wrong_step INTEGER NOT NULL)",
        "CREATE TABLE browser_totp_state(credential_hash TEXT PRIMARY KEY,last_used_step INTEGER NOT NULL,extra TEXT)",
        "CREATE VIEW browser_totp_state AS SELECT 'synthetic' AS credential_hash,1 AS last_used_step",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        create_v2(&conn);
        conn.execute_batch(definition).unwrap();
        conn.execute("INSERT INTO memento_migrations VALUES(3)", []).unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        assert!(db::build_pool(path.to_str().unwrap()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!dir.path().join("broken.db-wal").exists());
    }
}

#[test]
fn upgrades_real_v1_preserving_rows_sessions_ledger_and_id_history() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    create_v1(&conn);
    let before_diaries = table_values(&conn, "diaries");
    let related: Vec<_> = [
        "favorites",
        "browser_sessions",
        "diary_imports",
        "sqlite_sequence",
    ]
    .map(|table| (table, table_values(&conn, table)))
    .into();
    drop(conn);
    let pool = db::build_pool(file.path().to_str().unwrap()).unwrap();
    db::init_schema(&pool).unwrap();
    let conn = pool.get().unwrap();
    let rows = table_values(&conn, "diaries");
    for (old, new) in before_diaries.iter().zip(&rows) {
        assert_eq!(old.as_slice(), &new[..6]);
        assert_eq!(new[6], rusqlite::types::Value::Null);
    }
    assert_eq!(rows.len(), before_diaries.len());
    for (table, before) in related {
        assert_eq!(table_values(&conn, table), before, "changed {table}");
    }
    assert_eq!(
        table_values(&conn, "memento_migrations"),
        vec![
            vec![rusqlite::types::Value::Integer(1)],
            vec![rusqlite::types::Value::Integer(2)],
            vec![rusqlite::types::Value::Integer(3)]
        ]
    );
    conn.execute(
        "UPDATE diaries SET deleted_at='2026-01-01T00:00:00.000Z',version=10 WHERE id=1",
        [],
    )
    .unwrap();
    let marked = table_values(&conn, "diaries");
    db::init_schema(&pool).unwrap();
    assert_eq!(table_values(&conn, "diaries"), marked);
    drop(conn);
    drop(pool);
    let reopened = db::build_pool(file.path().to_str().unwrap()).unwrap();
    db::init_schema(&reopened).unwrap();
    let conn = reopened.get().unwrap();
    assert_eq!(table_values(&conn, "diaries"), marked);
    conn.execute(
        "INSERT INTO diaries(content,create_date) VALUES('next','2020-01-02')",
        [],
    )
    .unwrap();
    assert_eq!(conn.last_insert_rowid(), 51);
}

#[test]
fn v2_migration_failure_rolls_back_added_column_and_version() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    create_v1(&conn);
    conn.execute_batch("CREATE TRIGGER reject_v2 BEFORE INSERT ON memento_migrations WHEN NEW.version=2 BEGIN SELECT RAISE(ABORT,'controlled migration failure'); END;").unwrap();
    let before = table_values(&conn, "diaries");
    drop(conn);
    let pool = db::build_pool(file.path().to_str().unwrap()).unwrap();
    assert!(db::init_schema(&pool).is_err());
    let conn = pool.get().unwrap();
    assert_eq!(table_values(&conn, "diaries"), before);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM pragma_table_info('diaries') WHERE name='deleted_at'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT max(version) FROM memento_migrations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='idx_diaries_date'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn rejects_mislabeled_v2_and_invalid_deletion_column_before_wal() {
    for column in [
        None,
        Some("deleted_at INTEGER"),
        Some("deleted_at TEXT NOT NULL DEFAULT ''"),
        Some("deleted_at TEXT DEFAULT 'hidden'"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broken.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        create_v1(&conn);
        if let Some(column) = column {
            conn.execute_batch(&format!("ALTER TABLE diaries ADD COLUMN {column};"))
                .unwrap();
        }
        conn.execute("INSERT INTO memento_migrations VALUES(2)", [])
            .unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        assert!(db::build_pool(path.to_str().unwrap()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert!(!directory.path().join("broken.db-wal").exists());
    }
}

#[test]
fn foreign_database_is_rejected_before_switching_journal_mode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("foreign.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE diaries(id INTEGER PRIMARY KEY, content TEXT, create_date TEXT);",
    )
    .unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    assert!(db::build_pool(path.to_str().unwrap()).is_err());
    assert_eq!(before, std::fs::read(&path).unwrap());
    assert!(!dir.path().join("foreign.db-wal").exists());
    assert!(!dir.path().join("foreign.db-shm").exists());
}

#[test]
fn unrelated_database_and_incomplete_legacy_collections_are_not_v0() {
    for schema in [
        "CREATE TABLE unrelated_records(id INTEGER PRIMARY KEY, data TEXT); INSERT INTO unrelated_records VALUES(1,'untouched');",
        "CREATE TABLE favorites(id INTEGER PRIMARY KEY, name TEXT);",
        "CREATE VIEW favorites AS SELECT 1 AS id;",
        "CREATE TABLE browser_totp_state(credential_hash TEXT PRIMARY KEY,last_used_step INTEGER NOT NULL);",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-memento.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(schema).unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        assert!(db::build_pool(path.to_str().unwrap()).is_err());
        assert_eq!(before, std::fs::read(&path).unwrap());
        assert!(!dir.path().join("not-memento.db-wal").exists());
    }
}

#[test]
fn complete_diary_schema_with_incomplete_collections_is_rejected() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    create_v1(&conn);
    conn.execute_batch(
        "DROP TABLE favorites; CREATE TABLE favorites(id INTEGER PRIMARY KEY,name TEXT);",
    )
    .unwrap();
    drop(conn);
    assert!(db::build_pool(file.path().to_str().unwrap()).is_err());
}

#[cfg(unix)]
#[test]
fn new_private_database_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private.db");
    let pool = db::build_pool(path.to_str().unwrap()).unwrap();
    db::init_schema(&pool).unwrap();
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
