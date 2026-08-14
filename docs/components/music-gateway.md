# music-gateway

**Path:** `crates/music-gateway/`
**Type:** binary (with library crate for tests)
**Test count:** ≈350 (largest in the workspace)

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
│   ├── recommend.rs             # /v1/recommend/{next, from-any, from-seeds, station, enqueue}
│   ├── recommend_feedback.rs    # POST /v1/recommend/feedback (thumbs up/down)
│   ├── library_rating.rs        # PUT/GET /v1/library/rating(s) — durable like/dislike
│   ├── scrobble.rs              # /rest/scrobble interceptor → play_history + event log
│   ├── events.rs                # POST /v1/events
│   ├── embedder.rs              # EmbedderHandle + degraded-mode boot probe
│   ├── ingest.rs                # background ingest worker glue
│   ├── discovery.rs             # catalog watcher: auto-enqueue new tracks
│   ├── diagnostics/             # trace ring, RUM, /v1/diagnostics/* handlers
│   ├── lyrics/                  # /v1/lyrics/* — tiered resolve, LRC parse, provider client
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
| `sync` | In-memory `SyncStore` — **per-room** queue + playback, keyed by `principal.room_id()` (see [music-sync → Rooms](./music-sync.md#rooms-per-user-partition)). |
| `embedder` | `EmbedderHandle`. Either an HTTP client to the sidecar or a "disabled" sentinel. |
| `embedding_store` | `track_embeddings` table + ingest queue. |
| `metadata_store` | `track_metadata` cache (filter inputs). |
| `event_store` | Append-only event log. |
| `play_history` | MMR recency clock. |
| `feedback` | Per-session thumbs-up/down store. |
| `ratings` | Durable per-entity like/dislike store (`RatingStore`; track/album/artist). |
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

## Per-user taste isolation (PR E)

Gateway-owned taste state partitions by `user_id` (the resolved
`Principal`). The `music-recommend` stores (`events`, `play_history`,
`recommend_feedback`, `track_affinity`, `entity_rating`,
`recommendation`) each carry a `user_id` column (recommend migrations
`0017–0022`, backfilled to the owner `id=1`), and every store method
takes a `user_id`.

How handlers resolve it:

- **Writes** (`/v1/events`, `/rest/scrobble`, `/v1/recommend/feedback`,
  `PUT /v1/library/rating`) attribute to the caller's `principal.user_id`.
- **Recommendation scoring reads** (dislike-exclusion, like-boost,
  decayed affinity, provenance) are scoped to `principal.room_id()` —
  the room's **host** user — so a guest scores against and reads the
  host's taste profile read-only.
- **`GET /v1/library/ratings`** reads the caller's own `user_id` (a guest
  sees their own empty partition, not the host's library).

**Guest taste sandboxing:** a guest is a transient participant in a
host's room and must never reshape anyone's taste. Their events and
scrobbles are dropped from training (accepted, persisted nowhere), their
recommendation thumbs are dropped, and `PUT /v1/library/rating` returns
**403**. Diagnostics reads (admin-only) stay cross-user — the admin
observability surface is intentionally not partitioned.

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
| `guest_codes` | shareable room-join codes (PR D); `sha256(code)`, host-owned, optional expiry/max-uses |

### Guest rooms (PR D)

A host (any real account) mints a **guest code**; a visitor redeems it for
an ephemeral guest principal that joins the host's sync room — a shared
jukebox (D5). The surface:

- `POST /oauth/guest` (public, in `oauth/handlers.rs`) — redeem a code.
  No PKCE, no password: the code is the credential. Mints a **single
  access token, no refresh** (guests are transient and hard-capped by the
  guest account row's `expires_at`, which `resolve_principal` also
  enforces). The minted `users` row is `role='guest'` with
  `host_user_id = code.host`, so `Principal::room_id()` routes the guest
  into the host's room with zero handler changes.
- `GET/POST /v1/guest_codes` + `DELETE /v1/guest_codes/:id`
  (`guest_codes.rs`, any-authenticated tier) — host-side management. Codes
  are always owned by the caller (`host_user_id = principal.user_id`), so
  a User manages their own and nobody touches another host's; the handlers
  403 a `Role::Guest`.
- Background reaper (`spawn_guest_sweep`) deletes expired guest rows on an
  interval, cascading their tokens via the schema's `ON DELETE CASCADE`.
  Config: `[oauth] guest_session_ttl_seconds` (default 12 h),
  `guest_sweep_interval_seconds` (default 1 h, `0` disables).

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

## Catalog discovery (`discovery.rs`)

Keeps the embedding queue in step with Navidrome, so newly-added music
becomes recommendable on its own. Before this existed, the only door into
the ingest queue was `POST /v1/recommend/enqueue` — in practice a manual
`scripts/enqueue_all_tracks.py` run — so new tracks browsed and played
fine but were invisible to `/v1/recommend/*` until someone remembered.

Stateless by construction: it enumerates the catalog and re-offers every
id to `EmbeddingStore::enqueue_many`, whose `INSERT OR IGNORE` on
`(track_id, model_version)` decides what's actually new. There is no
"last seen" cursor to persist, skew, or corrupt.

Two tiers, mirroring the browse cache's list-vs-entity TTL split:

| Tier | Default cadence | Upstream calls | Catches |
|---|---|---|---|
| Recent | every 5 min | `getAlbumList2?type=newest` + one `getAlbum` per album | a freshly-imported album |
| Full | boot, then every 24 h | paged empty-query `search3` | tracks added to a **pre-existing** album, and a fresh install's whole catalog |

The full tier isn't redundant: Navidrome's `newest` ordering keys on
*album* creation, so a track dropped into an album that already existed
never appears in the recent scan.

Runs whether or not the embedder is up — queue rows are durable and the
ingest worker drains them when the sidecar returns. Scan errors are
logged, never propagated: a transient upstream hiccup must not kill the
loop, and the queue is unchanged in the meantime.

Like `search/catalog.rs`, it uses `music_subsonic::Client` directly
rather than our own `/rest` proxy, so it neither reads nor pollutes the
L2 browse cache.

`POST /v1/admin/discovery/scan` runs the full sweep on demand (admin
only, works even when `[discovery] enabled = false`).

## Gateway-owned playlists (`playlists/`)

Decision D6 of the user-system plan moved playlists off Navidrome's
`/rest/*` onto `/v1/playlists/*` so membership can be **private per-user**.
One Navidrome account backs the whole gateway, so per-user privacy can only
live on our side; the *catalog* (the tracks) stays shared on Navidrome.

- `store.rs` — `PlaylistStore`, a pure id-plumbing layer over two tables in
  the **OAuth pool** (`gateway-state.sqlite`, migration `0007_playlists.sql`):
  `playlists` (owner, name, `visibility`, timestamps) and `playlist_tracks`
  (ordered `(playlist_id, position) → track_id`). It shares the OAuth pool
  (built from `OauthStore::pool()` in `AppState::new`) so `owner_user_id`'s
  foreign key and `ON DELETE CASCADE` work without a cross-file reference. It
  stores Navidrome **track ids only** and never touches catalog metadata.
- `handlers.rs` — the `/v1/playlists/*` CRUD. Authorization is two-layer:
  reads are any-authenticated (own + others' `shared`; a private playlist the
  caller doesn't own is **404**, not 403, so existence isn't leaked), and the
  mutating verbs self-gate on the `WritePlaylist` capability (a **guest** gets
  **403**) and then on ownership (non-owner → 404). `GET /v1/playlists/:id`
  returns the row plus ordered `track_ids`; clients hydrate those against
  `/rest/getSong`.

The endpoint surface is in [API.md](../API.md#playlists). Existing Navidrome
playlists are copied into the owner (`user_id=1`) once by
`scripts/import_navidrome_playlists.py` — idempotent by playlist name; it
reads through the verbatim `/rest/*` proxy and writes through the new
endpoints, so it needs only a gateway bearer.

## Lyrics (`lyrics/`)

Per-track lyrics for `/v1/lyrics/*`. The split to know: **storage lives in
`music-recommend`** (`LyricsStore`, migration `0023_track_lyrics.sql`,
`gateway-state.recommend.sqlite`) because the external lookup key — artist,
title, album, duration — is already in `track_metadata` in that same pool.
Everything *policy* lives here.

- `lrc.rs` — LRC text → `[{start_ms, text}]`. Pure, dependency-free, and the
  only place in the project that understands the format; the web client and
  the TUI consume the parsed shape. Handles the quirks that matter: repeated
  timestamps on one line (a chorus written once, sung twice — each stamp
  becomes its own line), `[mm:ss.xx]` vs `[mm:ss:xx]`, fraction scale set by
  digit count (`.5`/`.50`/`.500` all mean 500 ms), enhanced word-level
  `<00:12.34>` tags, and `[ar:…]`-style metadata that is not a lyric.
  Timestamped *empty* lines are deliberately kept — they mark instrumental
  gaps, and dropping them leaves the previous line highlighted through a
  whole solo.
- `lrclib.rs` — client for the external community database. Its error type
  has **no not-found variant**: absence is `Ok(None)`, only failure is
  `Err`. The resolver depends on that split.
- `resolver.rs` — the tiered lookup (cache → Navidrome → provider exact →
  provider without album → duration-guarded fuzzy search → miss), with
  single-flight per `track_id` and a semaphore bounding outbound
  concurrency.
- `handlers.rs` — `GET /v1/lyrics/:track_id` (any-authenticated, ETag'd) and
  `POST /v1/lyrics/:track_id/refresh` (`WriteTaste`; guests 403).

**The invariant worth preserving: a failure is never cached.** A provider
404 is knowledge — it stores a `source='none'` row for `miss_ttl_days`, so
an un-lyriced track isn't re-looked-up on every play. An unreachable
provider is not knowledge: nothing is written, an expired cached hit is
served if one exists, and otherwise the endpoint answers **503** rather than
a 200 that would read as "this song has no lyrics".

Navidrome always wins over the provider, because the file's own tags or
`.lrc` sidecar are ground truth for *that file* — someone who re-timed a
live version or fixed a transcription meant it. Note that Navidrome only
*surfaces* tags; it never fetches lyrics from the internet, so on a library
whose files carry none, the provider tiers do essentially all the work.

`AppState::lyrics()` is `Option`: `[lyrics] enabled = false`, or a resolver
that fails to build (an unparseable `provider_url`), degrades to `None` and
the routes 404. A leaf feature must not block boot.

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
- ~~**CLI uses static bearer**~~. **Done** — the CLI now uses the OAuth
  Device Authorization Grant (RFC 8628); the static bearer was removed.
- **No Prometheus `/metrics`.** The M0 trace ring covers the same
  ground for now and the `/diagnostics` page is enough for a single
  operator; we'll add `/metrics` if external scraping ever matters.
- **No `client_events` ring-trim.** The table grows; see
  `docs/components/music-gateway.md` follow-ups via memory.
