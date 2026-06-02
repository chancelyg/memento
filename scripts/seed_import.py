#!/usr/bin/env python3
"""
seed_import.py — backfill memento from the original 海报墙 site.

Reads every item from the OLD site's JSON API (paging until `total` is reached),
maps each Chinese type to its canonical English value, folds the OLD `data.*`
bag into the new top-level + extra payload, downloads each poster (when
`has_image`), base64-encodes it, and POSTs to the NEW memento write API using
the `X-API-Key` header.

Standard library only (urllib) — no third-party dependencies required.

Usage:
    export MEMENTO_API_KEY=...            # required for the NEW site writes
    python3 seed_import.py
    python3 seed_import.py --old http://147.79.20.135:23456 \
                           --new http://localhost:23457 \
                           --skip-existing

Env fallbacks:
    MEMENTO_OLD_URL   default http://147.79.20.135:23456
    MEMENTO_NEW_URL   default http://localhost:23457
    MEMENTO_API_KEY   write-auth key for the NEW site (required)

Idempotency note:
    The simple default re-inserts every OLD item (running twice => duplicates).
    Pass --skip-existing to first fetch the NEW site's current names and skip
    any OLD item whose `name` already exists. This is a best-effort de-dupe by
    name only; it is NOT a true upsert. For a clean reimport, delete the NEW
    db file (MEMENTO_DB_PATH) and start fresh.
"""

import argparse
import base64
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

# Chinese -> canonical English type. Already-English values pass through.
TYPE_MAP = {"游戏": "game", "电影": "movie", "图书": "book",
            "game": "game", "movie": "movie", "book": "book"}

# Fields that, when present in OLD item.data, are passed at the NEW payload's
# top level so the server folds them into `extra`. (genres/aka/release_date are
# first-class columns and handled separately.)
EXTRA_PASSTHROUGH = (
    # game
    "developer", "publisher", "platforms",
    # movie
    "director", "writers", "cast", "country", "language", "duration", "imdb",
    # book
    "author", "isbn", "pages", "price", "binding", "series",
)

DEFAULT_OLD = os.environ.get("MEMENTO_OLD_URL", "http://147.79.20.135:23456")
DEFAULT_NEW = os.environ.get("MEMENTO_NEW_URL", "http://localhost:23457")


def http_json(url, *, method="GET", data=None, headers=None, timeout=30):
    """Perform an HTTP request and decode a JSON response."""
    body = None
    hdrs = dict(headers or {})
    if data is not None:
        body = json.dumps(data).encode("utf-8")
        hdrs.setdefault("Content-Type", "application/json")
    req = urllib.request.Request(url, data=body, headers=hdrs, method=method)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        raw = resp.read()
        return resp.status, json.loads(raw.decode("utf-8")) if raw else {}


def http_bytes(url, *, timeout=30):
    """Fetch raw bytes (used for the OLD poster image). Returns (status, bytes, mime)."""
    req = urllib.request.Request(url, method="GET")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.status, resp.read(), resp.headers.get("Content-Type", "")


def fetch_old_items(old_base, timeout):
    """Page through the OLD API until all `total` items are collected."""
    items, page, total = [], 1, None
    while True:
        url = f"{old_base.rstrip('/')}/api/favorites?page={page}&per_page=100"
        status, payload = http_json(url, timeout=timeout)
        if status != 200 or not payload.get("success", True):
            raise RuntimeError(f"OLD fetch failed (status={status}): {payload}")
        data = payload.get("data", {})
        batch = data.get("favorites", [])
        total = data.get("total", total)
        if not batch:
            break
        items.extend(batch)
        print(f"  fetched page {page}: +{len(batch)} (have {len(items)}"
              f"{'/' + str(total) if total is not None else ''})")
        if total is not None and len(items) >= total:
            break
        page += 1
    return items, (total if total is not None else len(items))


def build_payload(old_item):
    """Map an OLD item into a NEW /api/favorites create payload."""
    cn_type = (old_item.get("type") or "").strip()
    new_type = TYPE_MAP.get(cn_type)
    if new_type is None:
        raise ValueError(f"unknown type {cn_type!r} for item id={old_item.get('id')}")

    bag = old_item.get("data") or {}
    payload = {
        "type": new_type,
        "name": old_item.get("name") or "",
        "url": old_item.get("url"),
    }

    # First-class columns sourced from the OLD data bag.
    if "aka" in bag:
        payload["aka"] = bag["aka"]            # array or string — server accepts both
    if "genres" in bag:
        payload["genres"] = bag["genres"]
    if "release_date" in bag:
        rd = bag["release_date"]
        # OLD movie release_date is sometimes a list; keep the first as the column value.
        payload["release_date"] = rd[0] if isinstance(rd, list) and rd else rd

    # sort_date defaults to the OLD create_time so chronological ordering is preserved.
    create_time = old_item.get("create_time")
    if create_time:
        payload["sort_date"] = str(create_time)[:10]  # YYYY-MM-DD

    # Type-specific fields -> top level; the server folds them into `extra`.
    for key in EXTRA_PASSTHROUGH:
        if key in bag and bag[key] not in (None, "", []):
            payload[key] = bag[key]

    return payload


def attach_image(payload, old_base, old_id, timeout):
    """Download the OLD poster and embed it as base64 in the payload."""
    url = f"{old_base.rstrip('/')}/api/favorites/{old_id}/image"
    try:
        status, raw, mime = http_bytes(url, timeout=timeout)
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return False
        raise
    if status != 200 or not raw:
        return False
    b64 = base64.b64encode(raw).decode("ascii")
    mime = mime or "image/jpeg"
    payload["image_base64"] = f"data:{mime};base64,{b64}"
    return True


def fetch_existing_names(new_base, api_key, timeout):
    """Collect names already present on the NEW site (best-effort de-dupe)."""
    names, page, total = set(), 1, None
    headers = {"X-API-Key": api_key}
    while True:
        url = f"{new_base.rstrip('/')}/api/favorites?page={page}&per_page=100"
        status, payload = http_json(url, headers=headers, timeout=timeout)
        if status != 200:
            break
        data = payload.get("data", {})
        items = data.get("items", [])
        total = data.get("total", total)
        if not items:
            break
        for it in items:
            if it.get("name"):
                names.add(it["name"])
        if total is not None and len(names) >= total:
            break
        page += 1
    return names


def post_new(new_base, api_key, payload, timeout):
    """POST one create payload to the NEW write API."""
    url = f"{new_base.rstrip('/')}/api/favorites"
    headers = {"X-API-Key": api_key}
    try:
        status, body = http_json(url, method="POST", data=payload,
                                 headers=headers, timeout=timeout)
        return status, body
    except urllib.error.HTTPError as exc:
        try:
            body = json.loads(exc.read().decode("utf-8"))
        except Exception:
            body = {"error": exc.reason}
        return exc.code, body


def main():
    parser = argparse.ArgumentParser(description="Seed memento from the OLD 海报墙 site.")
    parser.add_argument("--old", default=DEFAULT_OLD, help=f"OLD base url (default {DEFAULT_OLD})")
    parser.add_argument("--new", default=DEFAULT_NEW, help=f"NEW base url (default {DEFAULT_NEW})")
    parser.add_argument("--api-key", default=os.environ.get("MEMENTO_API_KEY"),
                        help="NEW site write key (default $MEMENTO_API_KEY)")
    parser.add_argument("--skip-existing", action="store_true",
                        help="skip OLD items whose name already exists on the NEW site")
    parser.add_argument("--no-images", action="store_true", help="do not download/attach posters")
    parser.add_argument("--timeout", type=int, default=30, help="per-request timeout seconds")
    parser.add_argument("--limit", type=int, default=0, help="only import the first N items (0 = all)")
    args = parser.parse_args()

    if not args.api_key:
        print("ERROR: MEMENTO_API_KEY is required (env or --api-key).", file=sys.stderr)
        return 2

    print(f"OLD: {args.old}\nNEW: {args.new}")
    print("Fetching OLD items...")
    try:
        items, total = fetch_old_items(args.old, args.timeout)
    except Exception as exc:
        print(f"ERROR fetching OLD items: {exc}", file=sys.stderr)
        return 1
    print(f"Fetched {len(items)} items (reported total {total}).")

    existing = set()
    if args.skip_existing:
        print("Fetching existing NEW names for de-dupe...")
        existing = fetch_existing_names(args.new, args.api_key, args.timeout)
        print(f"  {len(existing)} names already present on NEW site.")

    if args.limit:
        items = items[: args.limit]

    created = skipped = with_image = failed = 0
    for idx, old_item in enumerate(items, start=1):
        old_id = old_item.get("id")
        name = old_item.get("name", "?")
        if args.skip_existing and name in existing:
            skipped += 1
            print(f"[{idx}/{len(items)}] SKIP existing: {name}")
            continue
        try:
            payload = build_payload(old_item)
        except ValueError as exc:
            failed += 1
            print(f"[{idx}/{len(items)}] SKIP (bad data): {exc}", file=sys.stderr)
            continue

        if not args.no_images and old_item.get("has_image"):
            try:
                if attach_image(payload, args.old, old_id, args.timeout):
                    with_image += 1
            except Exception as exc:
                print(f"[{idx}/{len(items)}] image download failed for id={old_id}: {exc}",
                      file=sys.stderr)

        status, body = post_new(args.new, args.api_key, payload, args.timeout)
        if status == 201 and body.get("success"):
            created += 1
            new_id = (body.get("data") or {}).get("id")
            print(f"[{idx}/{len(items)}] OK  {payload['type']:5} -> id={new_id}  {name}")
        elif status == 401:
            print("ERROR: 401 Unauthorized — check MEMENTO_API_KEY.", file=sys.stderr)
            return 1
        else:
            failed += 1
            err = body.get("error") if isinstance(body, dict) else body
            print(f"[{idx}/{len(items)}] FAIL ({status}): {name} — {err}", file=sys.stderr)

    print("\n=== Seed import summary ===")
    print(f"  created : {created}")
    print(f"  skipped : {skipped}")
    print(f"  failed  : {failed}")
    print(f"  with poster image : {with_image}")
    print(f"  total processed   : {len(items)}")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
