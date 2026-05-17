"""Render `gateway.toml` from environment variables.

The gateway Rust binary reads its config from a TOML file. In a
container deploy we don't want operators to mount a hand-crafted TOML
— we want plain env vars. This script bridges the two: env in, TOML
on stdout (or to a file via the CLI).

Pure function. The entrypoint shell script wraps this; tests cover
every branch without touching the filesystem or os.environ.

Required env:
    NAVIDROME_URL, NAVIDROME_USERNAME, NAVIDROME_PASSWORD,
    GATEWAY_BEARER_TOKEN

Optional env (with defaults):
    GATEWAY_LISTEN              0.0.0.0:8443
    GATEWAY_TLS_CERT            /data/certs/gateway.local.pem
    GATEWAY_TLS_KEY             /data/certs/gateway.local-key.pem
    GATEWAY_STATE_DB            /data/state/gateway-state.sqlite
    GATEWAY_CACHE_DB            /data/state/gateway-cache.sqlite
    GATEWAY_BROWSE_TTL_SECONDS  86400
    OAUTH_WEB_REDIRECT_URIS     https://gateway.local:8443/oauth/callback
    EMBEDDER_URL                (unset → no [embedder] section)
    EMBEDDER_TIMEOUT_SECONDS    30 (only used if EMBEDDER_URL set)
"""

from __future__ import annotations

import os
import sys
from typing import Mapping

DEFAULT_LISTEN = "0.0.0.0:8443"
DEFAULT_TLS_CERT = "/data/certs/gateway.local.pem"
DEFAULT_TLS_KEY = "/data/certs/gateway.local-key.pem"
DEFAULT_STATE_DB = "/data/state/gateway-state.sqlite"
DEFAULT_CACHE_PATH = "/data/state/gateway-cache.sqlite"
DEFAULT_BROWSE_TTL = 86400
DEFAULT_EMBEDDER_TIMEOUT = 30
DEFAULT_REDIRECT_URI = "https://gateway.local:8443/oauth/callback"

REQUIRED = (
    "NAVIDROME_URL",
    "NAVIDROME_USERNAME",
    "NAVIDROME_PASSWORD",
    "GATEWAY_BEARER_TOKEN",
)


class ConfigError(Exception):
    """Raised when env input is missing, empty, or unparseable.

    Surfaced as a non-zero exit from the CLI so the container fails
    fast at boot instead of starting the gateway with a broken config.
    """


def build_config(env: Mapping[str, str]) -> str:
    """Render a `gateway.toml` string from `env`.

    Validates required keys, applies defaults, and emits TOML in a
    fixed section order (server, upstream, cache, oauth, embedder).
    """
    for key in REQUIRED:
        value = env.get(key, "")
        if value == "":
            raise ConfigError(
                f"required env var {key} is missing or empty"
            )

    listen = env.get("GATEWAY_LISTEN", DEFAULT_LISTEN)
    tls_cert = env.get("GATEWAY_TLS_CERT", DEFAULT_TLS_CERT)
    tls_key = env.get("GATEWAY_TLS_KEY", DEFAULT_TLS_KEY)
    state_db = env.get("GATEWAY_STATE_DB", DEFAULT_STATE_DB)
    cache_path = env.get("GATEWAY_CACHE_DB", DEFAULT_CACHE_PATH)

    try:
        browse_ttl = int(env.get("GATEWAY_BROWSE_TTL_SECONDS", DEFAULT_BROWSE_TTL))
    except ValueError as e:
        raise ConfigError(f"GATEWAY_BROWSE_TTL_SECONDS must be an integer: {e}") from e

    redirect_raw = env.get("OAUTH_WEB_REDIRECT_URIS", DEFAULT_REDIRECT_URI)
    redirect_uris = [u.strip() for u in redirect_raw.split(",") if u.strip()]
    if not redirect_uris:
        raise ConfigError("OAUTH_WEB_REDIRECT_URIS resolved to an empty list")

    embedder_url = env.get("EMBEDDER_URL", "").strip()
    embedder_timeout: int | None = None
    if embedder_url:
        try:
            embedder_timeout = int(
                env.get("EMBEDDER_TIMEOUT_SECONDS", DEFAULT_EMBEDDER_TIMEOUT)
            )
        except ValueError as e:
            raise ConfigError(
                f"EMBEDDER_TIMEOUT_SECONDS must be an integer: {e}"
            ) from e

    parts: list[str] = []

    parts.append("[server]")
    parts.append(f'listen = {_str(listen)}')
    parts.append(f'tls_cert = {_str(tls_cert)}')
    parts.append(f'tls_key = {_str(tls_key)}')
    parts.append(f'bearer_token = {_str(env["GATEWAY_BEARER_TOKEN"])}')
    static_dir = env.get("WEB_STATIC_DIR", "").strip()
    if static_dir:
        parts.append(f'static_dir = {_str(static_dir)}')
    parts.append("")

    parts.append("[upstream]")
    parts.append(f'navidrome_url = {_str(env["NAVIDROME_URL"])}')
    parts.append(f'username = {_str(env["NAVIDROME_USERNAME"])}')
    parts.append(f'password = {_str(env["NAVIDROME_PASSWORD"])}')
    parts.append("")

    parts.append("[cache]")
    parts.append(f'path = {_str(cache_path)}')
    parts.append(f"browse_ttl_seconds = {browse_ttl}")
    parts.append("")

    parts.append("[oauth]")
    parts.append(f'state_db = {_str(state_db)}')
    parts.append("")

    parts.append("[[oauth.clients]]")
    parts.append('client_id = "web"')
    parts.append('name = "Web"')
    parts.append("redirect_uris = [")
    for uri in redirect_uris:
        parts.append(f"    {_str(uri)},")
    parts.append("]")
    parts.append("")

    if embedder_url:
        parts.append("[embedder]")
        parts.append(f"url = {_str(embedder_url)}")
        parts.append(f"timeout_seconds = {embedder_timeout}")
        parts.append("")

    return "\n".join(parts) + "\n"


def _str(value: str) -> str:
    """Render a TOML basic string. Escapes \\ and ", and rejects
    control chars — anything weirder than that doesn't belong in
    container env vars."""
    if any(ord(c) < 0x20 for c in value):
        raise ConfigError(
            f"control characters not allowed in config value: {value!r}"
        )
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def main(argv: list[str]) -> int:
    """Write rendered TOML to argv[1] (or stdout if absent)."""
    try:
        rendered = build_config(os.environ)
    except ConfigError as e:
        print(f"gen_config: {e}", file=sys.stderr)
        return 2

    if len(argv) >= 2 and argv[1] != "-":
        with open(argv[1], "w", encoding="utf-8") as fh:
            fh.write(rendered)
    else:
        sys.stdout.write(rendered)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
