# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`memento` is a personal **poster wall (海报墙)** with browser login and private diaries. Collection reads are public and collection writes use `X-API-Key`; diary API reads and writes both require that key. Browser diary access uses a separate cookie session. Release frontend assets (`static/`) are embedded via `rust-embed`; data remains in an external SQLite file, and default debug builds read assets from disk.

## Commands

All commands run from the current repository/worktree root, not the obsolete `/root/codes/memento` path.

```bash
cargo build                       # debug build
cargo build --release             # single-file binary → target/release/memento
MEMENTO_ENV=development cargo run  # dev run after configuring password and TOTP
cargo test                        # run tests
cargo test <name>                 # run a single test by substring
cargo clippy --all-targets        # lint
cargo fmt                         # format
```

No-argument server startup selects `MEMENTO_ENV=development|production` from the system environment first, defaulting to production. Load only cwd `.env.development` or `.env.production`; never load generic `.env`, search parents, override existing system values or let the file switch mode. `hash-password`, `totp-secret` and `init-db` dispatch before dotenv/server startup and load no env file. Development example: bind `0.0.0.0:23457`, DB `./memento.dev.db`; production example: bind `127.0.0.1:23457`, DB `./memento.db`, launch `MEMENTO_ENV=production ./memento`. Site display variables `MEMENTO_SITE_NAME` / `MEMENTO_SLOGAN` / `MEMENTO_ICON` retain escaped placeholder injection. Unset/blank API Key disables all key routes without logging a secret. Never commit actual environment files or credentials; example hash/secret values stay empty.

`Config` retains its legacy random-key generation for compatibility, but `run()` passes an empty key to `AppState` when `api_key_generated` is true: the generated value is neither used for authorization nor printed. Public collection reads and separately configured browser login remain available without an external key. Do not restore the old secret-logging behavior.

```bash
# Seed importer — backfills from the OLD 海报墙 site's API (urllib, stdlib only)
MEMENTO_API_KEY=... python3 scripts/seed_import.py --old http://147.79.20.135:23456 --new http://localhost:23457 --skip-existing
# Minimal write-API client (drop-in for a bot handler)
MEMENTO_API_KEY=... python3 scripts/submit_example.py --base http://localhost:23457
```

## Architecture

Axum 0.8 + Tokio. SQLite via `rusqlite` (bundled) behind an `r2d2` pool. The request flow is layered; respect these boundaries when editing:

```
main.rs (CLI dispatch or server bootstrap) -> lib.rs (router, layers)
  └─ handlers/ (HTTP: extract, validate query, build ApiResponse)
       └─ models.rs (validate + normalise request body → Prepared{Create,Update})
       └─ repo.rs   (pure SQL against a pooled Connection)
            └─ db.rs (pool + transactional schema upgrades)
```

- **`lib.rs`** (`build_router`) keeps public collection reads and guarded collection writes (`GET /api/auth/verify` also guarded; collection body limit 20 MiB). Key verification returns `{valid:true}` in `data`, or upstream 401. Separate diary/session groups are described below. Security headers (`nosniff`, `X-Frame-Options: DENY`, `no-referrer`) are global; permissive CORS stays on the legacy collection/public group, not diaries or sessions. The crate has both a `[lib]` and `[[bin]]` target so integration tests can drive `build_router`.
- **Blocking DB work** (`rusqlite` is sync) is always run inside `tokio::task::spawn_blocking`. Handlers `.clone()` `state.pool` into the closure. Keep this pattern — never call `repo::*` directly on the async runtime.
- **`repo.rs`** functions take a `&Connection` and are pure data access (no HTTP types). Dynamic `WHERE`/`SET` clauses use parameterized binds (`Box<dyn ToSql>`) — never string-interpolate user values into SQL. LIKE search escapes `%`/`_` via `escape_like`.
- **`error.rs`** defines `AppError`, which maps failures to an HTTP status and a **sanitized public message**; internal detail stays server-side. Application JSON uses `ApiResponse { success, data, error }`; do not assume empty 204, image/static responses or every extractor rejection use this envelope. Diary/login extractors explicitly redact private input.

## Private diaries and browser login

Authoritative details: [design](docs/diary-design.md), [API](docs/diary-api.md), [operations and migration](docs/diary-operations.md). Operations records reported local validation separately from unverified deployment, real-data migration and cross-platform behavior. Config defaults, not just example settings, are development bind 0.0.0.0:23457 / DB ./memento.dev.db and production bind 127.0.0.1:23457 / DB ./memento.db.

- `/api/diaries` and `/api/diaries/{id}` require `X-API-Key` for **all reads and writes**. The single key is shared with collection writes; there are no per-agent scopes or read-only diary keys.
- `/private/diaries` uses browser cookies only and shares diary handlers/business with the key API. Both modes require session-bound `X-CSRF-Token` for authenticated unsafe requests; the frontend handles it without user configuration. Production additionally requires exact Origin. Never let a key bypass cookie/CSRF checks or a cookie authorize the key API.
- Both modes require bcrypt `MEMENTO_PASSWORD_HASH` and Base32 `MEMENTO_TOTP_SECRET` decoding to at least 20 bytes; username defaults to admin. Old Argon2 hashes are unsupported: regenerate the hash without changing diary/collection data. Do not migrate legacy authentication data. First POST `/session` validates username/password and returns data{requires_totp:true,challenge} without Cookie. Second POST to the same URL sends {challenge,code}, returning data{username,csrf_token} with Cookie only after successful TOTP. Challenges last 5 minutes, allow at most 5 wrong OTP attempts and at most 64 concurrent pending challenges; this is not a long-lived session limit.
- Development does not compare Origin/Host and uses HttpOnly, SameSite=Lax, no Secure. Production requires explicit canonical HTTP(S) PUBLIC_ORIGIN and exact unsafe Origin, uses HttpOnly/SameSite=Strict and adds Secure only for https origin. No automatic-Origin mode. LAN production HTTP behind Nginx is supported without domains/certificates; public HTTPS is recommended, not universally mandatory. Collection CORS must not spread to cookie/diary routes.
- Generate credentials with `cargo run -- hash-password` and `cargo run -- totp-secret`, configure the selected env file and local authenticator, then explicitly run `MEMENTO_ENV=development cargo run`. Production examples in deploy/nginx are alternative complete http-context include snippets: HTTP 8080 or optional HTTPS. Preserve Host ports and original Origin, proxy all routes without rewrite, disable caching/buffering and upstream retries, retain collection 20m/session 8k/diary 64k body limits. Never automatically deploy or operate system Nginx.
- Preserve no-store and persistent token-hash sessions. `MEMENTO_SESSION_TTL_DAYS` defaults to 7 positive days, without sliding renewal or a 32-session cap. Username/hash/TOTP changes invalidate old sessions; origin changes do not. Schema v3 adds browser_totp_state(credential_hash TEXT PRIMARY KEY,last_used_step INTEGER NOT NULL): accept only a greater OTP step, atomically with session insertion in one IMMEDIATE transaction; failures roll back both. No reuse across challenges, restart or logout. Keep +/-1 step tolerance, but a replay 401 means wait for the next code and restart login. An accepted future step requires waiting for an even later step; clock rollback may temporarily reject login. No manual global bypass. This is minimal replay protection, not an audit/account platform. Application IP buckets, global hash semaphore and ConnectInfo server dependency are removed; Nginx limits /session with 429. No recovery codes or self-service account platform; operators replace lost secrets. Never log credentials or private content.
- An empty undeleted set (`deleted_at IS NULL`) starts at business-timezone today (`MEMENTO_DIARY_TIMEZONE`, default `Asia/Shanghai`); otherwise allocate **undeleted MAX(create_date) + one day**, inside an immediate transaction. Deleting the last entry can reuse its date; all-soft-deleted is not a physically empty table. Do not add a date high-water mark or fill middle holes.
- New/edited content is trimmed, nonempty and at most 10000 Unicode scalar values; JSON body limit is 64 KiB (login 8 KiB). Only content is editable. PATCH/DELETE accept optional quoted positive `If-Match`: omission acts on the latest record, not 428; supplied malformed values yield 400, stale active versions 412. The web UI still sends versions and never auto-merges. Deleted IDs return 404 after valid preconditions; concurrent same-version deletes yield one 204 and one 404. Version overflow returns 409 without deleting. Detail/create/update return ETag.
- Diary DELETE is soft: preserve content, create_date and created_at; set deleted_at to UTC RFC3339 milliseconds, updated_at to the same value, and increment version. List q/date/total/pagination and detail all filter deleted_at IS NULL. DTOs keep their original six fields without deleted_at; POST/PATCH cannot set it or restore a row. No restore, recycle-bin or permanent-purge API exists this phase. Collection deletion remains hard. Neither promises secure erasure; UI confirmation/success must explain hidden entries, retained content and no current restore feature.
- Schema v1 adds diaries/sessions/ledger/migrations; v2 adds nullable deleted_at; v3 adds browser_totp_state. Empty/legacy-collection databases and v1/v2 upgrade transactionally to v3 with history 1,2,3, preserving original diary fields, sessions, favorites, ledger and ID sequences. Preflight is read-only before WAL and repeated inside the initialization transaction; reject foreign/future/incomplete schemas. New Unix DB files use 0600; existing permissions do not change. Preserve historical duplicate dates, blank content and nullable/opaque timestamps.
- `memento hash-password` reads a confirmed password without echo; `--stdin` is for test/integration. Explicitly validate nonempty and <=72 UTF-8 bytes before bcrypt::hash with DEFAULT_COST=12; no custom 12-character minimum. Do not substitute non_truncating_hash, which rejects exactly 72 bytes. `totp-secret` generates a new 20-byte Base32 secret to the user's terminal; configure the authenticator locally and securely (6 digits, SHA1, 30 seconds, +/-1 step), independent of domain. `init-db <path>` initializes an explicit target offline. Prefer umask 077 and single-quoted hash values to avoid dollar expansion.
- `scripts/import_diaries.py` requires complete v3 metadata including browser_totp_state, without reading its actual state; defaults to read-only dry-run, with --apply writing only the stopped, backed-up target. Never use the source as MEMENTO_DB_PATH. Source fingerprints retain five fields; new imports set deleted_at=NULL. First import requires physically empty target diaries, not all-soft-deleted rows. Matching ledger entries skip without overwriting edits or resurrecting deleted rows. Soft-deleted diary_id retains the original row; ON DELETE SET NULL only supports historical physical deletion. Untracked source IDs must exceed the target pre-import sqlite_sequence high watermark, even after hard deletion. Never reset sequence/ledger to bypass conflicts; restore consistent backups for rollback.
- No stats, goal, separate JSON import/export API or audit subsystem is in scope. `static/` remains dependency-free; drafts are page-local, not durable storage.

Validation order: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`, `cargo build --release`, then `python3 -B -W error::ResourceWarning -m unittest discover -s tests -p test_import_diaries.py -v`. Optional `python3 -B tests/browser_smoke.py` needs agent-browser; `python3 -B tests/nginx_smoke.py /path/to/nginx` needs Nginx and openssl for temporary certificates. Both use isolated fixtures, not system services. See operations for execution evidence and coverage scope; rerun after changes rather than inferring current correctness or deployment from historical test totals.

## Data model (one table, JSON `extra` bag)

All three item types share the single `favorites` table (`db.rs`). Common columns (`name`, `url`, `aka`, `genres`, `rating`, `summary`, `release_date`, `sort_date`, …) are first-class; **type-specific fields live in the `extra` JSON object**. Listed in `models.rs::EXTRA_FIELD_KEYS` (e.g. game: `developer`/`publisher`/`platforms`; movie: `director`/`cast`/`imdb`; book: `author`/`isbn`/`pages`).

Key normalisation rules in `models.rs`:
- Convenience top-level type fields are **folded into `extra`** by `fold_extra` (top-level loses to an explicit `extra` key). On UPDATE, the `extra` patch is **merged** with existing stored `extra` (`repo::merge_extra`) — partial updates never drop sibling keys.
- `type` accepts English or Chinese (`game`/`游戏`, etc.); stored canonically lowercase-English.
- `aka` accepts string or array; `genres` accepts string (→ single-element array) or array. Both stored as JSON text.
- `rating` validated to `0..=10`.
- `url` must be `http(s)` (rejects `javascript:`/`data:` — stored-XSS defence).

## Images

Stored as a `BLOB` + `image_mime` in the same row. The list/get DTO **never** includes raw bytes — it exposes `has_image` and an `image_url` pointing at `GET /api/favorites/{id}/image`. Two inline input sources (`image_base64` — bare or `data:` URL — and `image_url`, which the server fetches), plus the dedicated `POST /…/image` (multipart `image` field or raw `image/*` body). At most one inline source per write; **create requires exactly one** (`PreparedCreate::from_body` rejects an imageless create with 400), while update treats it as optional (omitted = keep existing image).

Security invariants in `image.rs` — preserve them:
- **MIME is always sniffed from magic bytes** (`detect_image_mime`), never trusted from the declared header/data-URL. Non-raster content (HTML/JS/SVG) is rejected. This pairs with the app-wide `nosniff` header.
- Remote fetch (`fetch_image_blocking`) is **SSRF-guarded** (`guard_public_url`): http(s) only, DNS resolved up front, any private/loopback/link-local/CGNAT/ULA address rejected, and **redirects disabled** so a public URL can't bounce into the internal network.
- 10 MiB size cap (`MAX_IMAGE_BYTES`).

## Conventions specific to this project

- API-key check uses **constant-time comparison** (`subtle::ConstantTimeEq`) in `auth.rs` — don't replace with `==`.
- Follow the existing immutability style: validation produces new `Prepared*` structs rather than mutating the parsed body.
- Frontend is **embedded in release builds**: after editing `static/`, rebuild and restart the delivery binary. Default debug builds read assets from the filesystem.
- `static/` is plain vanilla HTML/CSS/JS (no bundler, no `package.json`). Keep it dependency-free.
