# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`memento` is a personal **poster wall (海报墙)** — a single-binary Rust web app that displays favorite **movies / games / books** as cards. Data is written via an `X-API-Key`-guarded JSON API (intended to be fed by a Telegram bot / agent tool); reads are public. The frontend (`static/`) is embedded into the binary at compile time via `rust-embed`.

## Commands

All commands run from this directory (`/root/codes/memento`).

```bash
cargo build                       # debug build
cargo build --release             # single-file binary → target/release/memento
cargo run                         # dev run (binds 0.0.0.0:23457 by default)
cargo test                        # run tests
cargo test <name>                 # run a single test by substring
cargo clippy --all-targets        # lint
cargo fmt                         # format
```

Config is read from the process environment, and the binary **auto-loads a `.env`** from the working directory at startup (`dotenvy::dotenv()` in `main.rs`, before tracing/config; existing env vars win, missing file is OK). Variables: `MEMENTO_API_KEY` (write auth; if unset a random ephemeral key is generated and logged **once** at WARN), `MEMENTO_DB_PATH` (default `./memento.db`), `MEMENTO_BIND` (default `0.0.0.0:23457`), `RUST_LOG` (default `info`); plus operator-customisable site display strings `MEMENTO_SITE_NAME` / `MEMENTO_SLOGAN` / `MEMENTO_ICON` (in `config::SiteConfig`, each with a built-in default), injected into `index.html` at serve time by `assets::index_handler` (string-replacing `{{SITE_NAME}}`/`{{SLOGAN}}`/`{{ICON}}` placeholders, HTML-escaped).

```bash
# Seed importer — backfills from the OLD 海报墙 site's API (urllib, stdlib only)
MEMENTO_API_KEY=... python3 scripts/seed_import.py --old http://147.79.20.135:23456 --new http://localhost:23457 --skip-existing
# Minimal write-API client (drop-in for a bot handler)
MEMENTO_API_KEY=... python3 scripts/submit_example.py --base http://localhost:23457
```

## Architecture

Axum 0.8 + Tokio. SQLite via `rusqlite` (bundled) behind an `r2d2` pool. The request flow is layered; respect these boundaries when editing:

```
main.rs (router, layers)
  └─ handlers/ (HTTP: extract, validate query, build ApiResponse)
       └─ models.rs (validate + normalise request body → Prepared{Create,Update})
       └─ repo.rs   (pure SQL against a pooled Connection)
            └─ db.rs (pool + idempotent schema)
```

- **`lib.rs`** (`build_router`; `main.rs` is a thin shim calling `memento::run`) splits routes into a public `Router` and a `guarded` `Router` (write routes + `GET /api/auth/verify` wrapped in `auth::require_api_key` + a 20 MiB body limit). `GET /api/auth/verify` is a key-check endpoint: reaching the handler means the key was valid (returns `{valid:true}`); a bad/missing key is rejected upstream with 401. Security headers (`nosniff`, `X-Frame-Options: DENY`, `no-referrer`) and permissive CORS are applied app-wide. The crate has both a `[lib]` and `[[bin]]` target (same name) so integration tests can drive `build_router`.
- **Blocking DB work** (`rusqlite` is sync) is always run inside `tokio::task::spawn_blocking`. Handlers `.clone()` `state.pool` into the closure. Keep this pattern — never call `repo::*` directly on the async runtime.
- **`repo.rs`** functions take a `&Connection` and are pure data access (no HTTP types). Dynamic `WHERE`/`SET` clauses use parameterized binds (`Box<dyn ToSql>`) — never string-interpolate user values into SQL. LIKE search escapes `%`/`_` via `escape_like`.
- **`error.rs`** is the single error type. `AppError` maps each variant to an HTTP status and a **sanitized public message**; full detail is logged server-side only. Every endpoint returns the `ApiResponse { success, data, error }` envelope (errors use `error_envelope`).

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
- Frontend is **embedded at compile time** — after editing `static/`, rebuild the binary and restart for changes to take effect (a running server serves the old embedded copy).
- `static/` is plain vanilla HTML/CSS/JS (no bundler, no `package.json`). Keep it dependency-free.
