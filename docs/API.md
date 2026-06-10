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

**Identity & roles.** `require_bearer` resolves every authenticated
request to a `Principal { user_id, role, host_user_id? }` and injects it
downstream. `role` is one of `admin | user | guest`. A NULL-`user_id`
token (the legacy static bearer, or pre-multi-user rows) resolves to the
owner (id=1, admin). The protected surface is split into two tiers:

- **Any authenticated principal** — browse, play, recommend reads, room
  control, ratings/events, `whoami`.
- **Admin-only** (`require_admin`, 403 otherwise) — `/v1/admin/*`,
  `/v1/diagnostics/*`, and recommender maintenance (`refit_whitening`,
  `enqueue`).

Real accounts (admin/user) are provisioned by an admin (see
[`/v1/admin/users`](#post-v1adminusers)); guests come later (PR D).

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
credentials and creates a session bound to the resolved `user_id`. Used
by the OAuth Authorization Code flow when the user isn't already logged
in.

Form fields: `username` (optional — absent/empty logs in the owner, so
the original master-password-only login is unchanged) and `password`.
A real account supplies its `username`. Unknown username, wrong
password, and not-yet-bootstrapped all return an identical 401 (no
user-existence or bootstrap-state oracle). The logged-in `user_id`
threads through the session → auth code → token chain, so the issued
access token resolves to that account.

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

Resolves the calling principal — clients call this post-login to drive
role-gated UI.

```json
{
  "user_id": 2,
  "role": "user",
  "username": "alice",
  "display_name": "Alice",
  "host_user_id": null
}
```

`username`/`display_name` are `null` for accounts that have none (e.g.
guests). `host_user_id` is set only for guests (PR D).

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

There are four recommend endpoints. They share the same ANN; the
difference is how they pick the *query vector*.

| Endpoint | Query vector source |
|---|---|
| `GET /v1/recommend/next` | Embedding of a single track id. 404 if not embedded. |
| `POST /v1/recommend/from-any` | Embedding of the first indexed track in a candidate list (album-start case). |
| `POST /v1/recommend/from-seeds` | Multi-seed Σ-similarity fan-out (playlist case). |
| `GET /v1/recommend/station` | **text** embedding of a natural-language prompt via the embedder's text encoder. |

The track-seeded variants accept an optional `queue_context` for
server-side diversity filtering and an optional `session_id` for
per-session downvote exclusion (see "Queue context & filtering"
below). `/v1/recommend/station` is currently a thin "embed text →
ANN top-N" pass; the queue-context / filter machinery is not wired
into it yet.

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

`model_version` is dynamic and reflects the deployed backend. The
`clap-music_...` strings throughout these examples are CLAP-era; a
CLaMP 3 deployment reports a `weights_clamp3_saas_...` version.

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

#### `POST /v1/recommend/similar_albums`

"Albums that sound like this album." Fans out per-seed ANN queries
over the supplied tracks, aggregates hits by `album_id`, returns the
top-N groups by Σ-similarity. Backs the similar-albums rail on the
web Album page.

Body:
```json
{
  "seed_track_ids": ["track_a", "track_b", ...],
  "exclude_album_ids": ["album_self"],   // drop the seed album from results
  "n": 10,                                // default 10, capped at MAX_N
  "per_seed_n": 50,                       // default 50
  "sample_size": 8                        // default 8
}
```

Response:
```json
{
  "model_version": "clap-music_...",
  "results": [
    {"album_id": "alb_xyz", "score": 4.12, "supporting_tracks": 5}
  ],
  "all_seeds_unindexed": false
}
```

- `score` is the sum of per-hit similarities for tracks rolled up to
  this album; not comparable across responses.
- `supporting_tracks` is the number of distinct tracks on the
  candidate album that showed up as ANN hits. A high value means the
  match is broad-based, not driven by one outlier track.
- `all_seeds_unindexed: true` when none of the seeds have an
  embedding yet — UI should render "still indexing this album."

#### `POST /v1/recommend/similar_artists`

Same shape as `similar_albums` but aggregates by `artist_id` instead.
`exclude_artist_ids` replaces `exclude_album_ids`; the result item is
`{artist_id, score, supporting_tracks}`.

#### `GET /v1/recommend/station`

Natural-language "playlist from a prompt." The gateway sends the
prompt to the embedder's `/embed/text` endpoint (the backend's text
encoder), then runs the resulting vector through the same content ANN
that powers `/v1/recommend/next`. The vector dimension is
backend-dependent (512 for CLAP, 768 for CLaMP 3), not a hardcoded
512.

When `[recommend].whitening_enabled = true` (the default), text-query
stations are centered by the cross-modal text mean before the shared
All-but-the-Top de-coning. This corrects the embedding anisotropy
that would otherwise collapse unrelated text queries toward each
other (e.g. "death metal" and "smooth jazz" returning near-identical
results). The text mean is refreshed via
`POST /v1/recommend/refit_whitening`.

| Query param | Required | Default | Description |
|---|---|---|---|
| `text` | yes | — | Prompt. Trimmed; must be non-empty and ≤ 500 chars. |
| `n` | no | `20` | How many results. Capped at 100. `0` returns 400. |

Response:
```json
{
  "query": "sunny afternoon",
  "model_version": "clap-music_audioset_epoch_15_esc_90.14",
  "results": [
    {"track_id": "track_xyz", "similarity": 0.387}
  ]
}
```

`model_version` is backend-dependent — the example above is the CLAP
string; a CLaMP 3 deployment reports a `weights_clamp3_saas_...`
version instead.

- 400 if `text` is empty or `n == 0`.
- 400 if `text` exceeds 500 chars.
- 502 if the embedder returns an error (e.g. text too long for the
  encoder).
- 503 if the embedder isn't configured or wasn't ready at the last
  health probe. **No degraded-mode fallback** — without the text
  encoder there's no seed track to derive tag-similarity from.

There is no `degraded` field on the response (unlike track-seeded
endpoints): the operation either has the text encoder available or
it returns 503.

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

#### `POST /v1/recommend/refit_whitening`

Admin endpoint. Empty request body. Refits the All-but-the-Top
whitening transform from the current embedded corpus, persists it,
installs it on the ANN, and rebuilds the index so stored vectors are
re-whitened. Best-effort: also fits the cross-modal text mean if the
embedder is reachable, so text-station queries are centered by the
text-modality mean rather than the audio mean. Use after a large
batch of new embeddings, or to (re)enable whitening.

Response (200):
```json
{
  "model_version": "weights_clamp3_saas_...",
  "n_samples": 7349,
  "k": 7,
  "dim": 768,
  "fitted_at_ms": 1780174710400,
  "has_text_mean": true
}
```

- `has_text_mean` is `false` when the embedder was unreachable during
  the refit — text stations then fall back to centering by the audio
  mean.
- 409 Conflict if there are no embeddings to fit on.
- 500 if listing / fitting / persisting fails.

### Library ratings

The user's **durable like/dislike** for a track, album, or artist. This
is the gateway's own taste store — we never write back to Navidrome, so a
verdict lives only here. It is a distinct channel from the recommendation
thumbs (`POST /v1/recommend/feedback`): that rates whether a *recommendation*
was a good fit (session-scoped, decays); this rates the *entity itself*
(durable, never decays). Enforcement is **always-on**, independent of the
`preference_enabled` flag:

- **dislike** hard-excludes the entity from play — a disliked track, or
  *every track* of a disliked album/artist, is dropped from all recommender
  candidate generation and auto-skipped by the player on queue advance (a
  direct click still overrides the skip).
- **like** boosts relevance, weighted `track > album > artist` (additive;
  see the `like_bonus*` knobs in [CONFIGURATION.md](./CONFIGURATION.md)).

**Per-user partition (PR E).** Ratings are stored per `user_id` (the
calling principal). One household member's verdicts are invisible to
another, and they only shape *that user's* recommendations. The
recommend reads that consume ratings (dislike-exclusion, like-boost) are
scoped to the **room's host user**, so a guest gets the host's
personalised recs read-only. A **guest cannot write a rating** — `PUT
/v1/library/rating` returns **403** for a guest principal (they have no
library of their own and must never reshape the host's taste).

#### `PUT /v1/library/rating`

Set or clear one entity's verdict. The nullable `rating` field encodes all
three states in one request shape (so the client has a single mutation call
site, mirroring the feedback endpoint):

```json
{
  "kind": "album",          // "track" | "album" | "artist"; defaults to "track"
  "id": "al-123",
  "rating": "dislike"       // "like" | "dislike" | null
}
```

- `"like"` / `"dislike"` upserts the `(kind, id)` row.
- `null` (or omitted) clears it back to neutral (deletes the row).
- `kind` defaults to `"track"` so the original track-only wire shape
  (`{id, rating}`) keeps working unchanged.

Response echoes the stored verdict (200):
```json
{ "kind": "album", "id": "al-123", "rating": "dislike" }
```

- 400 if `id` is empty or longer than 256 chars, or the body is invalid JSON.
- 500 if the rating store write fails.

#### `GET /v1/library/ratings`

Every rated entity, newest first. Ids only — the client hydrates
titles/art via the Subsonic `getSong` / `getAlbum` / `getArtist` paths
(the gateway's metadata cache has no cover art).

```json
{
  "ratings": [
    { "kind": "track",  "id": "tr-9", "rating": "like" },
    { "kind": "artist", "id": "ar-2", "rating": "dislike" }
  ]
}
```

- 500 if the rating store read fails.

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

**Per-user partition + guest sandboxing (PR E).** Events are attributed
to the calling `user_id` and only feed *that user's* taste/affinity. A
**guest's events are dropped from training**: the request is accepted
(so the player's fire-and-forget batcher never errors) but nothing is
persisted and no affinity is folded — the response is `{"accepted": 0}`.
The same drop applies to a guest's `/rest/scrobble` (play_history /
event log / affinity writes are skipped) and to a guest's thumb vote on
`POST /v1/recommend/feedback` (no write; zeroed counts echoed).

### Admin

All `/v1/admin/*` endpoints are **admin-only** — a User or Guest token
authenticates but is 403'd by `require_admin`.

#### `GET /v1/admin/users`

List the real accounts (admin/user). Guests are excluded. No credential
material is returned.

```json
{
  "users": [
    {"id": 1, "username": "owner", "display_name": "Owner", "role": "admin", "created_at": 0},
    {"id": 2, "username": "alice", "display_name": "Alice", "role": "user", "created_at": 1718000000000}
  ]
}
```

#### `POST /v1/admin/users`

Create a real account. JSON body:

| Field | Required | Description |
|---|---|---|
| `username` | yes | Unique. |
| `password` | yes | ≥ 12 chars. Argon2id-hashed before storage. |
| `role` | yes | `admin` or `user` (`guest` is rejected — guests come from the guest-code flow). |
| `display_name` | no | Defaults to none. |

Returns `201 {"id": <new id>}`. A duplicate username is `409
{"error":"username_taken", ...}`; validation failures are `400`.

#### `DELETE /v1/admin/users/:id`

Remove an account and cascade-delete its tokens/sessions. `204` on
success, `404` if unknown. The owner (id=1) is undeletable (`400`).

#### `POST /v1/admin/users/:id/password`

Admin-driven password reset (account recovery without email). JSON body
`{"password": "<≥12 chars>"}`. Rewrites the Argon2 hash in place — the
account keeps its id and all dependent data. `204` on success, `404` if
unknown.

> The **owner's** master password is reset out-of-band on the gateway
> host: `music-gateway --config … reset-master-password` (host access is
> the root of trust). See the runbook.

#### `POST /v1/admin/cache/invalidate`

Clear the L2 browse cache. Use when new content has been added to
Navidrome and you don't want to wait for the cache TTL (default 24 h)
to expire. Cover-art rows are preserved — Navidrome cover ids are
content-addressed, so the old entries become unreachable rather than
stale.

No body. Response:
```json
{"removed": 142}
```

Surfaced as a "Refresh metadata" button on the web `/diagnostics`
page.

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

#### `GET /v1/diagnostics/span_series`

Time-series of closed-span durations for a single span name. Backs
the timeline charts on `/diagnostics/tracing`.

| Query param | Default | Description |
|---|---|---|
| `name` | — | **Required.** Span name to plot (no "all" sentinel; use `histogram` for the cross-name view). |
| `since_ms` | — | Optional lower bound on `end_ms`. |
| `limit` | 2000 | Points returned. Clamped to `[1, 10000]`. |

Response:
```json
{
  "name": "fetch_clip.stream_body",
  "points": [
    {"end_ms": 1739000000000, "duration_ms": 142}
  ]
}
```

#### `GET /v1/diagnostics/span_children`

Per-parent subspan aggregation. For a given parent span name (e.g.
`ingest.fetch_clip`), returns the count + sum + mean duration of each
distinct child name observed under it. Backs the child-breakdown
panel that separates metadata-fetch from body-read for the ingest
audio fetcher.

| Query param | Default | Description |
|---|---|---|
| `name` | — | **Required.** Parent span name. |
| `since_ms` | — | Optional lower bound on the parent's `started_ms`. |

Response:
```json
{
  "parent_name": "ingest.fetch_clip",
  "parent_count": 124,
  "parent_sum_ms": 8120,
  "children": [
    {"name": "fetch_clip.get_song",      "count": 124, "sum_ms":  720, "mean_ms":  5.8},
    {"name": "fetch_clip.stream_request","count": 124, "sum_ms": 1140, "mean_ms":  9.2},
    {"name": "fetch_clip.stream_body",   "count": 124, "sum_ms": 6260, "mean_ms": 50.4}
  ]
}
```

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

#### `GET /v1/diagnostics/recommend/latent_neighbours`

Hover overlay for the latent-space scatter. Returns the k nearest
neighbours of a seed track in the **original** CLAP space — the
distances UMAP doesn't preserve. Cheap (sub-ms HNSW query) and
fetched on-demand so `latent_space` itself stays small.

| Query param | Default | Description |
|---|---|---|
| `track_id` | — | **Required.** Seed track. |
| `k` | 10 | Neighbours to return. Clamped to `[1, 100]`. |

Response:
```json
{
  "track_id": "track_abc",
  "neighbours": [
    {"track_id": "track_xyz", "cosine_distance": 0.142}
  ]
}
```

- `cosine_distance = 1 - cosine_similarity`. Range `[0, 2]`, with
  `0` = identical direction. Reported as distance (not similarity)
  because the UI maps line length → distance.
- 404 if the seed has no vector in the ANN (rare; possible after a
  model-version flip). UI degrades to "no overlay" for that point.

#### `GET /v1/diagnostics/recommend/sessions?limit=N`

Recent recommend-session lifetimes, newest `started_ms` first. Each
item joins the persisted `recommend_sessions` row with a count of
events stamped with that `session_id` from the event log — the
"how much signal did this session generate" indicator.

```json
{
  "items": [
    {
      "session_id": "sess-abc",
      "anchor_track_id": "t-1",
      "items_count": 30,
      "started_ms": 1739000000000,
      "ended_ms": null,
      "event_count": 5
    }
  ]
}
```

- `ended_ms = null` means the session is still active. By the
  single-active invariant there is at most one such row.
- `event_count = 0` is common right after a `start_session` op — the
  user opened a queue but hasn't hit `submission=true` on the first
  scrobble yet.
- `limit` defaults to 50, clamps to `[1, 500]`.

To reconstruct what happened during a specific session, combine this
with the events table (queryable via `EventStore::by_session`
server-side; no client endpoint exposes per-session events yet).

### Intercepted Subsonic endpoints

#### `ANY /rest/scrobble`

Intercepted *before* the catch-all proxy. The gateway:

1. Parses `id`, `submission`, `time` query params.
2. On submission (i.e. not a now-playing ping): writes
   `play_history.last_played_ms` (the MMR recency clock) and appends a
   `Scrobble` event to the event log, stamped with the currently-active
   `session_id` (read from the in-memory `SessionAnchor`). Both writes
   are best-effort — Navidrome remains the canonical play-count
   ledger, and a write failure here does not block the upstream
   forward.
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
| 409 | Conflict — e.g. `refit_whitening` with no embeddings to fit on. |
| 410 | `/oauth/setup` after master password is set. |
| 413 | Batch too large (events, client_events). |
| 422 | RUM `client_events` payload schema mismatch. |
| 500 | Server bug. Logged with stack trace. |
| 502 | Upstream Navidrome failure. |
