#!/usr/bin/env python3
"""One-time import of Navidrome playlists into the gateway-owned store.

Decision D6 of the user-system plan moved playlist ownership off Navidrome
onto the gateway (`/v1/playlists/*`, migration `0007_playlists.sql`) so
playlists can be private per-user. Existing Navidrome playlists predate
that table; this script copies them into the **owner** (user_id=1) once.

It needs no Navidrome credentials of its own — it reads playlists through
the gateway's still-verbatim `/rest/*` proxy and writes them through the
new `/v1/playlists/*` endpoints, all with one gateway bearer:

    1. GET  /v1/playlists                      → names already imported
    2. GET  /rest/getPlaylists                 → Navidrome's playlists
    3. for each not-already-present by name:
         GET  /rest/getPlaylist?id=…           → ordered track ids
         POST /v1/playlists {name}             → new gateway playlist id
         PUT  /v1/playlists/<id>/tracks {…}    → set membership (replace)

Idempotent: a playlist whose **name** already exists in /v1/playlists is
skipped, so re-runs are safe. (Renaming an imported playlist and re-running
would re-import the original name — acceptable for a one-shot tool.)

Required env:
    GATEWAY_URL       base URL e.g. https://crates.example.com:8443
    GATEWAY_BEARER    OAuth access token (owner/admin) or static bearer

Optional env:
    DRY_RUN=1         list what would be imported, write nothing
    CA_CERT_PATH      PEM with the CA that issued the gateway cert (mkcert
                      root CA: `mkcert -CAROOT`). Keeps TLS verification ON.
    INSECURE=1        DISABLE TLS verification (loud warning; leaks the
                      bearer to a MITM). Prefer CA_CERT_PATH.
"""

from __future__ import annotations

import json
import os
import ssl
import sys
import urllib.error
import urllib.parse
import urllib.request

SUBSONIC_VERSION = "1.16.1"
CLIENT_NAME = "crates-music-playlist-import"


def die(msg: str, code: int = 1) -> None:
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(code)


def envreq(key: str) -> str:
    val = os.environ.get(key, "").strip()
    if not val:
        die(f"missing required env var: {key}")
    return val


def make_ssl_ctx(ca_cert_path: str, insecure: bool) -> ssl.SSLContext | None:
    if ca_cert_path:
        return ssl.create_default_context(cafile=ca_cert_path)
    if insecure:
        print(
            "WARNING: INSECURE=1 — TLS verification DISABLED; your bearer "
            "token is exposed to a MITM. Prefer CA_CERT_PATH (`mkcert -CAROOT`).",
            file=sys.stderr,
        )
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        return ctx
    return None


def http_json(
    method: str,
    url: str,
    bearer: str,
    ssl_ctx: ssl.SSLContext | None,
    body: dict | None = None,
) -> tuple[int, dict]:
    data = json.dumps(body).encode("utf-8") if body is not None else None
    headers = {"Authorization": f"Bearer {bearer}"}
    if data is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, context=ssl_ctx, timeout=30) as resp:
            raw = resp.read().decode("utf-8")
            return resp.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", errors="replace")
        try:
            payload = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            payload = {"_raw": raw}
        return e.code, payload


def subsonic_get(
    gateway: str, path: str, params: dict[str, str], bearer: str, ssl_ctx
) -> dict:
    """GET a /rest/* endpoint through the gateway proxy; unwrap the
    `subsonic-response` envelope."""
    q = {**params, "v": SUBSONIC_VERSION, "c": CLIENT_NAME, "f": "json"}
    url = f"{gateway}{path}?{urllib.parse.urlencode(q)}"
    status, payload = http_json("GET", url, bearer, ssl_ctx)
    if status != 200:
        die(f"GET {path} → HTTP {status}: {payload}")
    env = payload.get("subsonic-response", {})
    if env.get("status") != "ok":
        die(f"GET {path} → subsonic error: {env.get('error')}")
    return env


def main() -> None:
    gateway = envreq("GATEWAY_URL").rstrip("/")
    bearer = envreq("GATEWAY_BEARER")
    dry_run = os.environ.get("DRY_RUN", "").strip() in ("1", "true", "yes")
    ssl_ctx = make_ssl_ctx(
        os.environ.get("CA_CERT_PATH", "").strip(),
        os.environ.get("INSECURE", "").strip() in ("1", "true", "yes"),
    )

    # 1. Existing gateway playlists → names to skip (idempotency).
    status, payload = http_json("GET", f"{gateway}/v1/playlists", bearer, ssl_ctx)
    if status != 200:
        die(f"GET /v1/playlists → HTTP {status}: {payload}")
    existing = {p["name"] for p in payload.get("playlists", [])}
    print(f"gateway already has {len(existing)} playlist(s)")

    # 2. Navidrome playlists via the proxy.
    nd = subsonic_get(gateway, "/rest/getPlaylists", {}, bearer, ssl_ctx)
    nav_playlists = nd.get("playlists", {}).get("playlist", [])
    print(f"navidrome reports {len(nav_playlists)} playlist(s)")

    imported = 0
    skipped = 0
    for pl in nav_playlists:
        name = pl.get("name", "").strip()
        pid = pl.get("id")
        if not name or pid is None:
            continue
        if name in existing:
            skipped += 1
            print(f"  skip  '{name}' (already present)")
            continue

        detail = subsonic_get(gateway, "/rest/getPlaylist", {"id": str(pid)}, bearer, ssl_ctx)
        entries = detail.get("playlist", {}).get("entry", [])
        track_ids = [e["id"] for e in entries if e.get("id")]

        if dry_run:
            print(f"  DRY   '{name}' → {len(track_ids)} track(s)")
            imported += 1
            continue

        status, created = http_json(
            "POST", f"{gateway}/v1/playlists", bearer, ssl_ctx, {"name": name}
        )
        if status != 201:
            die(f"create '{name}' → HTTP {status}: {created}")
        new_id = created["id"]

        if track_ids:
            status, resp = http_json(
                "PUT",
                f"{gateway}/v1/playlists/{urllib.parse.quote(new_id)}/tracks",
                bearer,
                ssl_ctx,
                {"track_ids": track_ids, "mode": "replace"},
            )
            if status != 204:
                die(f"set tracks for '{name}' → HTTP {status}: {resp}")

        imported += 1
        print(f"  import '{name}' → {len(track_ids)} track(s)")

    verb = "would import" if dry_run else "imported"
    print(f"done: {verb} {imported}, skipped {skipped}")


if __name__ == "__main__":
    main()
