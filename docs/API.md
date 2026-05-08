# Gateway API reference

Every HTTP endpoint the gateway exposes, organised by area. The
source of truth is the route table in
[crates/music-gateway/src/app.rs](../crates/music-gateway/src/app.rs);
this doc is generated from that file plus the handler implementations.

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
`?access_token=<token>` query param) matches either (1) or (2).

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

#### `GET /v1/recommend/next`

Top-N tracks similar to a seed.

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
    {"track_id": "track_xyz", "similarity": 0.847},
    ...
  ]
}
```

`degraded: true` means the embedder was unreachable at boot and the
gateway is operating without recommendations. Currently the endpoint
just 404s in that mode — the field is reserved for when we have a
tag-only fallback.

`similarity` is cosine similarity in `[-1, 1]`. Higher = more similar.

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
already-enqueued tracks don't bump it. Use the queue stats endpoint
(coming) for actual queue length.

### Event log

#### `POST /v1/events`

Append-only event log for user-interaction signal. Currently
write-only — the behavioural recommender will consume it later.

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
Unknown values are accepted and stored verbatim — we'll decide later
whether to consume them.

`occurred_at` is client-supplied unix milliseconds. The gateway adds
its own `received_at` so we can detect clock skew.

`metadata` is optional opaque JSON.

Constraints:
- Empty `events` array → 400.
- More than 1000 events in one batch → 413. Coalesce smaller.
- Malformed JSON → 400.

Response (202):
```json
{"accepted": 3}
```

### Subsonic pass-through

#### `ANY /rest/*`

Any request under `/rest/*` proxies verbatim to Navidrome. The
gateway:

1. Adds the upstream credentials (configured in `[upstream]`).
2. Layers the L2 metadata cache for browse endpoints.
3. (Future) layers the L4 transcoded-audio cache for `/rest/stream`.

This means clients can use any existing Subsonic SDK without
reimplementation. The gateway intercepts only the endpoints where
caching or augmentation adds value.

Endpoints currently augmented with caching:

| Endpoint | Cache |
|---|---|
| `/rest/getAlbumList2` | L2 (browse_ttl_seconds) |
| `/rest/getAlbum` | L2 (browse_ttl_seconds) |
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
| 404 | Resource not found, including "seed track not embedded yet" on `/v1/recommend/next`. |
| 410 | `/oauth/setup` after master password is set. |
| 413 | Batch too large (events). |
| 500 | Server bug. Logged with stack trace. |
| 502 | Upstream Navidrome failure. |
