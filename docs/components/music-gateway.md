# music-gateway

**Path:** `crates/music-gateway/`
**Type:** binary (with library crate for tests)
**Test count:** 131 (largest in the workspace)

The HTTP gateway. Loads config, wires the axum router, terminates
TLS, runs OAuth, layers caches in front of Navidrome, hosts the
recommender + sync + event log.

This is where most ongoing code changes land.

## Layout

```
crates/music-gateway/
├── src/
│   ├── main.rs           # binary entrypoint: TLS termination, boot wiring
│   ├── lib.rs            # library entrypoint: re-exports for tests
│   ├── app.rs            # build_router() — single source of truth for routes
│   ├── config.rs         # TOML schema + Config::load
│   ├── state.rs          # AppState: shared, Arc-backed, cloned per-request
│   ├── auth.rs           # require_bearer middleware
│   ├── proxy.rs          # /rest/* → Navidrome with cache layered in
│   ├── recommend.rs      # /v1/recommend/* handlers
│   ├── events.rs         # POST /v1/events handler
│   ├── embedder.rs       # EmbedderHandle + degraded-mode boot probe
│   ├── oauth/            # OAuth 2.1 server (modules: handlers, storage, ...)
│   └── sync/             # sync transport (HTTP snapshot + WS fan-out)
├── tests/
│   ├── common/mod.rs     # shared test fixtures
│   ├── oauth_*.rs        # 5 files
│   ├── recommend.rs
│   ├── events.rs
│   ├── proxy.rs
│   ├── cache_layer.rs
│   └── …                 # 19 files total
└── Cargo.toml
```

## Boot path

`main.rs` does the wiring:

1. Install rustls' ring crypto provider.
2. Parse `--config` and load `gateway.toml`.
3. Load TLS certs (mkcert PEMs).
4. Open the L2 metadata cache.
5. Open the OAuth state DB. Register pre-declared clients
   (idempotent — skipped when already in DB).
6. Generate a one-time setup token if no master password is set.
7. Probe the embedder sidecar (or run in degraded mode if absent /
   unreachable).
8. Open the recommend DB. Reset stuck `in_progress` rows. Open the
   ANN. Rebuild from SQLite if empty.
9. Construct `AppState`, build the router, bind, serve.

The whole thing is in `main()` plus one helper (`boot_recommender`)
to keep `main` under the line cap.

## Why TLS lives in main, not app.rs

> "TLS is wired here, not in `app.rs`, so unit/integration tests can
> exercise the router over plain HTTP via `tower::ServiceExt::oneshot`."
> *— `crates/music-gateway/src/main.rs`*

Tests don't want to bind TCP, generate certs, or set up TLS. They
construct an `AppState` (with all in-memory backing stores), call
`build_router(state)`, and drive the resulting `Router` via
`oneshot`. This gets you a 1ms-per-test integration suite.

## AppState

```rust
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    config: Config,
    http: reqwest::Client,
    cache: Cache,
    oauth: OauthStore,
    setup_token: SetupToken,
    sync: SyncStore,
    embedder: EmbedderHandle,
    embedding_store: EmbeddingStore,
    event_store: EventStore,
    ann: Arc<AnnIndex>,
    recommend_model_version: ModelVersion,
}
```

Trivially `Clone` (just an Arc bump), so axum can hand it to every
handler cheaply. The `reqwest::Client` is here so HTTP/2 connection
pooling is per-process, not per-request.

## Auth middleware (`require_bearer`)

Applied to `/v1/*` and `/rest/*` via `from_fn_with_state`. Accepts:

1. The static bearer from `gateway.toml`.
2. Any active OAuth-issued access token (looked up by `sha256(token)`).

Token can come via header (`Authorization: Bearer <t>`) or query
param (`?access_token=<t>`). The query-param path is for `<audio>`
and `<img>` URLs that can't set headers — required for the web app's
playback element.

Failure modes:
- Missing token → 401 with `WWW-Authenticate: Bearer`.
- Invalid token → 401.
- Setup mode (no master password set) → 401 (with the setup URL in
  the gateway logs).

## OAuth 2.1 server (`oauth/`)

Hand-rolled. Single-tenant. The implementation is in `oauth/`:

- `handlers/` — endpoint handlers (`setup`, `login`, `authorize`,
  `token`, `revoke`).
- `storage.rs` — `OauthStore` wraps the SQLite pool, owns the table
  layout via migrations.
- `tokens.rs` — token generation (256-bit random, base64-url),
  storage as `sha256(token)` (so DB compromise doesn't reveal live
  tokens).
- `pkce.rs` — S256 code challenge verification.

Tables:

| Table | What |
|---|---|
| `users` | one row, master password hashed with Argon2id |
| `oauth_clients` | registered client_ids + redirect URIs |
| `auth_codes` | short-lived (60 s) authorization codes |
| `refresh_tokens` | long-lived, per-device, individually revocable |
| `access_tokens` | short-lived (1 h), looked up by sha256 |
| `sessions` | login sessions (cookie-based, separate from access tokens) |

Refresh tokens **rotate** on every refresh: the old one is invalidated
and a new one is issued in the same response. This bounds replay
damage.

## Pass-through proxy (`proxy.rs`)

`ANY /rest/*` is forwarded to Navidrome. The proxy:

1. Replaces the client's auth (Subsonic `t+s` or our bearer) with the
   gateway's upstream creds.
2. For cacheable endpoints (`getAlbumList2`, `getAlbum`), checks the
   L2 cache first. On hit, returns the cached body. On stale, sends
   `If-None-Match` upstream and revalidates.
3. Streams the response back to the client.

The proxy intentionally doesn't try to be smart about the response —
it forwards the JSON unchanged. Clients can use any Subsonic SDK.

## Recommend / events / sync handlers

See per-component docs:
- [music-recommend](./music-recommend.md) — endpoints in `recommend.rs`.
- [music-sync](./music-sync.md) — endpoints in `sync/handlers.rs` and
  `sync/ws.rs`.
- Events handler in `events.rs` is ~70 lines: validate input, batch,
  call `EventStore::append_batch`.

## Testing patterns

Every integration test in `tests/` follows this shape:

```rust
mod common;
use common::{TEST_BEARER, build_state, test_config};

#[tokio::test]
async fn it_does_a_thing() {
    let state = build_state(test_config()).await;
    let app = build_router(state.clone());

    let req = Request::builder()
        .method("POST")
        .uri("/v1/something")
        .header("authorization", format!("Bearer {TEST_BEARER}"))
        .body(Body::from(...))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
```

`common::build_state` constructs an in-memory cache, OAuth store,
embedding store, and ANN — so every test runs against a fresh state.

## Known gaps

- **No transcoding.** Streaming is forwarded raw from Navidrome.
  When clients request lower bitrates, we'll add the L4 transcoded
  cache.
- **No rate limiting.** Single-user, local network — not yet
  needed.
- **CLI uses static bearer**. Will move to OAuth Device Authorization
  Grant (RFC 8628) at P4.
- **No metrics endpoint.** No `/metrics` for Prometheus, no
  histogram of request durations. Easy to add via `tower-http::Metrics`
  when there's an actual operator who wants it.
