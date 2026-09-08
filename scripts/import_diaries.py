#!/usr/bin/env python3
"""Import legacy diaries without normalization; default to a read-only dry run.

Initialize/upgrade the target to schema v3 with `memento init-db` first.
This importer never upgrades schemas. Only --apply writes.
duplicate_dates counts rows beyond the first per date; planned counts new rows
in this batch, and imported counts rows actually committed. Keep diary_imports
when editing/deleting diaries: its fingerprints prevent overwrites/resurrection.
Untracked source IDs must exceed the target's pre-import AUTOINCREMENT watermark.
Fingerprints use SHA-256 of compact ASCII JSON arrays of the five source values.
"""

import argparse
from collections import Counter
from contextlib import closing
from datetime import date
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import sys


COLUMNS = ('id', 'content', 'create_date', 'created_at', 'updated_at')
TARGET_ERROR = 'invalid target schema/version; run memento init-db first'


class ImportFailure(Exception):
    pass


def table_columns(db, name, error):
    # Names come only from constants, never from command-line input.
    definition = db.execute(
        'SELECT type, sql FROM sqlite_master WHERE name=?', (name,)
    ).fetchone()
    if (not definition or definition[0] != 'table' or not definition[1]
            or not re.match(r'CREATE\s+TABLE\s', definition[1], re.I)):
        raise ImportFailure(error)
    return {row[1]: row for row in db.execute(f'PRAGMA table_info("{name}")')}


def validate_target(db):
    expected = {
        'diaries': {'id': ('INTEGER', 0, 1), 'content': ('TEXT', 1, 0),
                    'create_date': ('TEXT', 1, 0), 'created_at': ('TEXT', 0, 0),
                    'updated_at': ('TEXT', 0, 0), 'version': ('INTEGER', 1, 0),
                    'deleted_at': ('TEXT', 0, 0)},
        'diary_imports': {'source_id': ('INTEGER', 0, 1),
                          'diary_id': ('INTEGER', 0, 0), 'fingerprint': ('TEXT', 1, 0)},
        'memento_migrations': {'version': ('INTEGER', 0, 1)},
        'browser_totp_state': {'credential_hash': ('TEXT', 0, 1),
                               'last_used_step': ('INTEGER', 1, 0)},
        'browser_sessions': {'token_hash': ('TEXT', 0, 1),
                             'csrf_token': ('TEXT', 1, 0),
                             'credential_hash': ('TEXT', 1, 0),
                             'expires_at': ('INTEGER', 1, 0)},
        'favorites': {'id': ('INTEGER', 0, 1), 'type': ('TEXT', 1, 0),
                      'name': ('TEXT', 1, 0), 'url': ('TEXT', 0, 0),
                      'aka': ('TEXT', 0, 0), 'genres': ('TEXT', 0, 0),
                      'release_date': ('TEXT', 0, 0), 'rating': ('REAL', 0, 0),
                      'summary': ('TEXT', 0, 0), 'extra': ('TEXT', 1, 0),
                      'image': ('BLOB', 0, 0), 'image_mime': ('TEXT', 0, 0),
                      'sort_date': ('TEXT', 1, 0), 'created_at': ('TEXT', 1, 0),
                      'updated_at': ('TEXT', 1, 0)},
    }
    for name, fields in expected.items():
        columns = table_columns(db, name, TARGET_ERROR)
        if set(columns) != set(fields):
            raise ImportFailure(TARGET_ERROR)
        for field, (kind, required, primary) in fields.items():
            info = columns[field]
            if (info[2].upper(), info[3], info[5]) != (kind, required, primary):
                raise ImportFailure(TARGET_ERROR)
        if name == 'diaries' and (columns['version'][4] != '1'
                                  or columns['deleted_at'][4] is not None):
            raise ImportFailure(TARGET_ERROR)
    if db.execute('SELECT version FROM memento_migrations ORDER BY version').fetchall() != [(1,), (2,), (3,)]:
        raise ImportFailure(TARGET_ERROR)
    foreign_keys = db.execute('PRAGMA foreign_key_list(diary_imports)').fetchall()
    if not any(row[2:5] == ('diaries', 'diary_id', 'id') and row[6] == 'SET NULL'
               for row in foreign_keys):
        raise ImportFailure(TARGET_ERROR)
    sql = db.execute("SELECT sql FROM sqlite_master WHERE name='diaries'").fetchone()[0]
    if (not re.search(r'\bAUTOINCREMENT\b', sql, re.I)
            or not re.search(r'CHECK\s*\(\s*version\s*>\s*0\s*\)', sql, re.I)):
        raise ImportFailure(TARGET_ERROR)


def import_diaries(source, target, apply=False):
    source, target = Path(source).resolve(), Path(target).resolve()
    if not source.is_file():
        raise ImportFailure('source file missing')
    if not target.is_file():
        raise ImportFailure('target file missing; run memento init-db first')
    if source.samefile(target):
        raise ImportFailure('source and target must differ')

    with closing(sqlite3.connect(source.as_uri() + '?mode=ro', uri=True)) as src:
        src.execute('PRAGMA query_only=ON')
        src.execute('PRAGMA trusted_schema=OFF')
        src.execute('PRAGMA busy_timeout=5000')
        src.execute('BEGIN')
        columns = table_columns(src, 'diaries', 'invalid source schema')
        if not set(COLUMNS).issubset(columns):
            raise ImportFailure('invalid source schema')
        # Restrict even indirect reads to the required diary fields only.
        src.set_authorizer(lambda action, table, column, database, origin:
                           sqlite3.SQLITE_DENY if action == sqlite3.SQLITE_READ
                           and (table != 'diaries' or column not in COLUMNS)
                           else sqlite3.SQLITE_OK)
        rows = src.execute('SELECT id,content,create_date,created_at,updated_at '
                           'FROM diaries ORDER BY id').fetchall()
        seen = set()
        fingerprints = {}
        for row in rows:
            row_id, content, create_date, created_at, updated_at = row
            if (type(row_id) is not int or row_id <= 0 or row_id in seen
                    or not isinstance(content, str)
                    or not isinstance(create_date, str)
                    or not re.fullmatch(r'[0-9]{4}-[0-9]{2}-[0-9]{2}', create_date)
                    or any(value is not None and not isinstance(value, str)
                           for value in (created_at, updated_at))):
                raise ImportFailure('invalid source row')
            try:
                date.fromisoformat(create_date)
            except ValueError:
                raise ImportFailure('invalid source row') from None
            seen.add(row_id)
            fingerprints[row_id] = hashlib.sha256(
                json.dumps(row, ensure_ascii=True, separators=(',', ':')).encode('ascii')
            ).hexdigest()

        mode = 'rw' if apply else 'ro'
        with closing(sqlite3.connect(target.as_uri() + '?mode=' + mode, uri=True)) as dst:
            dst.execute('PRAGMA foreign_keys=ON')
            dst.execute('PRAGMA busy_timeout=5000')
            if not apply:
                dst.execute('PRAGMA query_only=ON')
            with dst:
                dst.execute('BEGIN IMMEDIATE' if apply else 'BEGIN')
                validate_target(dst)
                high_watermark = dst.execute(
                    "SELECT COALESCE(MAX(seq), 0) FROM sqlite_sequence WHERE name='diaries'"
                ).fetchone()[0]
                ledger = dict(dst.execute('SELECT source_id,fingerprint FROM diary_imports'))
                target_ids = {row[0] for row in dst.execute('SELECT id FROM diaries')}
                if not ledger and target_ids:
                    raise ImportFailure('target diaries must be empty for first import')
                pending = []
                for row in rows:
                    row_id = row[0]
                    if row_id in ledger:
                        if ledger[row_id] != fingerprints[row_id]:
                            raise ImportFailure('source row changed')
                    elif row_id in target_ids:
                        raise ImportFailure('target id conflict')
                    else:
                        pending.append(row)
                # Deleted local IDs must not be reused with a fresh CAS version.
                if any(row[0] <= high_watermark for row in pending):
                    raise ImportFailure('target id conflict')
                if apply:
                    dst.executemany('INSERT INTO diaries '
                                    '(id,content,create_date,created_at,updated_at,version,deleted_at) '
                                    'VALUES (?,?,?,?,?,1,NULL)', pending)
                    dst.executemany('INSERT INTO diary_imports '
                                    '(source_id,diary_id,fingerprint) VALUES (?,?,?)',
                                    [(row[0], row[0], fingerprints[row[0]]) for row in pending])
            return {'total': len(rows), 'imported': len(pending) if apply else 0,
                    'skipped': len(rows) - len(pending), 'planned': len(pending),
                    'duplicate_dates': sum(n - 1 for n in Counter(row[2] for row in rows).values()),
                    'blank_contents': sum(not row[1].strip() for row in rows)}


class SafeParser(argparse.ArgumentParser):
    def error(self, message):
        self.exit(2, 'error: invalid arguments; use --help\n')


def main():
    parser = SafeParser(description=__doc__)
    parser.add_argument('--source', required=True, help='legacy SQLite file (read only)')
    parser.add_argument('--target', required=True, help='initialized memento SQLite file')
    parser.add_argument('--apply', action='store_true', help='commit imports; default is dry run')
    args = parser.parse_args()
    try:
        counts = import_diaries(args.source, args.target, args.apply)
    except ImportFailure as error:
        print('error: ' + str(error), file=sys.stderr)
        return 1
    except (sqlite3.Error, OSError, ValueError):
        print('error: database operation failed', file=sys.stderr)
        return 1
    print(json.dumps(counts, sort_keys=True))
    return 0


if __name__ == '__main__':
    sys.exit(main())
