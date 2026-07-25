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
    EMBEDDER_FALLBACK_URLS      (comma-separated failover endpoints, tried
                                 in order after EMBEDDER_URL; only used if
                                 EMBEDDER_URL set)
    EMBEDDER_PROBE_INTERVAL_SECONDS  (unset → gateway default 20; background
                                 re-probe cadence for failover)
    EMBEDDER_TIMEOUT_SECONDS    30 (only used if EMBEDDER_URL set)
    EMBEDDER_BEARER_TOKEN       (unset/empty → no bearer_token field;
                                 only used if EMBEDDER_URL set)
    RECOMMEND_EMBEDDING_DIM     (unset → gateway defaults to 512 for CLAP;
                                 set to 768 for the CLaMP 3 embedder)
    RECOMMEND_PREFERENCE_ENABLED        (bool; gateway default false — turns
                                         on per-track feedback re-scoring)
    RECOMMEND_PREFERENCE_WEIGHT         (float >= 0; gateway default 0.15)
    RECOMMEND_AFFINITY_HALF_LIFE_DAYS   (float > 0; gateway default 30)
    RECOMMEND_LIKE_BONUS                (float >= 0; gateway default 0.15 —
                                         always-on liked-track boost)
    RECOMMEND_LIKE_BONUS_ALBUM          (float >= 0; gateway default 0.06 —
                                         liked-album boost)
    RECOMMEND_LIKE_BONUS_ARTIST         (float >= 0; gateway default 0.03 —
                                         liked-artist boost)
    RECOMMEND_LEASH_TAU                 (float >= 0; gateway default 0.28 —
                                         anchor-leash radius for travelling
                                         stations)
    RECOMMEND_LEASH_LAMBDA              (float >= 0; gateway default 16 —
                                         anchor-leash strength; 0 disables)
    RECOMMEND_LOG_PROVENANCE            (bool; gateway default true — capture
                                         served recommendations as model
                                         training data; false to disable)
    RECOMMEND_RECENTLY_PLAYED_EXCLUDE_HOURS
                                        (float >= 0; gateway default 4 —
                                         autoplay: exclude recently-played
                                         tracks from from-seeds; 0 disables)
    RECOMMEND_SERVED_COOLDOWN_HOURS     (float >= 0; gateway default 2 —
                                         autoplay: cooldown on recently-served
                                         tracks; 0 disables)
    RECOMMEND_EXPLORE_TEMPERATURE       (float >= 0; gateway default 0.15 —
                                         autoplay: Gumbel sampling of the final
                                         pick; 0 → deterministic argmax)

The [recommend] section is emitted when any RECOMMEND_* key above is set;
each field is omitted individually when its env var is unset.
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
    fixed section order (server, upstream, cache, oauth, embedder,
    recommend).
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
    embedder_bearer: str | None = None
    embedder_fallbacks: list[str] = []
    embedder_probe_interval: int | None = None
    if embedder_url:
        # Empty string treated as "unset" — matches the embedder side
        # so a misconfigured deploy fails closed, not silently open.
        token = env.get("EMBEDDER_BEARER_TOKEN", "").strip()
        embedder_bearer = token or None
        try:
            embedder_timeout = int(
                env.get("EMBEDDER_TIMEOUT_SECONDS", DEFAULT_EMBEDDER_TIMEOUT)
            )
        except ValueError as e:
            raise ConfigError(
                f"EMBEDDER_TIMEOUT_SECONDS must be an integer: {e}"
            ) from e
        # Comma-separated failover endpoints, tried in order after the
        # primary. Typically a CPU fallback sidecar (e.g.
        # http://embedder-fallback:9000) that a watchdog brings up when the
        # GPU box is down. All must serve the same model_version + dim.
        fallback_raw = env.get("EMBEDDER_FALLBACK_URLS", "")
        embedder_fallbacks = [u.strip() for u in fallback_raw.split(",") if u.strip()]
        # Background re-probe cadence. Only emitted when explicitly set, so
        # existing deploys render byte-identical (the gateway defaults to 20
        # and re-probes regardless).
        interval_raw = env.get("EMBEDDER_PROBE_INTERVAL_SECONDS", "").strip()
        if interval_raw:
            try:
                embedder_probe_interval = int(interval_raw)
            except ValueError as e:
                raise ConfigError(
                    f"EMBEDDER_PROBE_INTERVAL_SECONDS must be an integer: {e}"
                ) from e

    # Optional [recommend] section. Only emitted when explicitly set, so
    # existing CLAP/stub deploys render byte-identical config (the gateway
    # then applies its 512 default). Set to 768 for the CLaMP 3 embedder.
    embedding_dim_raw = env.get("RECOMMEND_EMBEDDING_DIM", "").strip()
    embedding_dim: int | None = None
    if embedding_dim_raw:
        try:
            embedding_dim = int(embedding_dim_raw)
        except ValueError as e:
            raise ConfigError(
                f"RECOMMEND_EMBEDDING_DIM must be an integer: {e}"
            ) from e
        if embedding_dim <= 0:
            raise ConfigError(
                f"RECOMMEND_EMBEDDING_DIM must be positive, got {embedding_dim}"
            )

    # Preference-affinity knobs (recommend re-scoring by per-track feedback).
    # All optional and independent of the dim; an unset key is omitted so the
    # gateway applies its own default. preference_enabled defaults to FALSE
    # gateway-side, so the feature stays dark until explicitly switched on
    # here — affinity is captured regardless, only the read is gated.
    preference_enabled = _parse_bool(env, "RECOMMEND_PREFERENCE_ENABLED")
    preference_weight = _parse_float(
        env, "RECOMMEND_PREFERENCE_WEIGHT", non_negative=True
    )
    affinity_half_life = _parse_float(
        env, "RECOMMEND_AFFINITY_HALF_LIFE_DAYS", positive=True
    )
    # Durable-like boost. Always-on server-side (independent of
    # preference_enabled), so these knobs just tune the magnitude; an unset
    # key omits the field and the gateway applies its own default. The three
    # tiers follow the contribution hierarchy track > album > artist
    # (gateway defaults 0.15 / 0.06 / 0.03).
    like_bonus = _parse_float(env, "RECOMMEND_LIKE_BONUS", non_negative=True)
    like_bonus_album = _parse_float(
        env, "RECOMMEND_LIKE_BONUS_ALBUM", non_negative=True
    )
    like_bonus_artist = _parse_float(
        env, "RECOMMEND_LIKE_BONUS_ARTIST", non_negative=True
    )
    # Anchor-leash defaults for travelling autoplay stations. The web client
    # sends per-request overrides from its Settings page; these only set the
    # server fallback for clients that don't (CLI, old web builds). Unset →
    # gateway defaults (tau 0.28 / lambda 16). lambda 0 disables the leash.
    leash_tau = _parse_float(env, "RECOMMEND_LEASH_TAU", non_negative=True)
    leash_lambda = _parse_float(env, "RECOMMEND_LEASH_LAMBDA", non_negative=True)
    # Recommendation provenance logging (gateway default TRUE). Pure
    # append-only capture of what was served — the training substrate for
    # future ranking models. Set false to stop capturing (e.g. to bound
    # disk on a long-running deploy).
    log_provenance = _parse_bool(env, "RECOMMEND_LOG_PROVENANCE")
    # Autoplay anti-repetition + exploration (gateway defaults 4h / 2h / 0.15).
    # recently_played_exclude_hours and served_cooldown_hours hard-exclude
    # recently-heard / recently-served tracks from from-seeds refills; 0
    # disables either. explore_temperature samples the final pick from
    # softmax(score/T) instead of a deterministic argmax; 0 → argmax.
    recently_played_exclude_hours = _parse_float(
        env, "RECOMMEND_RECENTLY_PLAYED_EXCLUDE_HOURS", non_negative=True
    )
    served_cooldown_hours = _parse_float(
        env, "RECOMMEND_SERVED_COOLDOWN_HOURS", non_negative=True
    )
    explore_temperature = _parse_float(
        env, "RECOMMEND_EXPLORE_TEMPERATURE", non_negative=True
    )

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

    # CLI client for the Device Authorization Grant (RFC 8628). No redirect
    # URIs — the device grant never uses one. Always registered so
    # `music auth login` works against the deployed gateway out of the box.
    parts.append("[[oauth.clients]]")
    parts.append('client_id = "cli"')
    parts.append('name = "CLI"')
    parts.append("redirect_uris = []")
    parts.append("")

    if embedder_url:
        parts.append("[embedder]")
        parts.append(f"url = {_str(embedder_url)}")
        if embedder_fallbacks:
            inner = ", ".join(_str(u) for u in embedder_fallbacks)
            parts.append(f"fallback_urls = [{inner}]")
        if embedder_probe_interval is not None:
            parts.append(f"probe_interval_seconds = {embedder_probe_interval}")
        parts.append(f"timeout_seconds = {embedder_timeout}")
        if embedder_bearer is not None:
            parts.append(f"bearer_token = {_str(embedder_bearer)}")
        parts.append("")

    recommend_lines: list[str] = []
    if embedding_dim is not None:
        recommend_lines.append(f"embedding_dim = {embedding_dim}")
    if preference_enabled is not None:
        recommend_lines.append(
            f"preference_enabled = {'true' if preference_enabled else 'false'}"
        )
    if preference_weight is not None:
        recommend_lines.append(f"preference_weight = {preference_weight}")
    if affinity_half_life is not None:
        recommend_lines.append(f"affinity_half_life_days = {affinity_half_life}")
    if like_bonus is not None:
        recommend_lines.append(f"like_bonus = {like_bonus}")
    if like_bonus_album is not None:
        recommend_lines.append(f"like_bonus_album = {like_bonus_album}")
    if like_bonus_artist is not None:
        recommend_lines.append(f"like_bonus_artist = {like_bonus_artist}")
    if leash_tau is not None:
        recommend_lines.append(f"leash_tau = {leash_tau}")
    if leash_lambda is not None:
        recommend_lines.append(f"leash_lambda = {leash_lambda}")
    if log_provenance is not None:
        recommend_lines.append(
            f"log_provenance = {'true' if log_provenance else 'false'}"
        )
    if recently_played_exclude_hours is not None:
        recommend_lines.append(
            f"recently_played_exclude_hours = {recently_played_exclude_hours}"
        )
    if served_cooldown_hours is not None:
        recommend_lines.append(f"served_cooldown_hours = {served_cooldown_hours}")
    if explore_temperature is not None:
        recommend_lines.append(f"explore_temperature = {explore_temperature}")
    if recommend_lines:
        parts.append("[recommend]")
        parts.extend(recommend_lines)
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


def _parse_bool(env: Mapping[str, str], key: str) -> bool | None:
    """Parse a boolean env var. Empty/unset → None (omit the field).
    Accepts true/false/1/0/yes/no (case-insensitive); anything else is a
    fail-fast ConfigError rather than a silent default."""
    raw = env.get(key, "").strip().lower()
    if raw == "":
        return None
    if raw in ("true", "1", "yes"):
        return True
    if raw in ("false", "0", "no"):
        return False
    raise ConfigError(
        f"{key} must be a boolean (true/false), got {env.get(key)!r}"
    )


def _parse_float(
    env: Mapping[str, str],
    key: str,
    *,
    positive: bool = False,
    non_negative: bool = False,
) -> float | None:
    """Parse a float env var. Empty/unset → None (omit the field).
    `positive` requires > 0; `non_negative` requires >= 0."""
    raw = env.get(key, "").strip()
    if raw == "":
        return None
    try:
        value = float(raw)
    except ValueError as e:
        raise ConfigError(f"{key} must be a number: {e}") from e
    if positive and value <= 0:
        raise ConfigError(f"{key} must be positive, got {value}")
    if non_negative and value < 0:
        raise ConfigError(f"{key} must be non-negative, got {value}")
    return value


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
