#!/usr/bin/env python3
"""Walk a Navidrome catalog and enqueue every track for embedding.

**Mostly superseded.** The gateway now discovers new tracks by itself:
`crates/music-gateway/src/discovery.rs` sweeps the catalog at boot and on
a timer, and `POST /v1/admin/discovery/scan` runs a full sweep on demand.
Reach for one of those first — they need no Navidrome credentials and no
local Python.

This script is still the tool for the cases the in-gateway watcher can't
cover: enqueueing from *outside* the gateway (a different machine, a
cron host), running against a Navidrome the gateway isn't pointed at, or
counting a catalog without writing anything (`DRY_RUN=1`).

Pipeline:
    1. Page Navidrome's `/rest/getAlbumList2?type=alphabeticalByName` until empty.
    2. For each album, call `/rest/getAlbum?id=<albumId>` and collect song ids.
    3. POST batches to the gateway's `/v1/recommend/enqueue`.

Idempotent: the gateway's enqueue uses `INSERT OR IGNORE` on
`(track_id, model_version)`, so re-runs do nothing harmful — already-
embedded tracks stay skipped, in-flight tracks stay queued.

Required env:
    NAVIDROME_URL          base URL e.g. http://192.0.2.68:30043
    NAVIDROME_USERNAME     Subsonic user
    NAVIDROME_PASSWORD     plain-text password (used with Subsonic salt/token)
    GATEWAY_URL            base URL e.g. https://crates.example.com:8443
    GATEWAY_BEARER         OAuth access token or static bearer for the gateway

Optional env:
    BATCH_SIZE             tracks per enqueue POST (default: 500)
    DRY_RUN=1              walk Navidrome and count, skip gateway POSTs
    CA_CERT_PATH           PEM file with the CA that issued the gateway cert
                           (e.g. the mkcert root CA, `mkcert -CAROOT`). Added
                           as a trust anchor so verification stays ON.
    INSECURE=1             DISABLE TLS verification entirely (loud warning;
                           exposes the bearer token to a MITM). Prefer
                           CA_CERT_PATH.
"""

from __future__ import annotations

import hashlib
import json
import os
import secrets
import ssl
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


SUBSONIC_VERSION = "1.16.1"
CLIENT_NAME = "crates-music-bulk-enqueue"
ALBUM_PAGE_SIZE = 500


def die(msg: str, code: int = 1) -> None:
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(code)


def envreq(key: str) -> str:
    val = os.environ.get(key, "").strip()
    if not val:
        die(f"missing required env var: {key}")
    return val


def subsonic_auth_params(username: str, password: str) -> dict[str, str]:
    """Subsonic's salted-MD5 auth (token+salt). Avoids sending plaintext.

    The server reconstructs md5(password + salt) and compares.
    """
    salt = secrets.token_hex(8)
    token = hashlib.md5((password + salt).encode("utf-8")).hexdigest()
    return {
        "u": username,
        "t": token,
        "s": salt,
        "v": SUBSONIC_VERSION,
        "c": CLIENT_NAME,
        "f": "json",
    }


def make_ssl_ctx(ca_cert_path: str, insecure: bool) -> ssl.SSLContext | None:
    """Build the TLS context for gateway/Navidrome calls.

    Priority:
      1. CA_CERT_PATH set  → verify against the system store *plus* that CA
         (the secure way to trust an mkcert `gateway.local` cert).
      2. INSECURE=1        → disable verification entirely, with a loud
         stderr warning. Escape hatch only; leaks the bearer to a MITM.
      3. neither           → None, i.e. urllib's default system-store
         verification.
    """
    if ca_cert_path:
        ctx = ssl.create_default_context(cafile=ca_cert_path)
        return ctx
    if insecure:
        print(
            "WARNING: INSECURE=1 — TLS certificate verification is DISABLED, "
            "so anyone who can intercept the connection can read your bearer "
            "token. Set CA_CERT_PATH to the mkcert root CA instead "
            "(`mkcert -CAROOT`).",
            file=sys.stderr,
        )
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        return ctx
    return None


def http_get_json(url: str, params: dict[str, str], ssl_ctx: ssl.SSLContext | None) -> dict:
    full = f"{url}?{urllib.parse.urlencode(params)}"
    req = urllib.request.Request(full, method="GET")
    with urllib.request.urlopen(req, context=ssl_ctx, timeout=30) as resp:
        return json.loads(resp.read().decode("utf-8"))


def http_post_json(
    url: str,
    body: dict,
    bearer: str,
    ssl_ctx: ssl.SSLContext | None,
) -> tuple[int, dict]:
    data = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=data,
        method="POST",
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {bearer}",
        },
    )
    try:
        with urllib.request.urlopen(req, context=ssl_ctx, timeout=30) as resp:
            payload = json.loads(resp.read().decode("utf-8"))
            return resp.status, payload
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")
        die(f"gateway POST failed: {e.code} {e.reason} — {body[:300]}")


def walk_navidrome(
    nav_url: str,
    auth: dict[str, str],
    ssl_ctx: ssl.SSLContext | None,
) -> list[str]:
    """Page album list, expand each album to its songs, return all track ids."""
    album_ids: list[str] = []
    offset = 0
    print(f"navidrome: paging albums (size={ALBUM_PAGE_SIZE}) ...", flush=True)
    while True:
        params = {
            **auth,
            "type": "alphabeticalByName",
            "size": str(ALBUM_PAGE_SIZE),
            "offset": str(offset),
        }
        data = http_get_json(f"{nav_url}/rest/getAlbumList2", params, ssl_ctx)
        resp = data.get("subsonic-response") or {}
        if resp.get("status") != "ok":
            die(f"navidrome getAlbumList2 failed: {resp.get('error') or resp}")
        albums = (resp.get("albumList2") or {}).get("album") or []
        if not albums:
            break
        album_ids.extend(a["id"] for a in albums)
        print(f"  +{len(albums)} albums (total {len(album_ids)})", flush=True)
        if len(albums) < ALBUM_PAGE_SIZE:
            break
        offset += ALBUM_PAGE_SIZE

    print(f"navidrome: expanding {len(album_ids)} albums to tracks ...", flush=True)
    track_ids: list[str] = []
    t0 = time.monotonic()
    for i, aid in enumerate(album_ids, 1):
        params = {**auth, "id": aid}
        data = http_get_json(f"{nav_url}/rest/getAlbum", params, ssl_ctx)
        resp = data.get("subsonic-response") or {}
        if resp.get("status") != "ok":
            print(f"  warn: getAlbum({aid}) failed: {resp.get('error')}", file=sys.stderr)
            continue
        songs = (resp.get("album") or {}).get("song") or []
        track_ids.extend(s["id"] for s in songs)
        if i % 50 == 0 or i == len(album_ids):
            rate = i / max(time.monotonic() - t0, 0.001)
            print(
                f"  {i}/{len(album_ids)} albums  →  {len(track_ids)} tracks "
                f"({rate:.1f} albums/s)",
                flush=True,
            )
    return track_ids


def enqueue_in_batches(
    gateway_url: str,
    bearer: str,
    track_ids: list[str],
    batch_size: int,
    ssl_ctx: ssl.SSLContext | None,
) -> int:
    url = f"{gateway_url}/v1/recommend/enqueue"
    total_attempted = 0
    for i in range(0, len(track_ids), batch_size):
        batch = track_ids[i : i + batch_size]
        status, body = http_post_json(url, {"track_ids": batch}, bearer, ssl_ctx)
        enqueued = body.get("enqueued", 0)
        total_attempted += enqueued
        print(
            f"  batch {i // batch_size + 1}: posted={len(batch)} "
            f"enqueued={enqueued} (HTTP {status})",
            flush=True,
        )
    return total_attempted


def main() -> int:
    nav_url = envreq("NAVIDROME_URL").rstrip("/")
    nav_user = envreq("NAVIDROME_USERNAME")
    nav_pass = envreq("NAVIDROME_PASSWORD")
    gw_url = envreq("GATEWAY_URL").rstrip("/")
    gw_bearer = envreq("GATEWAY_BEARER")
    batch_size = int(os.environ.get("BATCH_SIZE", "500"))
    dry_run = os.environ.get("DRY_RUN") == "1"
    ca_cert_path = os.environ.get("CA_CERT_PATH", "").strip()
    insecure = os.environ.get("INSECURE") == "1"

    ssl_ctx = make_ssl_ctx(ca_cert_path, insecure)
    auth = subsonic_auth_params(nav_user, nav_pass)

    print(f"navidrome: {nav_url}")
    print(f"gateway:   {gw_url}")
    print(f"batch:     {batch_size}{'  (DRY RUN)' if dry_run else ''}")
    print()

    track_ids = walk_navidrome(nav_url, auth, ssl_ctx)
    print()
    print(f"collected {len(track_ids)} track ids")

    if dry_run:
        print("DRY_RUN=1 — skipping gateway POSTs")
        return 0

    print()
    print(f"gateway: enqueueing in batches of {batch_size} ...")
    total = enqueue_in_batches(gw_url, gw_bearer, track_ids, batch_size, ssl_ctx)
    print()
    print(f"done. posted {len(track_ids)} ids, gateway reported {total} insert attempts")
    print("watch the embedder access log on the GPU host for /embed/audio calls")
    return 0


if __name__ == "__main__":
    sys.exit(main())
