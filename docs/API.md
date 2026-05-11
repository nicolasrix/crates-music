# Gateway API reference

Every HTTP endpoint the gateway exposes, organised by area. The
source of truth is the route table in
[crates/music-gateway/src/app.rs](../crates/music-gateway/src/app.rs);
this doc reflects the handlers in that file.

All endpoints require HTTPS — there is no plain-HTTP server. Examples
in this doc use `-k` (`--insecure`) because mkcert's local CA isn't
installed in `curl`'s trust store on most setups.

## Auth model

There are three authentication modes that gate `/v1/*` and `/rest/*`:

1. **Bearer token from `gateway.toml`** — the legacy static token.
   Used by the CLI today.
2. **OAuth-issued access token** — short-lived (1 h), looked up by
   `sha256(token)`. Used by the web app.
3. **Setup mode** — gateway hasn't been configured yet. Only
   `/oauth/setup` accepts requests; everything else returns 401.

A request is authenticated if its `Authorization: Bearer <token>` (or
`?access_token=<token>` query param) matches either (1) or (2). The
query-param path is required by `<audio>` and `<img>` URLs that can't
set headers — see RFC 6750 §2.3.

## Public endpoints

These don't require auth.

### `GET /healthz`

Liveness check. Returns 200 with the gateway version.

```bash
curl -k https://gateway.local:8443/healthz
```

```json
{"status": "ok", "service": "music-gateway", "version": "0.1.0"}
```

### `POST /oauth/setup`

One-time bootstrap. Sets the master password. Only accepts requests
when no master password is configured yet; subsequent requests return
410 Gone.

The setup token is logged to stderr on first start
(`gateway is unconfigured — visit https://… with token: <hex>`). It's
single-use.

Form body (HTML form, not JSON):

| Field | Description |
|---|---|
| `token` | The setup token from the gateway logs. |
| `password` | The master password. Hashed with Argon2id and stored. |

### `GET /oauth/login` / `POST /oauth/login`

Browser login form. The GET serves the HTML; the POST processes
credentials and creates a session. Used by the OAuth Authorization
Code flow when the user isn't already logged in.

### `GET /oauth/authorize`

Authorization endpoint. Standard OAuth 2.1 + PKCE.

| Query param | Description |
|---|---|
| `client_id` | Pre-registered client ID. |
| `redirect_uri` | Must match a `redirect_uris` entry for this client. |
| `response_type` | Must be `code`. |
| `state` | Opaque CSRF token; round-tripped to the redirect. |
| `code_challenge` | Base64URL of `sha256(code_verifier)`. |
| `code_challenge_method` | Must be `S256`. |

If the user is logged in, redirects to `redirect_uri` with `code` and
`state`. Otherwise redirects to `/oauth/login`.

### `POST /oauth/token`

Token endpoint. Two grant types:

**Authorization code:**
```
grant_type=authorization_code
code=<from /oauth/authorize>
redirect_uri=<must match the original>
client_id=<must match>
code_verifier=<the original PKCE verifier>
```

**Refresh:**
```
grant_type=refresh_token
refresh_token=<from previous token response>
client_id=<must match>
```

Refresh tokens rotate on every refresh — the old one is invalidated
and a new one is issued. This bounds replay damage.

Response:
```json
{
  "access_token": "<opaque>",
  "token_type": "Bearer",
  "expires_in": 3600,
  "refresh_token": "<opaque>",
  "scope": ""
}
```

### `POST /oauth/revoke`

RFC 7009 token revocation. Body is form-encoded:

```
token=<the token to revoke>
token_type_hint=refresh_token  # optional
```

Always returns 200, even if the token wasn't found (per RFC).

## Protected endpoints

Below this line, every endpoint requires auth.

### `GET /v1/whoami`

Diagnostic. Returns the gateway version. (Doesn't actually surface
*which* token authenticated, by design — single-user means there's
nothing useful to disambiguate.)

### Sync

#### `GET /v1/sync/snapshot`

Current sync state. Use this to bootstrap a client; subsequent updates
arrive over the WebSocket.

Response:
```json
{
  "version": 42,
  "playback": { "track_id": "abc", "position_ms": 12345, "playing": true },
  "queue": { "items": [...], "current_index": 3 },
  "likes": ["track_a", "track_b"]
}
```

#### `POST /v1/sync/ops`

Submit a `SyncOp` (queue change, like, playback update). The gateway
applies it to canonical state and broadcasts to all WebSocket
subscribers.

#### `GET /v1/sync` (WebSocket)

WebSocket for sync fan-out. Send `ClientMessage`s, receive
`ServerMessage`s. Shapes are defined in
[crates/music-sync/src/wire.rs](../crates/music-sync/src/wire.rs).

### Recommender

There are three recommend endpoints. They share the same
post-filter / queue-context / session-id machinery; the difference is
how they pick the *seed vector* the ANN is queried with.

| Endpoint | Seed strategy |
|---|---|
| `GET /v1/recommend/next` | Single track id. 404 if not embedded. |
| `POST /v1/recommend/from-any` | First indexed track in a candidate list (album-start case). |
| `POST /v1/recommend/from-seeds` | Multi-seed Σ-similarity fan-out (playlist case). |

All three accept an optional `queue_context` for server-side
diversity filtering and an optional `session_id` for per-session
downvote exclusion. See "Queue context & filtering" below.

#### `GET /v1/recommend/next`

Top-N tracks similar to a single seed.

| Query param | Required | Default | Description |
|---|---|---|---|
| `seed` | yes | — | Track ID. Must be embedded; returns 404 otherwise. |
| `n` | no | `20` | How many results. Capped at 100. `0` returns 400. |

Response:
```json
{
  "seed": "track_abc",
  "model_version": "clap-music_audioset_epoch_15_esc_90.14",
  "degraded": false,
  "results": [
    {"track_id": "track_xyz", "similarity": 0.847}
  ]
}
```

`degraded: true` means the embedder was unreachable at boot. Currently
the endpoint 404s in that mode — the field is reserved for a future
tag-only fallback.

`similarity` is cosine similarity in `[-1, 1]`. Higher = more similar.

#### `POST /v1/recommend/from-any`

First-indexed-wins fallback. Tries `candidate_seeds` in order, returns
results for the first one with an ANN entry. Replaces the
client-side loop the web client used to run when a playlist start
was clicked and the head track wasn't embedded.

Body:
```json
{
  "candidate_seeds": ["track_a", "track_b", "track_c"],
  "n": 20,
  "queue_context": { ... },          // optional, see below
  "session_id": "rec-2026-05-11-..."  // optional, see below
}
```

Response:
```json
{
  "seed_used": "track_b",
  "model_version": "clap-music_...",
  "degraded": false,
  "results": [ {"track_id": "...", "similarity": 0.84} ]
}
```

- 400 if `candidate_seeds` is empty, longer than 200, or `n == 0`.
- 404 if none of the candidates are embedded.

#### `POST /v1/recommend/from-seeds`

Multi-seed station. Sample up to `sample_size` seeds from the request,
fan out per-seed ANN queries, fold them with Σ-similarity. Used for
playlist-as-station and "more like these scrobbles."

Body:
```json
{
  "seeds": ["track_a", "track_b", "track_c", ...],
  "seed_weights": [3, 2, 2, 1, ...],   // optional; must match seeds.len()
  "per_seed_n": 20,                     // default 20
  "sample_size": 8,                     // default 8
  "top_n": 20,                          // default 20, capped at 100
  "exclude_track_ids": ["track_x"],     // already-added, dismissed, etc.
  "queue_context": { ... },             // optional
  "session_id": "rec-2026-..."          // optional
}
```

- 400 if `seeds` is empty or longer than 200.
- 400 if `seed_weights.len() != seeds.len()`. Negative weights clamp
  to 0; zero-weight seeds contribute nothing.

Response:
```json
{
  "model_version": "clap-music_...",
  "degraded": false,
  "results": [ {"track_id": "...", "similarity": 1.42} ],
  "all_seeds_unindexed": false
}
```

`all_seeds_unindexed: true` lets the UI distinguish "playlist hasn't
been embedded yet" from "we just don't have anything more to suggest."

Note: with Σ-similarity scoring, `similarity` values are *sums* and
can exceed 1.0. They are still comparable within a single response,
but not across responses.

#### Queue context & filtering

When `queue_context` is supplied, the gateway runs candidates through
the server-side queue filter (`music_recommend::queue_filter`) before
returning. The filter handles:

- Per-artist cap (`max_per_artist`).
- Same-title dedup (`dedup_titles`).
- Diversity mode: `hard_cap` (default), `mmr`, or `off`.
- MMR `λ` (relevance vs. novelty) and `μ` (same-artist penalty).

Shape:
```json
{
  "queue_track_ids": ["t1", "t2", ...],
  "now_playing_track_id": "t1",
  "max_per_artist": 2,
  "dedup_titles": true,
  "diversity_mode": "mmr",
  "mmr_lambda": 0.8,
  "artist_penalty_weight": 0.15
}
```

- `queue_track_ids` is both counted toward state and excluded from
  results. `now_playing_track_id` counts but is *not* itself excluded.
- 400 if `queue_track_ids.len() > 400` or `exclude_track_ids.len() > 1000`.
- Out-of-range `mmr_lambda` / `artist_penalty_weight` are clamped
  server-side, not rejected.

#### Session-scoped downvotes

When `session_id` is supplied, the gateway excludes any tracks the
user has thumbs-downed *within this session*. Downvotes from other
sessions are not consulted — the user may have been in a different
mood. The session id is a client-generated opaque string; the
gateway trusts it.

#### `POST /v1/recommend/enqueue`

Enqueue tracks for embedding. Idempotent on
`(track_id, model_version)`.

Body:
```json
{"track_ids": ["track_a", "track_b", "track_c"]}
```

Response (202):
```json
{"enqueued": 3}
```

The `enqueued` count is inserts attempted, not the new queue depth —
already-enqueued tracks don't bump it.

#### `POST /v1/recommend/feedback`

Capture thumbs-up / thumbs-down on a played recommendation. Three
states are encoded in the same request shape:

```json
{
  "track_id": "track_a",
  "vote": "up",                  // "up" | "down" | null
  "session_id": "rec-2026-...",
  "occurred_ms": 1715472000000   // optional; server clock if absent
}
```

- `"up"` / `"down"` upserts the `(track_id, session_id)` row.
- `null` (or omitted) deletes the row — the user un-clicked an active
  thumb. Keeping one shape across all three states means clients have
  a single mutation call site.

Response carries the fresh `(up, down)` totals so the player can
re-render without a follow-up GET:
```json
{ "track_id": "track_a", "up": 7, "down": 1 }
```

- 400 if `track_id` or `session_id` are empty or oversized
  (256 / 128 chars).
- Session attribution is trusted: a hostile client could spam votes
  under rotating session ids. Single-tenant — not a security concern
  today.

### Event log

#### `POST /v1/events`

Append-only event log for user-interaction signal. Used today by the
diagnostics dashboards (recently-played list) and reserved for the
behavioural recommender.

Body:
```json
{
  "events": [
    {
      "event_type": "scrobble",
      "track_id": "track_a",
      "occurred_at": 1712345678901,
      "metadata": {"played_ms": 180000}
    },
    {
      "event_type": "skip",
      "track_id": "track_b",
      "occurred_at": 1712345700000
    }
  ]
}
```

`event_type` is one of: `scrobble`, `skip`, `like`, `unlike`, `seek`.
Unknown values are accepted and stored verbatim.

`occurred_at` is client-supplied unix milliseconds. The gateway adds
its own `received_at` so we can detect clock skew.

Constraints:
- Empty `events` array → 400.
- More than 1000 events in one batch → 413. Coalesce smaller.
- Malformed JSON → 400.

Response (202):
```json
{"accepted": 3}
```

### Diagnostics

Authenticated read-mostly endpoints under `/v1/diagnostics/*` that
back the web `/diagnostics` page. All responses are JSON; all
timestamps are unix milliseconds.

#### `GET /v1/diagnostics/traces`

Closed `tracing` spans from the gateway's M0 ring buffer, newest
first.

| Query param | Default | Description |
|---|---|---|
| `name` | — | Filter by exact span name (e.g. `recommend.next`). |
| `since_ms` | — | Only spans started on or after this unix ms. |
| `limit` | 200 | Capped server-side at 1000. |

Response items carry `fields_json` parsed back into a JSON object;
parse failure falls under `_raw`.

#### `GET /v1/diagnostics/histogram`

Per-span-name duration histogram. SQL aggregates `count/min/max/sum`;
quantiles are computed in Rust via nearest-rank on the sorted slice.

| Query param | Default | Description |
|---|---|---|
| `since_ms` | — | Optional lower bound on `started_ms`. |

Response: array of `{name, count, min_ms, max_ms, sum_ms, p50_ms, p95_ms, p99_ms}`.

#### `GET /v1/diagnostics/queue_depth`

Embedding ingest queue counts by status.

| Query param | Default | Description |
|---|---|---|
| `model_version` | `recommend_model_version` (the active one) | Explicit override is useful during rolling model upgrades. |

Response: `{not_started, in_progress, done, failed}`.

#### `GET /v1/diagnostics/recently_played`

Recent scrobbles, newest first. Sourced from the event log written by
the `/rest/scrobble` interceptor.

| Query param | Default | Description |
|---|---|---|
| `limit` | 50 | Capped server-side. |

#### `POST /v1/diagnostics/client_events`

Browser RUM batch upload (web vitals + custom marks).

Body:
```json
{
  "events": [
    {
      "session_id": "<one per page-load>",
      "occurred_ms": 1715472000000,
      "name": "web-vital.LCP",
      "value_ms": 1240.5,
      "rating": "good",
      "page_path": "/albums",
      "fields": { "...": "..." }
    }
  ]
}
```

- 422 on schema mismatch.
- 413 if `events.len() > 50`. Web emitter caps batches at this size.
- Server stamps `received_ms` and `user_agent` (truncated to 256 chars).

Response: `{"accepted": N}`.

#### `GET /v1/diagnostics/client_events`

Most recent RUM events, newest received first. Two timestamps preserved:
client-supplied `occurred_ms` and gateway-stamped `received_ms`.

| Query param | Default | Description |
|---|---|---|
| `limit` | 100 | |
| `name` | — | Exact event-name filter. |

#### `GET /v1/diagnostics/recommend/queue_fill`

Per-`recommend.from_*` span: requested vs. returned, shortfall reason,
queue-context settings observed.

#### `GET /v1/diagnostics/recommend/shortfall`

Shortfall histogram across recent recommend calls — *how often* the
filter / ANN couldn't produce the requested `n`, and *why*.

#### `GET /v1/diagnostics/recommend/similarity`

Distribution of admitted vs. dropped similarity scores per recommend
call. Lets you see whether the filter is rejecting near-misses or
junk.

#### `GET /v1/diagnostics/recommend/top_results`

Most-recommended tracks across a recent window. "What is the system
in love with this week?"

#### `GET /v1/diagnostics/recommend/feedback`

Thumbs-up / thumbs-down aggregates. Two flavours: per-track counts
and per-day totals.

#### `GET /v1/diagnostics/recommend/latent_space`

UMAP 2D projection of every embedded track. Backs the
`/recommend/latent-space` web view. Sourced from the
`embedding_projection_2d` table (computed by the
`backfill_projection` binary; see
[components/music-recommend.md](./components/music-recommend.md)).

### Intercepted Subsonic endpoints

#### `ANY /rest/scrobble`

Intercepted *before* the catch-all proxy. The gateway:

1. Parses `id`, `submission`, `time` query params.
2. On submission (i.e. not a now-playing ping): writes
   `play_history.last_played_ms` (the MMR recency clock) and appends a
   `Scrobble` event to the event log. Both writes are best-effort —
   Navidrome remains the canonical play-count ledger.
3. Forwards the unmodified request to the upstream proxy.

`time` is the client-supplied unix-ms timestamp; offline scrobble
batches can deliver minutes or hours after the play, and the event
log's `occurred_at` should reflect the user's clock.

#### `ANY /rest/getCoverArt`

Intercepted to layer the L2 cover-art cache and the placeholder
self-heal:

- Cache hits return `Cache-Control: public, max-age=300, must-revalidate`.
- Placeholder bodies (our SVG, or Navidrome's default "no artwork"
  body — detected by etag dedup across distinct cover-art ids) return
  `Cache-Control: no-cache, must-revalidate` so a later real cover
  isn't shadowed by a stale browser cache.
- On a placeholder cache hit the gateway opportunistically refetches
  upstream in the background (coalesced to one fetch per key per
  cooldown window) so missing art self-heals without a manual cache
  flush.

### Subsonic pass-through

#### `ANY /rest/*`

Anything not specifically intercepted forwards verbatim to Navidrome.
The proxy:

1. Adds the upstream credentials (configured in `[upstream]`).
2. Layers the L2 metadata cache for browse endpoints.
3. Streams the response back to the client.

This means clients can use any existing Subsonic SDK without
reimplementation.

Endpoints currently augmented with caching:

| Endpoint | Cache |
|---|---|
| `/rest/getAlbumList2` | L2 (`browse_ttl_seconds`). Bypassed when `type=random`. |
| `/rest/getAlbum` | L2 (`browse_ttl_seconds`) |
| `/rest/getCoverArt` | L2 (separate budget; see above) |
| `/rest/scrobble` | intercepted (see above) |
| `/rest/ping` | not cached |
| `/rest/stream` | L4 (planned) |
| anything else | passthrough only |

Subsonic spec: <http://www.subsonic.org/pages/api.jsp>.
OpenSubsonic extensions: <https://opensubsonic.netlify.app/>.

## Status code conventions

| Code | When |
|---|---|
| 200 | Successful read. |
| 202 | Accepted — for fire-and-forget writes (events, enqueue). |
| 400 | Malformed input (bad JSON, invalid query param). |
| 401 | Missing or invalid auth token. |
| 403 | Auth valid but not authorized. (Rare today; single-user.) |
| 404 | Resource not found, including "seed track not embedded yet" on recommend endpoints. |
| 410 | `/oauth/setup` after master password is set. |
| 413 | Batch too large (events, client_events). |
| 422 | RUM `client_events` payload schema mismatch. |
| 500 | Server bug. Logged with stack trace. |
| 502 | Upstream Navidrome failure. |
