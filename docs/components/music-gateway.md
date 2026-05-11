# music-gateway

**Path:** `crates/music-gateway/`
**Type:** binary (with library crate for tests)
**Test count:** 315 (largest in the workspace)

The HTTP gateway. Loads config, wires the axum router, terminates
TLS, runs OAuth, layers caches in front of Navidrome, hosts the
recommender + sync + event log.

This is where most ongoing code changes land.

## Layout

```
crates/music-gateway/
├── src/
│   ├── main.rs                  # binary entrypoint: TLS, boot wiring
│   ├── lib.rs                   # library entrypoint: re-exports for tests
│   ├── app.rs                   # build_router() — single source of truth for routes
│   ├── config.rs                # TOML schema + Config::load
│   ├── state.rs                 # AppState (Arc-backed, cloned per-request)
│   ├── auth.rs                  # require_bearer middleware
│   ├── proxy.rs                 # /rest/* → Navidrome; L2 + cover-art proxy
│   ├── recommend.rs             # /v1/recommend/next, from-any, from-seeds, enqueue
│   ├── recommend_feedback.rs    # POST /v1/recommend/feedback (thumbs up/down)
│   ├── scrobble.rs              # /rest/scrobble interceptor → play_history + event log
│   ├── events.rs                # POST /v1/events
│   ├── embedder.rs              # EmbedderHandle + degraded-mode boot probe
│   ├── ingest.rs                # background ingest worker glue
│   ├── diagnostics/             # trace ring, RUM, /v1/diagnostics/* handlers
│   ├── oauth/                   # OAuth 2.1 server
│   ├── sync/                    # sync transport (HTTP snapshot + WS fan-out)
│   └── bin/
│       ├── backfill_genre.rs    # one-shot genre/year backfill into MetadataStore
│       └── recommend_bench.rs   # offline quality harness (rebuild + score)
├── tests/                       # ≈27 integration files
└── Cargo.toml
```

The integration tests cover OAuth, sync (REST + WS), recommend,
events, proxy, cover-art, cache layer, embedder probe, diagnostics,
client_events, scrobble, and the ingest fetcher.

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

`AppState` is `Arc<Inner>` and trivially `Clone`. axum hands it to
every handler cheaply. The current fields (see `state.rs`):

| Field | Purpose |
|---|---|
| `config` | Loaded `gateway.toml`. |
| `http` | Process-wide `reqwest::Client` (HTTP/2 connection pooling). |
| `cache` | L2 metadata cache. |
| `oauth` | OAuth 2.1 state pool. |
| `setup_token` | One-time first-run token (gated by absence of master password). |
| `sync` | In-memory `SyncStore` — queue + playback + likes. |
| `embedder` | `EmbedderHandle`. Either an HTTP client to the sidecar or a "disabled" sentinel. |
| `embedding_store` | `track_embeddings` table + ingest queue. |
| `metadata_store` | `track_metadata` cache (filter inputs). |
| `event_store` | Append-only event log. |
| `play_history` | MMR recency clock. |
| `feedback` | Per-session thumbs-up/down store. |
| `projection` | UMAP 2D projection store. |
| `ann` | `usearch` HNSW index (Arc — shared with the ingest worker). |
| `recommend_model_version` | Stamp for new embeddings + ANN queries. Sourced from the embedder's last health probe. |
| `trace_store` | M0 diagnostics ring (read-only view; the drainer task is the sole writer). |
| `placeholder_etags` | In-memory dedup of Navidrome's "no artwork" placeholder etags. Rebuilt on restart. |
| `placeholder_revalidations` | Cooldown map for the cover-art self-heal background refetch. |

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

`type=random` on `getAlbumList2` bypasses the cache (it's supposed to
shuffle every time).

### Cover-art proxy

`getCoverArt` is handled in the same file but with its own logic:

- Cache hits are served with `max-age=300, must-revalidate`.
- Placeholder bodies (our SVG or Navidrome's "no artwork" default —
  detected by etag dedup across distinct cover-art ids) are served
  with `no-cache, must-revalidate`. This is the fix for the "G logo
  is stuck" bug class: once a real cover lands, the browser
  revalidates instead of serving the placeholder from disk cache.
- A placeholder cache hit kicks off a coalesced background refetch
  (one per key per cooldown window) so missing art self-heals.

## Intercepted Subsonic endpoints

`/rest/scrobble` is registered as a specific route ahead of the
catch-all `/rest/*subsonic_path`. axum's matchit prefers the more
specific path, so this wins regardless of registration order. The
handler:

1. Writes `play_history.last_played_ms` (recency clock for MMR).
2. Appends a `Scrobble` event to the event log.
3. Forwards the unmodified request to the proxy — Navidrome remains
   the canonical play-count ledger.

Both writes are best-effort: a failure is logged and never blocks the
forwarded request.

## Recommend / events / sync handlers

See per-component docs:
- [music-recommend](./music-recommend.md) — `recommend.rs`,
  `recommend_feedback.rs`. Endpoint surface in [API.md](../API.md#recommender).
- [music-sync](./music-sync.md) — endpoints in `sync/handlers.rs` and
  `sync/ws.rs`.
- Events handler in `events.rs` is small: validate input, batch, call
  `EventStore::append_batch`.

## Diagnostics

`diagnostics/` contains four pieces:

- `store.rs` — sqlx pool against `gateway-state.traces.sqlite`, plus
  the ring-trim policy.
- `layer.rs` — a `tracing` subscriber layer that drains closed spans
  into the store (batched).
- `handlers.rs` — every `/v1/diagnostics/*` HTTP handler.
- `types.rs` — wire shapes for trace records, histogram buckets,
  client events.

The trace ring is M0 of the diagnostics roadmap; the per-feature
recommend dashboards (`queue_fill`, `shortfall`, `similarity`,
`top_results`, `feedback`) are M2.2; browser RUM is M3. The
`client_events` table currently has no automatic trim — see the
memory note `project_m3_client_events_followups`.

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

## Bins (one-shot tooling)

- `backfill_genre` — reads `track_metadata` rows missing `genre` /
  `year`, refetches from Navidrome, upserts. Run after the schema
  added those columns.
- `recommend_bench` — offline quality harness. Loads a fixed seed
  list, runs the recommender at several configs, reports per-config
  metrics. Useful for "did my filter change regress?"

## Known gaps

- **No transcoding.** Streaming is forwarded raw from Navidrome.
  When clients request lower bitrates, we'll add the L4 transcoded
  cache.
- **No rate limiting.** Single-user, local network — not yet
  needed.
- **CLI uses static bearer**. Will move to OAuth Device Authorization
  Grant (RFC 8628) at P4.
- **No Prometheus `/metrics`.** The M0 trace ring covers the same
  ground for now and the `/diagnostics` page is enough for a single
  operator; we'll add `/metrics` if external scraping ever matters.
- **No `client_events` ring-trim.** The table grows; see
  `docs/components/music-gateway.md` follow-ups via memory.
