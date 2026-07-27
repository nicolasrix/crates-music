"""Tests for the gateway config templater.

`gen_config.build_config(env)` is a pure function: env-var dict in,
TOML string out. The entrypoint shell script is a thin wrapper around
this — keeping the logic in Python means we can test every
branch without spinning up a container.

Contract under test:
    - Required env vars: NAVIDROME_URL, NAVIDROME_USERNAME,
      NAVIDROME_PASSWORD, GATEWAY_BEARER_TOKEN.
    - Optional env vars fall back to documented defaults.
    - The output is always valid TOML and parses back to the schema
      shape the gateway expects (server, upstream, cache, oauth,
      optional embedder).
    - Embedder section is only emitted when EMBEDDER_URL is set —
      absence means degraded mode by design.
"""

import tomllib

import pytest

from gen_config import ConfigError, build_config


def _minimum_env() -> dict[str, str]:
    """The least env needed for a successful render."""
    return {
        "NAVIDROME_URL": "http://nav.lan:4533",
        "NAVIDROME_USERNAME": "alice",
        "NAVIDROME_PASSWORD": "wonderland",
        "GATEWAY_BEARER_TOKEN": "a" * 64,
    }


def test_minimum_env_produces_valid_toml() -> None:
    toml = build_config(_minimum_env())
    parsed = tomllib.loads(toml)

    assert parsed["server"]["listen"] == "0.0.0.0:8443"
    assert parsed["server"]["tls_cert"] == "/data/certs/gateway.local.pem"
    assert parsed["server"]["tls_key"] == "/data/certs/gateway.local-key.pem"
    assert parsed["server"]["bearer_token"] == "a" * 64

    assert parsed["upstream"]["navidrome_url"] == "http://nav.lan:4533"
    assert parsed["upstream"]["username"] == "alice"
    assert parsed["upstream"]["password"] == "wonderland"

    assert parsed["cache"]["path"] == "/data/state/gateway-cache.sqlite"
    assert parsed["cache"]["browse_ttl_seconds"] == 86400
    assert parsed["cache"]["list_ttl_seconds"] == 60

    assert parsed["oauth"]["state_db"] == "/data/state/gateway-state.sqlite"
    # Default web + cli clients always registered.
    clients = parsed["oauth"]["clients"]
    assert len(clients) == 2
    assert clients[0]["client_id"] == "web"
    assert clients[0]["redirect_uris"] == [
        "https://gateway.local:8443/oauth/callback"
    ]
    # CLI client (Device Authorization Grant, RFC 8628) — no redirect URIs.
    assert clients[1]["client_id"] == "cli"
    assert clients[1]["redirect_uris"] == []


def test_embedder_section_omitted_when_url_unset() -> None:
    parsed = tomllib.loads(build_config(_minimum_env()))
    assert "embedder" not in parsed


def test_embedder_section_included_when_url_set() -> None:
    env = _minimum_env() | {"EMBEDDER_URL": "http://embedder:9000"}
    parsed = tomllib.loads(build_config(env))
    assert parsed["embedder"]["url"] == "http://embedder:9000"
    assert parsed["embedder"]["timeout_seconds"] == 30


def test_embedder_timeout_override() -> None:
    env = _minimum_env() | {
        "EMBEDDER_URL": "http://embedder:9000",
        "EMBEDDER_TIMEOUT_SECONDS": "120",
    }
    parsed = tomllib.loads(build_config(env))
    assert parsed["embedder"]["timeout_seconds"] == 120


def test_embedder_bearer_token_omitted_when_unset() -> None:
    env = _minimum_env() | {"EMBEDDER_URL": "http://embedder:9000"}
    parsed = tomllib.loads(build_config(env))
    assert "bearer_token" not in parsed["embedder"]


def test_embedder_bearer_token_emitted_when_set() -> None:
    env = _minimum_env() | {
        "EMBEDDER_URL": "http://gpu-box.lan:9000",
        "EMBEDDER_BEARER_TOKEN": "shared-secret",
    }
    parsed = tomllib.loads(build_config(env))
    assert parsed["embedder"]["bearer_token"] == "shared-secret"


def test_embedder_bearer_token_empty_treated_as_unset() -> None:
    # Empty env var is a common deploy mistake. Matches the embedder
    # side, which also disables enforcement on empty.
    env = _minimum_env() | {
        "EMBEDDER_URL": "http://embedder:9000",
        "EMBEDDER_BEARER_TOKEN": "",
    }
    parsed = tomllib.loads(build_config(env))
    assert "bearer_token" not in parsed["embedder"]


def test_embedder_fallback_urls_omitted_when_unset() -> None:
    env = _minimum_env() | {"EMBEDDER_URL": "http://embedder:9000"}
    parsed = tomllib.loads(build_config(env))
    assert "fallback_urls" not in parsed["embedder"]
    # probe_interval not emitted unless explicitly overridden (gateway
    # defaults it) — keeps existing deploys byte-identical.
    assert "probe_interval_seconds" not in parsed["embedder"]


def test_embedder_fallback_urls_parsed_in_order() -> None:
    env = _minimum_env() | {
        "EMBEDDER_URL": "http://gpu-box.lan:9000",
        "EMBEDDER_FALLBACK_URLS": (
            "http://embedder-fallback:9000, http://spare.lan:9000"
        ),
    }
    parsed = tomllib.loads(build_config(env))
    assert parsed["embedder"]["fallback_urls"] == [
        "http://embedder-fallback:9000",
        "http://spare.lan:9000",
    ]


def test_embedder_fallback_urls_drops_blank_entries() -> None:
    env = _minimum_env() | {
        "EMBEDDER_URL": "http://gpu-box.lan:9000",
        "EMBEDDER_FALLBACK_URLS": "http://a:9000,, ,http://b:9000",
    }
    parsed = tomllib.loads(build_config(env))
    assert parsed["embedder"]["fallback_urls"] == [
        "http://a:9000",
        "http://b:9000",
    ]


def test_embedder_probe_interval_override() -> None:
    env = _minimum_env() | {
        "EMBEDDER_URL": "http://embedder:9000",
        "EMBEDDER_PROBE_INTERVAL_SECONDS": "10",
    }
    parsed = tomllib.loads(build_config(env))
    assert parsed["embedder"]["probe_interval_seconds"] == 10


def test_embedder_probe_interval_rejects_non_integer() -> None:
    env = _minimum_env() | {
        "EMBEDDER_URL": "http://embedder:9000",
        "EMBEDDER_PROBE_INTERVAL_SECONDS": "soon",
    }
    with pytest.raises(ConfigError):
        build_config(env)


def test_embedder_fallback_urls_ignored_when_url_unset() -> None:
    env = _minimum_env() | {
        "EMBEDDER_FALLBACK_URLS": "http://embedder-fallback:9000"
    }
    parsed = tomllib.loads(build_config(env))
    assert "embedder" not in parsed


def test_embedder_bearer_token_ignored_when_url_unset() -> None:
    # A token without a URL is meaningless — no [embedder] section at all.
    env = _minimum_env() | {"EMBEDDER_BEARER_TOKEN": "stray"}
    parsed = tomllib.loads(build_config(env))
    assert "embedder" not in parsed


def test_oauth_redirect_uris_comma_separated() -> None:
    env = _minimum_env() | {
        "OAUTH_WEB_REDIRECT_URIS": (
            "http://localhost:5173/oauth/callback,"
            "https://music.example.com/oauth/callback"
        )
    }
    parsed = tomllib.loads(build_config(env))
    uris = parsed["oauth"]["clients"][0]["redirect_uris"]
    assert uris == [
        "http://localhost:5173/oauth/callback",
        "https://music.example.com/oauth/callback",
    ]


def test_oauth_redirect_uris_whitespace_tolerant() -> None:
    """Users will paste with spaces — strip them rather than rejecting."""
    env = _minimum_env() | {
        "OAUTH_WEB_REDIRECT_URIS": (
            "  http://a/cb  ,\thttps://b/cb ,https://c/cb\n"
        )
    }
    parsed = tomllib.loads(build_config(env))
    uris = parsed["oauth"]["clients"][0]["redirect_uris"]
    assert uris == ["http://a/cb", "https://b/cb", "https://c/cb"]


def test_browse_ttl_seconds_override() -> None:
    env = _minimum_env() | {"GATEWAY_BROWSE_TTL_SECONDS": "3600"}
    parsed = tomllib.loads(build_config(env))
    assert parsed["cache"]["browse_ttl_seconds"] == 3600


def test_list_ttl_seconds_override() -> None:
    env = _minimum_env() | {"GATEWAY_LIST_TTL_SECONDS": "300"}
    parsed = tomllib.loads(build_config(env))
    assert parsed["cache"]["list_ttl_seconds"] == 300


def test_listen_override() -> None:
    env = _minimum_env() | {"GATEWAY_LISTEN": "127.0.0.1:9000"}
    parsed = tomllib.loads(build_config(env))
    assert parsed["server"]["listen"] == "127.0.0.1:9000"


def test_tls_path_overrides() -> None:
    env = _minimum_env() | {
        "GATEWAY_TLS_CERT": "/etc/ssl/cert.pem",
        "GATEWAY_TLS_KEY": "/etc/ssl/key.pem",
    }
    parsed = tomllib.loads(build_config(env))
    assert parsed["server"]["tls_cert"] == "/etc/ssl/cert.pem"
    assert parsed["server"]["tls_key"] == "/etc/ssl/key.pem"


@pytest.mark.parametrize(
    "missing",
    [
        "NAVIDROME_URL",
        "NAVIDROME_USERNAME",
        "NAVIDROME_PASSWORD",
        "GATEWAY_BEARER_TOKEN",
    ],
)
def test_missing_required_env_raises(missing: str) -> None:
    env = _minimum_env()
    del env[missing]
    with pytest.raises(ConfigError) as excinfo:
        build_config(env)
    # Error message names the missing variable so the operator can fix it.
    assert missing in str(excinfo.value)


def test_empty_required_value_raises() -> None:
    """Empty strings are not valid for required fields — clearer than
    rendering a TOML with `password = ""` and watching the gateway
    fail to authenticate against Navidrome at runtime."""
    env = _minimum_env() | {"NAVIDROME_PASSWORD": ""}
    with pytest.raises(ConfigError):
        build_config(env)


def test_invalid_timeout_seconds_raises() -> None:
    """Non-integer EMBEDDER_TIMEOUT_SECONDS must fail fast rather
    than render garbage into the TOML."""
    env = _minimum_env() | {
        "EMBEDDER_URL": "http://embedder:9000",
        "EMBEDDER_TIMEOUT_SECONDS": "thirty",
    }
    with pytest.raises(ConfigError):
        build_config(env)


def test_output_round_trips_through_tomllib() -> None:
    """If the renderer ever emits malformed TOML, fail loudly here
    rather than at gateway boot."""
    env = _minimum_env() | {
        "EMBEDDER_URL": "http://embedder:9000",
        "OAUTH_WEB_REDIRECT_URIS": "https://a/cb,https://b/cb",
    }
    toml = build_config(env)
    # Should not raise. Parse, re-render is *not* required; we only need
    # the renderer's output to be parseable.
    tomllib.loads(toml)


def test_static_dir_omitted_when_unset() -> None:
    """No env var → no `static_dir` field. Gateway falls through to
    the legacy 404 behaviour, which is what dev-without-Vite wants."""
    parsed = tomllib.loads(build_config(_minimum_env()))
    assert "static_dir" not in parsed["server"]


def test_static_dir_emitted_when_set() -> None:
    env = _minimum_env() | {"WEB_STATIC_DIR": "/etc/gateway/web/dist"}
    parsed = tomllib.loads(build_config(env))
    assert parsed["server"]["static_dir"] == "/etc/gateway/web/dist"


def test_state_db_override() -> None:
    env = _minimum_env() | {
        "GATEWAY_STATE_DB": "/var/lib/crates-music/state.sqlite",
        "GATEWAY_CACHE_DB": "/var/lib/crates-music/cache.sqlite",
    }
    parsed = tomllib.loads(build_config(env))
    assert parsed["oauth"]["state_db"] == "/var/lib/crates-music/state.sqlite"
    assert parsed["cache"]["path"] == "/var/lib/crates-music/cache.sqlite"


# --- [recommend] section --------------------------------------------------
#
# Only emitted when RECOMMEND_EMBEDDING_DIM is set. Existing CLAP/stub
# deploys leave it unset and the gateway applies its 512 default; the
# CLaMP 3 variant sets 768. The dim must match the embedder's /healthz
# dim or the gateway's ANN open fails loudly at boot.


def test_recommend_section_omitted_when_unset() -> None:
    parsed = tomllib.loads(build_config(_minimum_env()))
    assert "recommend" not in parsed


def test_recommend_embedding_dim_emitted_when_set() -> None:
    env = _minimum_env() | {"RECOMMEND_EMBEDDING_DIM": "768"}
    parsed = tomllib.loads(build_config(env))
    assert parsed["recommend"]["embedding_dim"] == 768


def test_recommend_embedding_dim_empty_treated_as_unset() -> None:
    env = _minimum_env() | {"RECOMMEND_EMBEDDING_DIM": ""}
    parsed = tomllib.loads(build_config(env))
    assert "recommend" not in parsed


def test_recommend_embedding_dim_rejects_non_integer() -> None:
    env = _minimum_env() | {"RECOMMEND_EMBEDDING_DIM": "768.0"}
    with pytest.raises(ConfigError, match="RECOMMEND_EMBEDDING_DIM"):
        build_config(env)


def test_recommend_embedding_dim_rejects_non_positive() -> None:
    env = _minimum_env() | {"RECOMMEND_EMBEDDING_DIM": "0"}
    with pytest.raises(ConfigError, match="positive"):
        build_config(env)


# --- [recommend] preference knobs -----------------------------------------
#
# Optional and independent of the dim. Each field is omitted when its env
# var is unset (gateway applies its own default: preference_enabled=false,
# weight=0.15, half_life=30 days). The section appears if ANY key is set.


def test_preference_section_emitted_without_embedding_dim() -> None:
    env = _minimum_env() | {"RECOMMEND_PREFERENCE_ENABLED": "true"}
    parsed = tomllib.loads(build_config(env))
    assert parsed["recommend"]["preference_enabled"] is True
    assert "embedding_dim" not in parsed["recommend"]


def test_preference_enabled_false_is_emitted() -> None:
    env = _minimum_env() | {"RECOMMEND_PREFERENCE_ENABLED": "false"}
    parsed = tomllib.loads(build_config(env))
    assert parsed["recommend"]["preference_enabled"] is False


def test_preference_knobs_emitted_together() -> None:
    env = _minimum_env() | {
        "RECOMMEND_EMBEDDING_DIM": "768",
        "RECOMMEND_PREFERENCE_ENABLED": "1",
        "RECOMMEND_PREFERENCE_WEIGHT": "0.25",
        "RECOMMEND_AFFINITY_HALF_LIFE_DAYS": "45",
    }
    parsed = tomllib.loads(build_config(env))
    rec = parsed["recommend"]
    assert rec["embedding_dim"] == 768
    assert rec["preference_enabled"] is True
    assert rec["preference_weight"] == pytest.approx(0.25)
    assert rec["affinity_half_life_days"] == pytest.approx(45.0)


def test_preference_knobs_omitted_when_unset() -> None:
    env = _minimum_env() | {"RECOMMEND_EMBEDDING_DIM": "768"}
    parsed = tomllib.loads(build_config(env))
    assert "preference_enabled" not in parsed["recommend"]
    assert "preference_weight" not in parsed["recommend"]


def test_preference_enabled_rejects_non_boolean() -> None:
    env = _minimum_env() | {"RECOMMEND_PREFERENCE_ENABLED": "maybe"}
    with pytest.raises(ConfigError, match="RECOMMEND_PREFERENCE_ENABLED"):
        build_config(env)


def test_preference_weight_rejects_negative() -> None:
    env = _minimum_env() | {"RECOMMEND_PREFERENCE_WEIGHT": "-0.1"}
    with pytest.raises(ConfigError, match="non-negative"):
        build_config(env)


def test_affinity_half_life_rejects_non_positive() -> None:
    env = _minimum_env() | {"RECOMMEND_AFFINITY_HALF_LIFE_DAYS": "0"}
    with pytest.raises(ConfigError, match="positive"):
        build_config(env)


def test_affinity_half_life_rejects_non_numeric() -> None:
    env = _minimum_env() | {"RECOMMEND_AFFINITY_HALF_LIFE_DAYS": "soon"}
    with pytest.raises(ConfigError, match="must be a number"):
        build_config(env)


def test_like_bonus_emitted_when_set() -> None:
    env = _minimum_env() | {"RECOMMEND_LIKE_BONUS": "0.3"}
    parsed = tomllib.loads(build_config(env))
    assert parsed["recommend"]["like_bonus"] == pytest.approx(0.3)


def test_like_bonus_omitted_when_unset() -> None:
    env = _minimum_env() | {"RECOMMEND_EMBEDDING_DIM": "768"}
    parsed = tomllib.loads(build_config(env))
    assert "like_bonus" not in parsed["recommend"]


def test_like_bonus_rejects_negative() -> None:
    env = _minimum_env() | {"RECOMMEND_LIKE_BONUS": "-0.5"}
    with pytest.raises(ConfigError, match="non-negative"):
        build_config(env)


def test_like_bonus_album_and_artist_emitted_when_set() -> None:
    env = _minimum_env() | {
        "RECOMMEND_LIKE_BONUS_ALBUM": "0.06",
        "RECOMMEND_LIKE_BONUS_ARTIST": "0.03",
    }
    parsed = tomllib.loads(build_config(env))
    assert parsed["recommend"]["like_bonus_album"] == pytest.approx(0.06)
    assert parsed["recommend"]["like_bonus_artist"] == pytest.approx(0.03)


def test_like_bonus_album_and_artist_omitted_when_unset() -> None:
    env = _minimum_env() | {"RECOMMEND_EMBEDDING_DIM": "768"}
    parsed = tomllib.loads(build_config(env))
    assert "like_bonus_album" not in parsed["recommend"]
    assert "like_bonus_artist" not in parsed["recommend"]


def test_like_bonus_album_rejects_negative() -> None:
    env = _minimum_env() | {"RECOMMEND_LIKE_BONUS_ALBUM": "-0.1"}
    with pytest.raises(ConfigError, match="non-negative"):
        build_config(env)


def test_like_bonus_artist_rejects_negative() -> None:
    env = _minimum_env() | {"RECOMMEND_LIKE_BONUS_ARTIST": "-0.1"}
    with pytest.raises(ConfigError, match="non-negative"):
        build_config(env)


def test_leash_knobs_emitted_when_set() -> None:
    env = _minimum_env() | {
        "RECOMMEND_LEASH_TAU": "0.3",
        "RECOMMEND_LEASH_LAMBDA": "20",
    }
    parsed = tomllib.loads(build_config(env))
    assert parsed["recommend"]["leash_tau"] == pytest.approx(0.3)
    assert parsed["recommend"]["leash_lambda"] == pytest.approx(20)


def test_leash_knobs_omitted_when_unset() -> None:
    env = _minimum_env() | {"RECOMMEND_EMBEDDING_DIM": "768"}
    parsed = tomllib.loads(build_config(env))
    assert "leash_tau" not in parsed["recommend"]
    assert "leash_lambda" not in parsed["recommend"]


def test_leash_lambda_zero_is_emitted() -> None:
    # 0 is a meaningful value (disables the leash), not "unset" — it must
    # survive into the config rather than being dropped.
    env = _minimum_env() | {"RECOMMEND_LEASH_LAMBDA": "0"}
    parsed = tomllib.loads(build_config(env))
    assert parsed["recommend"]["leash_lambda"] == pytest.approx(0)


def test_leash_tau_rejects_negative() -> None:
    env = _minimum_env() | {"RECOMMEND_LEASH_TAU": "-0.1"}
    with pytest.raises(ConfigError, match="non-negative"):
        build_config(env)


def test_log_provenance_true_is_emitted() -> None:
    env = _minimum_env() | {"RECOMMEND_LOG_PROVENANCE": "true"}
    parsed = tomllib.loads(build_config(env))
    assert parsed["recommend"]["log_provenance"] is True


def test_log_provenance_false_is_emitted() -> None:
    # false is meaningful (turn capture off) — must survive, not be dropped.
    env = _minimum_env() | {"RECOMMEND_LOG_PROVENANCE": "false"}
    parsed = tomllib.loads(build_config(env))
    assert parsed["recommend"]["log_provenance"] is False


def test_log_provenance_omitted_when_unset() -> None:
    env = _minimum_env() | {"RECOMMEND_EMBEDDING_DIM": "768"}
    parsed = tomllib.loads(build_config(env))
    assert "log_provenance" not in parsed["recommend"]


def test_log_provenance_rejects_non_boolean() -> None:
    env = _minimum_env() | {"RECOMMEND_LOG_PROVENANCE": "yes-please"}
    with pytest.raises(ConfigError):
        build_config(env)


# --- autoplay anti-repetition + exploration knobs -------------------------


def test_autoplay_knobs_emitted_when_set() -> None:
    env = _minimum_env() | {
        "RECOMMEND_RECENTLY_PLAYED_EXCLUDE_HOURS": "6",
        "RECOMMEND_SERVED_COOLDOWN_HOURS": "3",
        "RECOMMEND_EXPLORE_TEMPERATURE": "0.2",
    }
    rec = tomllib.loads(build_config(env))["recommend"]
    assert rec["recently_played_exclude_hours"] == pytest.approx(6.0)
    assert rec["served_cooldown_hours"] == pytest.approx(3.0)
    assert rec["explore_temperature"] == pytest.approx(0.2)


def test_autoplay_zero_values_are_emitted() -> None:
    # 0 is a meaningful "disable this knob" value, not "unset" — must survive.
    env = _minimum_env() | {
        "RECOMMEND_SERVED_COOLDOWN_HOURS": "0",
        "RECOMMEND_EXPLORE_TEMPERATURE": "0",
    }
    rec = tomllib.loads(build_config(env))["recommend"]
    assert rec["served_cooldown_hours"] == pytest.approx(0)
    assert rec["explore_temperature"] == pytest.approx(0)


def test_autoplay_knobs_omitted_when_unset() -> None:
    env = _minimum_env() | {"RECOMMEND_EMBEDDING_DIM": "768"}
    rec = tomllib.loads(build_config(env))["recommend"]
    assert "recently_played_exclude_hours" not in rec
    assert "served_cooldown_hours" not in rec
    assert "explore_temperature" not in rec


def test_autoplay_rejects_negative() -> None:
    env = _minimum_env() | {"RECOMMEND_EXPLORE_TEMPERATURE": "-0.1"}
    with pytest.raises(ConfigError, match="non-negative"):
        build_config(env)
