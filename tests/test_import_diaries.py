from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/import_diaries.py"
SOURCE_SCHEMA = """
CREATE TABLE diaries(id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL,
create_date varchar(10) NOT NULL, created_at datetime, updated_at datetime);
CREATE TABLE revoked_sessions(secret TEXT);
INSERT INTO revoked_sessions VALUES ('unrelated-secret');
"""
TARGET_SCHEMA = """
CREATE TABLE diaries(id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL,
create_date TEXT NOT NULL, created_at TEXT, updated_at TEXT,
version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0), deleted_at TEXT NULL);
CREATE INDEX idx_diaries_active_date ON diaries(create_date DESC, id DESC)
WHERE deleted_at IS NULL;
CREATE TABLE browser_sessions(token_hash TEXT PRIMARY KEY, csrf_token TEXT NOT NULL,
credential_hash TEXT NOT NULL, expires_at INTEGER NOT NULL);
CREATE INDEX idx_sessions_expiry ON browser_sessions(expires_at);
CREATE TABLE browser_totp_state(credential_hash TEXT PRIMARY KEY,
last_used_step INTEGER NOT NULL);
CREATE TABLE diary_imports(source_id INTEGER PRIMARY KEY,
diary_id INTEGER REFERENCES diaries(id) ON DELETE SET NULL, fingerprint TEXT NOT NULL);
CREATE TABLE memento_migrations(version INTEGER PRIMARY KEY);
INSERT INTO memento_migrations VALUES (1), (2), (3);
CREATE TABLE favorites (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    type TEXT NOT NULL CHECK (type IN ('game','movie','book')),
    name TEXT NOT NULL,
    url TEXT,
    aka TEXT,
    genres TEXT,
    release_date TEXT,
    rating REAL,
    summary TEXT,
    extra TEXT NOT NULL DEFAULT '{}',
    image BLOB,
    image_mime TEXT,
    sort_date TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_favorites_type ON favorites(type);
CREATE INDEX idx_favorites_sort ON favorites(sort_date DESC, id DESC);
INSERT INTO favorites(id,type,name,sort_date,created_at,updated_at)
VALUES (1,'book','keep-favorite','2025-01-01','old creation','old update');
"""
ROWS = [(2, 'private-body\r\n\u65e5\u8bb0\x00', '2024-02-29', None, 'opaque timestamp'),
        (7, ' \t\n', '2024-02-29', '', None),
        (10, 'x' * 10001, '0001-01-01', 'old timestamp', None),
        (12, '', '2025-01-01', None, None)]


@contextmanager
def fixture_db(path):
    db = sqlite3.connect(path)
    try:
        with db:
            yield db
    finally:
        db.close()


class ImportDiariesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='memento-import-')
        self.addCleanup(self.temp.cleanup)
        self.source = Path(self.temp.name) / 'source ?#.db'
        self.target = Path(self.temp.name) / 'target ?#.db'
        with fixture_db(self.source) as db:
            db.executescript(SOURCE_SCHEMA)
            db.executemany('INSERT INTO diaries VALUES (?, ?, ?, ?, ?)', ROWS)
        with fixture_db(self.target) as db:
            db.executescript(TARGET_SCHEMA)

    def run_cli(self, apply=False, source=None, target=None, error=None):
        command = [sys.executable, '-B', str(SCRIPT), '--source', str(source or self.source),
                   '--target', str(target or self.target)]
        if apply:
            command.append('--apply')
        result = subprocess.run(command, capture_output=True, text=True)
        self.assertNotIn('private-body', result.stdout + result.stderr)
        self.assertNotIn('unrelated-secret', result.stdout + result.stderr)
        self.assertNotIn('Traceback', result.stderr)
        if error:
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(result.stdout, '')
            self.assertIn(error, result.stderr)
            return
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, '')
        counts = json.loads(result.stdout)
        self.assertEqual(set(counts), {'total', 'imported', 'skipped', 'planned',
                                      'duplicate_dates', 'blank_contents'})
        return counts

    def query(self, sql):
        with fixture_db(self.target) as db:
            return db.execute(sql).fetchall()

    def test_dry_run_preserves_both_files_and_counts(self):
        before = (self.source.read_bytes(), self.target.read_bytes())
        self.assertEqual(self.run_cli(), dict(total=4, imported=0, skipped=0, planned=4,
                                             duplicate_dates=1, blank_contents=2))
        self.assertEqual(before, (self.source.read_bytes(), self.target.read_bytes()))
        self.assertEqual(self.query('SELECT * FROM diary_imports'), [])

    def test_apply_preserves_all_values_and_source_bytes(self):
        before = self.source.read_bytes()
        self.assertEqual(self.run_cli(True)['imported'], 4)
        self.assertEqual(self.query('SELECT id,content,create_date,created_at,updated_at '
                                    'FROM diaries ORDER BY id'), ROWS)
        self.assertEqual(self.query('SELECT DISTINCT version FROM diaries'), [(1,)])
        self.assertEqual(self.query('SELECT DISTINCT deleted_at FROM diaries'), [(None,)])
        self.assertEqual(self.query('SELECT * FROM favorites'),
                         [(1, 'book', 'keep-favorite', None, None, None, None, None,
                           None, '{}', None, None, '2025-01-01', 'old creation', 'old update')])
        self.assertEqual(before, self.source.read_bytes())

    def test_idempotent_even_after_edit_and_hard_delete_then_append(self):
        self.run_cli(True)
        with fixture_db(self.target) as db:
            db.execute('PRAGMA foreign_keys=ON')
            db.execute("UPDATE diaries SET content='edited',version=2 WHERE id=2")
            db.execute('DELETE FROM diaries WHERE id=7')
        self.assertEqual(self.query('SELECT diary_id FROM diary_imports WHERE source_id=7'),
                         [(None,)])
        self.assertEqual(self.run_cli()['skipped'], 4)
        counts = self.run_cli(True)
        self.assertEqual((counts['imported'], counts['skipped']), (0, 4))
        self.assertEqual(self.query('SELECT content,version FROM diaries WHERE id=2'),
                         [('edited', 2)])
        self.assertEqual(self.query('SELECT * FROM diaries WHERE id=7'), [])
        with fixture_db(self.source) as db:
            db.execute("INSERT INTO diaries VALUES (20,'new','2025-03-01',NULL,NULL)")
        self.assertEqual(self.run_cli(True)['imported'], 1)

    def test_changed_source_refuses_entire_batch(self):
        self.run_cli(True)
        with fixture_db(self.source) as db:
            db.execute("INSERT INTO diaries VALUES (1,'new','2025-03-01',NULL,NULL)")
            db.execute("UPDATE diaries SET updated_at='changed' WHERE id=12")
        before = self.target.read_bytes()
        self.run_cli(True, error='source row changed')
        self.assertEqual(before, self.target.read_bytes())

    def test_soft_delete_is_not_resurrected_and_append_stays_active(self):
        self.run_cli(True)
        ledger = self.query('SELECT * FROM diary_imports ORDER BY source_id')
        with fixture_db(self.target) as db:
            db.execute("UPDATE diaries SET deleted_at='2026-09-08T00:00:00Z', "
                       "updated_at='2026-09-08T00:00:00Z', version=version+1 WHERE id=7")
        deleted = self.query('SELECT * FROM diaries WHERE id=7')
        self.assertEqual(deleted, [(7, ROWS[1][1], ROWS[1][2], '',
                                   '2026-09-08T00:00:00Z', 2, '2026-09-08T00:00:00Z')])
        before = self.target.read_bytes()
        for apply in (False, True):
            counts = self.run_cli(apply)
            self.assertEqual((counts['imported'], counts['skipped'], counts['planned']), (0, 4, 0))
            self.assertEqual(before, self.target.read_bytes())
        with fixture_db(self.source) as db:
            db.execute("INSERT INTO diaries VALUES (20,'new','2025-03-01',NULL,NULL)")
        self.assertEqual(self.run_cli()['planned'], 1)
        counts = self.run_cli(True)
        self.assertEqual((counts['imported'], counts['skipped']), (1, 4))
        self.assertEqual(self.query('SELECT * FROM diaries WHERE id=7'), deleted)
        self.assertEqual(self.query('SELECT * FROM diary_imports WHERE source_id!=20 '
                                    'ORDER BY source_id'), ledger)
        self.assertEqual(self.query('SELECT version,deleted_at FROM diaries WHERE id=20'),
                         [(1, None)])

    def test_all_soft_deleted_initial_target_is_not_empty(self):
        with fixture_db(self.target) as db:
            db.execute("INSERT INTO diaries(id,content,create_date,updated_at,version,deleted_at) "
                       "VALUES (90,'local','2025-01-01','2026-09-08T00:00:00Z',2,"
                       "'2026-09-08T00:00:00Z')")
        before = self.target.read_bytes()
        for apply in (False, True):
            self.run_cli(apply, error='target diaries must be empty')
            self.assertEqual(before, self.target.read_bytes())
        self.assertEqual(self.query('SELECT * FROM diary_imports'), [])

    def test_changed_soft_deleted_source_refuses_entire_batch(self):
        self.run_cli(True)
        with fixture_db(self.target) as db:
            db.execute("UPDATE diaries SET deleted_at='2026-09-08T00:00:00Z', "
                       "updated_at='2026-09-08T00:00:00Z', version=version+1 WHERE id=12")
        with fixture_db(self.source) as db:
            db.execute("INSERT INTO diaries VALUES (1,'new','2025-03-01',NULL,NULL)")
            db.execute("INSERT INTO diaries VALUES (20,'new','2025-03-01',NULL,NULL)")
            db.execute("UPDATE diaries SET content='changed' WHERE id=12")
        before = self.target.read_bytes()
        for apply in (False, True):
            self.run_cli(apply, error='source row changed')
            self.assertEqual(before, self.target.read_bytes())
        self.assertEqual(self.query('SELECT id FROM diaries ORDER BY id'), [(2,), (7,), (10,), (12,)])
        self.assertEqual(self.query('SELECT source_id FROM diary_imports ORDER BY source_id'),
                         [(2,), (7,), (10,), (12,)])

    def test_deleted_local_id_cannot_be_reused_by_append(self):
        self.run_cli(True)
        with fixture_db(self.target) as db:
            cursor = db.execute("INSERT INTO diaries(content,create_date) VALUES ('local','2025-01-01')")
            self.assertEqual(cursor.lastrowid, 13)
            db.execute('DELETE FROM diaries WHERE id=13')
        with fixture_db(self.source) as db:
            db.executemany('INSERT INTO diaries VALUES (?,?,?,?,?)',
                           [(13, 'reuse', '2025-01-01', None, None),
                            (14, 'new', '2025-01-01', None, None)])
        before = self.target.read_bytes()
        for apply in (False, True):
            self.run_cli(apply, error='target id conflict')
            self.assertEqual(before, self.target.read_bytes())
        self.assertEqual(self.query('SELECT id FROM diaries ORDER BY id'), [(2,), (7,), (10,), (12,)])
        self.assertEqual(self.query("SELECT seq FROM sqlite_sequence WHERE name='diaries'"), [(13,)])

    def test_first_import_respects_deleted_target_history(self):
        with fixture_db(self.target) as db:
            db.execute("INSERT INTO diaries(id,content,create_date) VALUES (12,'local','2025-01-01')")
            db.execute('DELETE FROM diaries')
        before = self.target.read_bytes()
        for apply in (False, True):
            self.run_cli(apply, error='target id conflict')
            self.assertEqual(before, self.target.read_bytes())
        self.assertEqual(self.query('SELECT * FROM diary_imports'), [])

    def test_empty_source_with_deleted_target_history_is_noop(self):
        with fixture_db(self.target) as db:
            db.execute("INSERT INTO diaries(id,content,create_date) VALUES (12,'local','2025-01-01')")
            db.execute('DELETE FROM diaries')
        with fixture_db(self.source) as db:
            db.execute('DELETE FROM diaries')
        before = self.target.read_bytes()
        for apply in (False, True):
            self.assertEqual(self.run_cli(apply), dict(total=0, imported=0, skipped=0, planned=0,
                                                      duplicate_dates=0, blank_contents=0))
            self.assertEqual(before, self.target.read_bytes())

    def test_ledger_skips_below_watermark_and_new_ids_above_it_import(self):
        self.run_cli(True)
        with fixture_db(self.target) as db:
            db.execute('PRAGMA foreign_keys=ON')
            db.execute("INSERT INTO diaries(id,content,create_date) VALUES (30,'local','2025-01-01')")
            db.execute('DELETE FROM diaries')
        with fixture_db(self.source) as db:
            db.executemany('INSERT INTO diaries VALUES (?,?,?,?,?)',
                           [(31, 'new', '2025-01-01', None, None),
                            (32, 'new', '2025-01-01', None, None)])
        counts = self.run_cli(True)
        self.assertEqual((counts['imported'], counts['skipped']), (2, 4))
        self.assertEqual(self.query('SELECT id FROM diaries ORDER BY id'), [(31,), (32,)])

    def test_source_wal_uncheckpointed_commit_is_imported_read_only(self):
        db = sqlite3.connect(self.source)
        try:
            self.assertEqual(db.execute('PRAGMA journal_mode=WAL').fetchone(), ('wal',))
            db.execute('PRAGMA wal_autocheckpoint=0')
            db.execute('PRAGMA wal_checkpoint(TRUNCATE)')
            main_before = self.source.read_bytes()
            row = (13, 'wal-only', '2025-01-01', None, None)
            db.execute('INSERT INTO diaries VALUES (?,?,?,?,?)', row)
            db.commit()
            wal = Path(str(self.source) + '-wal')
            wal_before = wal.read_bytes()
            self.assertGreater(len(wal_before), 32)
            self.assertEqual(main_before, self.source.read_bytes())
            self.assertEqual(self.run_cli()['planned'], 5)
            self.assertEqual(self.run_cli(True)['imported'], 5)
            self.assertEqual(self.query('SELECT id,content,create_date,created_at,updated_at '
                                        'FROM diaries WHERE id=13'), [row])
            self.assertEqual(main_before, self.source.read_bytes())
            self.assertEqual(wal_before, wal.read_bytes())
        finally:
            db.close()

    def test_new_id_conflict_refuses_entire_batch(self):
        self.run_cli(True)
        with fixture_db(self.target) as db:
            db.execute("INSERT INTO diaries(id,content,create_date) VALUES (30,'local','2025-01-01')")
        with fixture_db(self.source) as db:
            db.executemany('INSERT INTO diaries VALUES (?,?,?,?,?)',
                           [(20, 'new', '2025-01-01', None, None),
                            (30, 'conflict', '2025-01-01', None, None)])
        before = self.target.read_bytes()
        self.run_cli(True, error='target id conflict')
        self.assertEqual(before, self.target.read_bytes())

    def test_write_failure_rolls_back_and_redacts_sqlite_error(self):
        with fixture_db(self.target) as db:
            db.executescript("CREATE TRIGGER fail_insert BEFORE INSERT ON diary_imports "
                             "WHEN NEW.source_id=7 BEGIN SELECT RAISE(ABORT,'private-body'); END;")
        self.run_cli(True, error='database operation failed')
        self.assertEqual(self.query('SELECT * FROM diaries'), [])
        self.assertEqual(self.query('SELECT * FROM diary_imports'), [])

    def test_nonempty_initial_target_rejected(self):
        with fixture_db(self.target) as db:
            db.execute("INSERT INTO diaries(id,content,create_date) VALUES (90,'local','2025-01-01')")
        self.run_cli(True, error='target diaries must be empty')

    def test_same_path_symlink_and_hardlink_rejected(self):
        self.run_cli(target=self.source, error='source and target must differ')
        for kind in ('symlink', 'hardlink'):
            alias = Path(self.temp.name) / kind
            if kind == 'symlink':
                alias.symlink_to(self.source)
            else:
                os.link(self.source, alias)
            self.run_cli(True, target=alias, error='source and target must differ')

    def test_missing_paths_never_created(self):
        missing = Path(self.temp.name) / 'missing.db'
        self.run_cli(source=missing, error='source file missing')
        self.run_cli(True, target=missing, error='memento init-db')
        self.assertFalse(missing.exists())

    def test_target_requires_schema_and_exact_migration_version(self):
        for index, sql in enumerate(('DROP TABLE diary_imports', 'DELETE FROM memento_migrations',
                    'INSERT INTO memento_migrations VALUES (4)',
                    'DELETE FROM memento_migrations WHERE version=1',
                    'DELETE FROM memento_migrations WHERE version=2',
                    'DELETE FROM memento_migrations WHERE version=3',
                    'DROP TABLE browser_totp_state',
                    'DROP INDEX idx_diaries_active_date; ALTER TABLE diaries DROP COLUMN deleted_at',
                    'ALTER TABLE diaries DROP COLUMN version')):
            with self.subTest(sql=sql):
                other = Path(self.temp.name) / f'bad-{index}.db'
                with fixture_db(other) as db:
                    db.executescript(TARGET_SCHEMA)
                    db.executescript(sql)
                before = other.read_bytes()
                for apply in (False, True):
                    self.run_cli(apply, target=other, error='memento init-db')
                    self.assertEqual(before, other.read_bytes())

    def test_v1_target_requires_init_db_without_auto_upgrade(self):
        with fixture_db(self.target) as db:
            db.execute('DROP INDEX idx_diaries_active_date')
            db.execute('ALTER TABLE diaries DROP COLUMN deleted_at')
            db.execute('CREATE INDEX idx_diaries_date ON diaries(create_date DESC, id DESC)')
            db.execute('DELETE FROM memento_migrations WHERE version>=2')
            db.execute('DROP TABLE browser_totp_state')
        before = self.target.read_bytes()
        for apply in (False, True):
            self.run_cli(apply, error='memento init-db')
            self.assertEqual(before, self.target.read_bytes())
        self.assertEqual(self.query('SELECT version FROM memento_migrations'), [(1,)])

    def test_v2_target_requires_init_db_without_auto_upgrade(self):
        with fixture_db(self.target) as db:
            db.execute('DELETE FROM memento_migrations WHERE version=3')
            db.execute('DROP TABLE browser_totp_state')
        before = self.target.read_bytes()
        for apply in (False, True):
            self.run_cli(apply, error='memento init-db')
            self.assertEqual(before, self.target.read_bytes())
        self.assertEqual(self.query('SELECT version FROM memento_migrations'), [(1,), (2,)])

    def test_target_invalid_totp_state_rejected(self):
        for index, definition in enumerate((
                'credential_hash TEXT, last_used_step INTEGER NOT NULL',
                'credential_hash INTEGER PRIMARY KEY, last_used_step INTEGER NOT NULL',
                'credential_hash TEXT PRIMARY KEY NOT NULL, last_used_step INTEGER NOT NULL',
                'credential_hash TEXT PRIMARY KEY, last_used_step INTEGER',
                'credential_hash TEXT PRIMARY KEY, last_used_step TEXT NOT NULL',
                'credential_hash TEXT PRIMARY KEY, wrong_step INTEGER NOT NULL',
                'credential_hash TEXT PRIMARY KEY, last_used_step INTEGER NOT NULL, extra TEXT')):
            with self.subTest(definition=definition):
                other = Path(self.temp.name) / f'totp-{index}.db'
                with fixture_db(other) as db:
                    db.executescript(TARGET_SCHEMA)
                    db.execute('DROP TABLE browser_totp_state')
                    db.execute(f'CREATE TABLE browser_totp_state({definition})')
                before = other.read_bytes()
                for apply in (False, True):
                    self.run_cli(apply, target=other, error='memento init-db')
                    self.assertEqual(before, other.read_bytes())

    def test_import_preserves_auth_state_without_reading_rows(self):
        with fixture_db(self.target) as db:
            db.execute("INSERT INTO browser_totp_state VALUES ('synthetic digest',12345)")
            db.execute("INSERT INTO browser_sessions VALUES ('synthetic token','synthetic csrf',"
                       "'synthetic digest',99999)")
        before = {table: self.query(f'SELECT * FROM {table}')
                  for table in ('browser_totp_state', 'browser_sessions')}
        spec = importlib.util.spec_from_file_location('import_diaries', SCRIPT)
        importer = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(importer)
        connect = sqlite3.connect

        def guarded_connect(*args, **kwargs):
            db = connect(*args, **kwargs)
            db.set_authorizer(lambda action, table, column, database, origin:
                              sqlite3.SQLITE_DENY if action == sqlite3.SQLITE_READ
                              and table in before else sqlite3.SQLITE_OK)
            return db

        with patch.object(importer.sqlite3, 'connect', guarded_connect):
            self.assertEqual(importer.import_diaries(self.source, self.target)['planned'], 4)
            self.assertEqual(importer.import_diaries(self.source, self.target, True)['imported'], 4)
            self.assertEqual(importer.import_diaries(self.source, self.target, True)['skipped'], 4)
        for table, rows in before.items():
            self.assertEqual(self.query(f'SELECT * FROM {table}'), rows)

    def test_target_deleted_at_requires_nullable_text_without_default(self):
        for index, definition in enumerate(('deleted_at INTEGER NULL',
                                            'deleted_at TEXT NOT NULL',
                                            'deleted_at TEXT DEFAULT NULL',
                                            "deleted_at TEXT DEFAULT 'deleted'")):
            with self.subTest(definition=definition):
                other = Path(self.temp.name) / f'deleted-at-{index}.db'
                with fixture_db(other) as db:
                    db.executescript(TARGET_SCHEMA.replace('deleted_at TEXT NULL', definition))
                before = other.read_bytes()
                for apply in (False, True):
                    self.run_cli(apply, target=other, error='memento init-db')
                    self.assertEqual(before, other.read_bytes())

    def test_target_missing_browser_sessions_rejected(self):
        with fixture_db(self.target) as db:
            db.execute('DROP TABLE browser_sessions')
        before = self.target.read_bytes()
        for apply in (False, True):
            with self.subTest(apply=apply):
                self.run_cli(apply, error='memento init-db')
                self.assertEqual(before, self.target.read_bytes())

    def test_target_incomplete_favorites_rejected(self):
        changes = (
            'DROP TABLE favorites',
            'DROP TABLE favorites; CREATE TABLE favorites(id INTEGER PRIMARY KEY, name TEXT)',
            'ALTER TABLE favorites DROP COLUMN image',
        )
        definitions = (
            TARGET_SCHEMA.replace('rating REAL', 'rating TEXT'),
            TARGET_SCHEMA.replace('name TEXT NOT NULL', 'name TEXT'),
            TARGET_SCHEMA.replace('id INTEGER PRIMARY KEY AUTOINCREMENT,\n    type',
                                  'id INTEGER,\n    type'),
        )
        for index, (schema, sql) in enumerate(
                [(TARGET_SCHEMA, sql) for sql in changes] + [(schema, '') for schema in definitions]):
            for apply in (False, True):
                with self.subTest(index=index, apply=apply):
                    other = Path(self.temp.name) / f'favorites-{index}-{apply}.db'
                    with fixture_db(other) as db:
                        db.executescript(schema)
                        db.executescript(sql)
                    before = other.read_bytes()
                    self.run_cli(apply, target=other, error='memento init-db')
                    self.assertEqual(before, other.read_bytes())

    def test_target_invalid_browser_sessions_columns_rejected(self):
        changes = (
            ('csrf_token TEXT NOT NULL', 'csrf_token TEXT'),
            ('expires_at INTEGER NOT NULL', 'expires_at TEXT NOT NULL'),
            ('token_hash TEXT PRIMARY KEY', 'token_hash TEXT'),
            ('credential_hash TEXT NOT NULL', 'wrong_column TEXT NOT NULL'),
        )
        for index, (old, new) in enumerate(changes):
            for apply in (False, True):
                with self.subTest(index=index, apply=apply):
                    other = Path(self.temp.name) / f'sessions-{index}-{apply}.db'
                    with fixture_db(other) as db:
                        db.executescript(TARGET_SCHEMA.replace(old, new))
                    before = other.read_bytes()
                    self.run_cli(apply, target=other, error='memento init-db')
                    self.assertEqual(before, other.read_bytes())

    def test_source_requires_table_and_all_essential_columns(self):
        with fixture_db(self.source) as db:
            db.execute('ALTER TABLE diaries RENAME TO original')
            db.execute('CREATE VIEW diaries AS SELECT * FROM original')
        self.run_cli(error='invalid source schema')
        with fixture_db(self.source) as db:
            db.execute('DROP VIEW diaries')
            db.execute('CREATE TABLE diaries(id INTEGER, content TEXT, create_date TEXT)')
        self.run_cli(error='invalid source schema')

    def test_extra_source_column_ignored(self):
        with fixture_db(self.source) as db:
            db.execute('ALTER TABLE diaries ADD COLUMN secret TEXT')
            db.execute("UPDATE diaries SET secret='unrelated-secret'")
        self.assertEqual(self.run_cli(True)['imported'], 4)

    def test_empty_source_is_noop(self):
        with fixture_db(self.source) as db:
            db.execute('DELETE FROM diaries')
        before = self.target.read_bytes()
        self.assertEqual(self.run_cli(True), dict(total=0, imported=0, skipped=0, planned=0,
                                                 duplicate_dates=0, blank_contents=0))
        self.assertEqual(before, self.target.read_bytes())

    def test_duplicate_and_noninteger_source_ids_rejected(self):
        with fixture_db(self.source) as db:
            db.execute('DROP TABLE diaries')
            db.execute('CREATE TABLE diaries(id,content,create_date,created_at,updated_at)')
        for ids in [(1, 1), (1, '2'), (1, 2.5), (1, None), (1, 0)]:
            with self.subTest(ids=ids):
                with fixture_db(self.source) as db:
                    db.execute('DELETE FROM diaries')
                    db.executemany('INSERT INTO diaries VALUES (?,?,?,?,?)',
                                   [(i, 'text', '2025-01-01', None, None) for i in ids])
                self.run_cli(True, error='invalid source row')
                self.assertEqual(self.query('SELECT * FROM diaries'), [])

    def test_target_missing_constraints_rejected(self):
        for index, removed in enumerate(('CHECK(version > 0)', 'ON DELETE SET NULL',
                                         'AUTOINCREMENT', 'DEFAULT 1')):
            with self.subTest(removed=removed):
                other = Path(self.temp.name) / f'constraints-{index}.db'
                with fixture_db(other) as db:
                    db.executescript(TARGET_SCHEMA.replace(removed, ''))
                self.run_cli(True, target=other, error='memento init-db')

    def test_invalid_values_rejected_before_any_write(self):
        for column, value in [('create_date', '2025-02-29'), ('create_date', '2024-2-29'),
                              ('create_date', '20240229'), ('create_date', '0000-01-01'),
                              ('content', b'private-body'), ('created_at', b'bad'),
                              ('updated_at', 3), ('id', -1)]:
            with self.subTest(column=column, value=value):
                with fixture_db(self.source) as db:
                    db.execute('DELETE FROM diaries')
                    db.executemany('INSERT INTO diaries VALUES (?,?,?,?,?)', ROWS)
                    db.execute(f'UPDATE diaries SET {column}=? WHERE id=12', (value,))
                self.run_cli(True, error='invalid source row')
                self.assertEqual(self.query('SELECT * FROM diaries'), [])


if __name__ == '__main__':
    unittest.main()
