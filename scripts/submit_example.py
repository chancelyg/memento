#!/usr/bin/env python3
"""
submit_example.py — minimal Telegram-bot-style client for memento's write API.

This is the smallest useful `submit_favorite(...)` helper you would drop into a
Telegram bot handler (or an openclaw-style agent tool) to POST a new poster-wall
entry. It uses ONLY the Python standard library (urllib) so it runs anywhere with
zero `pip install`.

The write API is guarded by the `X-API-Key` header. This script reads the key
from the MEMENTO_API_KEY environment variable.

Quick start:
    export MEMENTO_API_KEY=...
    python3 submit_example.py                       # posts a demo game entry
    python3 submit_example.py --base http://localhost:23457

As a library (e.g. inside a bot handler):
    from submit_example import submit_favorite
    dto = submit_favorite(
        item_type="movie",
        name="奥本海默 Oppenheimer",
        url="https://movie.douban.com/subject/35556001/",
        director="克里斯托弗·诺兰",
        country="美国",
        genres=["剧情", "传记", "历史"],
        release_date="2023-08-30",
        image_url="https://example.com/poster.jpg",
    )
    print(dto["id"])
"""

import argparse
import json
import os
import sys
import urllib.error
import urllib.request

DEFAULT_BASE = os.environ.get("MEMENTO_BASE_URL", "http://localhost:23457")

# Type-specific keys the server accepts at the top level (folded into `extra`).
_EXTRA_KEYS = {
    "developer", "publisher", "platforms",                              # game
    "director", "writers", "cast", "country", "language",
    "duration", "imdb",                                                 # movie
    "author", "isbn", "pages", "price", "binding", "series",            # book
}


class MementoError(Exception):
    """Raised when the API returns a non-success response."""

    def __init__(self, status, message):
        super().__init__(f"[{status}] {message}")
        self.status = status
        self.message = message


def submit_favorite(
    item_type,
    name,
    *,
    base_url=None,
    api_key=None,
    url=None,
    aka=None,
    genres=None,
    release_date=None,
    rating=None,
    summary=None,
    sort_date=None,
    image_base64=None,
    image_url=None,
    timeout=30,
    **extra_fields,
):
    """
    Create a favorite via POST /api/favorites and return the FavoriteDto dict.

    Required:
        item_type : "game" | "movie" | "book" (Chinese 游戏/电影/图书 also accepted)
        name      : display title

    Optional common fields: url, aka, genres, release_date, rating, summary, sort_date.
    Poster (provide AT MOST ONE): image_base64 (str or data: URL) OR image_url.
    Type-specific fields (developer, director, author, ...) may be passed as
    keyword args via **extra_fields; the server folds them into `extra`.

    Raises MementoError on 400/401/404/other non-201 responses.
    """
    base_url = (base_url or DEFAULT_BASE).rstrip("/")
    api_key = api_key if api_key is not None else os.environ.get("MEMENTO_API_KEY")
    if not api_key:
        raise MementoError(0, "MEMENTO_API_KEY not set (env or api_key= argument)")
    if not item_type or not name:
        raise MementoError(0, "item_type and name are required")
    if image_base64 and image_url:
        raise MementoError(0, "provide at most one of image_base64 / image_url")

    payload = {"type": item_type, "name": name}
    # Optional common fields — only include when set.
    for key, value in (
        ("url", url), ("aka", aka), ("genres", genres),
        ("release_date", release_date), ("rating", rating),
        ("summary", summary), ("sort_date", sort_date),
        ("image_base64", image_base64), ("image_url", image_url),
    ):
        if value is not None:
            payload[key] = value

    # Type-specific top-level fields (server folds these into `extra`).
    for key, value in extra_fields.items():
        if key not in _EXTRA_KEYS:
            raise MementoError(0, f"unknown field {key!r} (not a recognized extra field)")
        if value is not None:
            payload[key] = value

    body = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(
        f"{base_url}/api/favorites",
        data=body,
        method="POST",
        headers={"Content-Type": "application/json", "X-API-Key": api_key},
    )

    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            status = resp.status
            data = json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        # 400 (validation), 401 (bad/missing key), 404, etc. carry a JSON envelope.
        try:
            data = json.loads(exc.read().decode("utf-8"))
            message = data.get("error") or exc.reason
        except Exception:
            message = exc.reason
        raise MementoError(exc.code, message) from None
    except urllib.error.URLError as exc:
        raise MementoError(0, f"connection failed: {exc.reason}") from None

    if status != 201 or not data.get("success"):
        raise MementoError(status, data.get("error") or "unexpected response")
    return data["data"]


def _demo(base_url):
    """Post one example of each type so you can see the wall fill up."""
    samples = [
        dict(
            item_type="game",
            name="哈迪斯2 Hades II",
            url="https://www.douban.com/game/36185144/",
            genres=["乱斗/清版", "角色扮演", "动作"],
            aka=["黑帝斯2"],
            release_date="2025-09-25",
            developer="Supergiant Games",
            publisher="Supergiant Games",
            platforms=["PC", "Mac", "PS5", "XSX", "Nintendo Switch"],
        ),
        dict(
            item_type="movie",
            name="奥本海默 Oppenheimer",
            url="https://movie.douban.com/subject/35556001/",
            genres=["剧情", "传记", "历史"],
            release_date="2023-08-30",
            director="克里斯托弗·诺兰",
            country="美国",
            language="英语",
            duration="180分钟",
        ),
        dict(
            item_type="book",
            name="三体",
            url="https://book.douban.com/subject/2567698/",
            release_date="2008",
            author="刘慈欣",
            publisher="重庆出版社",
            isbn="9787536692930",
            pages=302,
        ),
    ]
    for sample in samples:
        try:
            dto = submit_favorite(base_url=base_url, **sample)
            print(f"OK  {dto['type']:5} id={dto['id']}  {dto['name']}")
        except MementoError as exc:
            print(f"FAIL {sample['item_type']:5} {sample['name']}: {exc}", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description="Submit a favorite to memento.")
    parser.add_argument("--base", default=DEFAULT_BASE, help=f"base url (default {DEFAULT_BASE})")
    parser.add_argument("--type", dest="item_type", help="game|movie|book (or 游戏/电影/图书)")
    parser.add_argument("--name", help="display title")
    parser.add_argument("--url", help="source page url")
    parser.add_argument("--image-url", help="poster image url for the server to fetch")
    args = parser.parse_args()

    if not os.environ.get("MEMENTO_API_KEY"):
        print("ERROR: set MEMENTO_API_KEY in the environment.", file=sys.stderr)
        return 2

    # No explicit item -> run the built-in demo of one of each type.
    if not args.item_type or not args.name:
        print(f"No --type/--name given; posting demo entries to {args.base} ...")
        _demo(args.base)
        return 0

    try:
        dto = submit_favorite(
            item_type=args.item_type,
            name=args.name,
            base_url=args.base,
            url=args.url,
            image_url=args.image_url,
        )
    except MementoError as exc:
        print(f"FAILED: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(dto, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
