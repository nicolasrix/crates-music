# Architecture

The system has one server (the gateway) and N clients (CLI, web, mobile).
Navidrome sits behind the gateway, unmodified — it remains the catalog
source of truth.

## High-level shape

```
                    ┌──────────────────┐
                    │  Navidrome       │   (unmodified)
                    └────────▲─────────┘
                             │ Subsonic /rest/*
                    ┌────────┴─────────────────────────┐
                    │  music-gateway (Rust, axum)      │
                    │  • Subsonic proxy + augments     │
                    │  • Transcoded-stream LRU cache   │
                    │  • OAuth 2.1 server              │
                    │  • Recommender (CLAP + ANN)      │
                    │  • Event log + WebSocket sync    │
                    │  • SQLite (gateway state)        │
                    └──┬──────────────┬─────────────┬──┘
                       │              │             │
                ┌──────▼─────┐ ┌──────▼─────┐ ┌─────▼─────┐
                │  CLI       │ │  Web       │ │  Mobile   │
                │  (Rust)    │ │  (TS+React │ │  (Compose │
                │            │ │   + Vite)  │ │   MP+KMP) │
                └────────────┘ └────────────┘ └───────────┘
```

The mobile client is planned (P4); it does not exist in the codebase yet.

## Why a gateway?

Three things benefit from being shared across clients:

1. **Cache** — transcoded audio is expensive to produce. Producing it
   once on the gateway and serving it to every client beats paying
   the transcode cost per device.
2. **Recommender** — the CLAP model is hundreds of MB; running it on
   each client is impractical. Running it once on the gateway and
   exposing similarity queries over HTTP is straightforward.
3. **Sync** — playback queue + playback position + likes need to be
   consistent across devices. The gateway is the linearizer, which
   means clients don't need vector clocks or CRDTs.

Anything that **doesn't** benefit from being centralized stays on the
client: UI, decoding, audio output device, local cache eviction policy.

## Repo layout

```
crates/
  music-core/        # domain types (Track, Album, Queue, PlaybackState)
  music-subsonic/    # typed Subsonic / OpenSubsonic client (reqwest)
  music-cache/       # SQLite metadata cache + on-disk LRU audio cache
  music-player/      # native playback (rodio + symphonia)
  music-sync/        # WebSocket client + state machine
  music-recommend/   # SERVER-ONLY: CLAP embedder, ANN index, event log
  music-gateway/     # the gateway binary
  music-cli/         # the CLI binary

apps/
  web/               # TS + React + Vite

services/
  embedder/          # Python FastAPI sidecar (CLAP audio + text)
```

Each `crates/*` directory is a workspace member with its own
`Cargo.toml`. The workspace `Cargo.toml` at the root pins shared
dependency versions. See [components/](./components/) for per-crate
docs.

## Caching

Four layers, each with a different lifetime, eviction policy, and
location:

| Layer | Where | Stores | Eviction |
|---|---|---|---|
| L1 — UI state | RAM, each client | Currently-displayed views | LRU, small |
| L2 — Metadata | SQLite, each client | Tracks/albums/artists/playlists | TTL + ETag refresh |
| L3 — Audio | Disk, each client | Encoded audio files | LRU by bytes; **pinned tracks have a separate budget and are never LRU-evicted** |
| L4 — Transcoded | Disk, gateway | Pre-transcoded variants | LRU by bytes |

Two strict invariants:

1. **Audio cache is content-addressed** by `(trackId, bitrate, codec)`,
   never by URL.
2. **Metadata cache uses ETags**, not just TTLs — `If-None-Match` is
   cheaper than either staleness or full refetch. ETags are
   `sha256(body)[:16]`, computed by the gateway, so they're stable
   across processes and restarts.

Cache budgets are user-configurable per client. Defaults:
- Android: ~2 GB
- Web: ~500 MB
- CLI: ~10 GB

Lowering the cap evicts immediately, not lazily. Users want the
"my disk is full" gesture to do something visible.

See [components/music-cache.md](./components/music-cache.md) for
implementation details.

## Auth

Single-user means OAuth does the job of *device pairing + token
rotation*, not user identification.

| Client | Flow | Status |
|---|---|---|
| Web | Authorization Code + PKCE | Implemented |
| Mobile | Authorization Code + PKCE via Custom Tabs | Planned (P4) |
| CLI | Device Authorization Grant (RFC 8628) | Planned; currently uses static bearer token |

**Bootstrap:** the gateway prints a one-time setup URL on first run.
The user visits it, sets a master password (Argon2id), and from then
on the OAuth endpoints are usable.

**Storage:** five tables in `gateway-state.sqlite` —
- `users` — Argon2id master password
- `oauth_clients` — registered client_ids + redirect URIs
- `auth_codes` — short-lived (60s) authorization codes
- `refresh_tokens` — long-lived, per-device, individually revocable
- `access_tokens` — short-lived (1h), looked up by `sha256(token)`

Plus a `sessions` table for the browser login flow (not the same as
access_tokens — sessions are how the *login form* remembers you between
the password POST and the consent screen).

`require_bearer` in [crates/music-gateway/src/auth.rs](../crates/music-gateway/src/auth.rs)
accepts either an OAuth-issued access token (sha256 lookup) or the
legacy static bearer from `gateway.toml`. Token can come via
`Authorization: Bearer <token>` or `?access_token=<token>` (RFC 6750
§2.3 — required for `<audio>` and `<img>` URLs that can't set
headers).

## TLS

Local network only. The gateway uses [mkcert](https://github.com/FiloSottile/mkcert)
for local certs:

- `mkcert -install` once on each device (dev machine, phone, …)
  installs the local CA into the system trust store.
- The gateway gets a cert for `gateway.local`.
- Annual cert regeneration; not automatic.

Plain HTTP is **not** acceptable. Service Workers,
`navigator.storage.persist()`, MSE, and mobile system-browser OAuth
all require a secure context. Browsers treat `localhost` as secure
without HTTPS, but that doesn't help phone-to-gateway access on the
LAN.

See [GETTING-STARTED.md](./GETTING-STARTED.md) for the cert generation
walkthrough.

## Gateway API surface

The gateway exposes three categories of endpoints:

1. **OAuth** (`/oauth/*`) — auth flow. See [API.md#auth](./API.md).
2. **Augmentations** (`/v1/*`) — anything beyond Subsonic: sync,
   recommender, event log.
3. **Pass-through** (`/rest/*`) — proxied verbatim to Navidrome with
   the upstream credentials and the L2/L4 cache layered in front.

Pass-through is what lets clients keep using existing Subsonic SDK
shapes without us reimplementing the full API surface.

See [API.md](./API.md) for the endpoint reference.

## Recommender pipeline

Backend-only. Two indices, blended at query time — they capture
different things:

- **Content embeddings** (CLAP) — computed once per track at ingest.
  Captures "these tracks sound similar." Bonus: text-aligned, so
  natural-language queries ("rainy sunday afternoon") work via the
  same index.
- **Behavioural embeddings** (track2vec on listening sessions) —
  retrained nightly. Captures "this user plays these together," which
  can diverge from acoustic similarity. **Not yet implemented**;
  the event log feeds it.

Both will eventually be stored as mmap'd HNSW files via
[`usearch`](https://github.com/unum-cloud/usearch). The content index
is live; the behavioural index lands in a future phase.

Inference runs in a Python sidecar (FastAPI + LAION CLAP) so the
gateway stays lightweight. Boot probe: gateway checks the embedder's
`/healthz` at startup. If unreachable, it logs a warning and runs in
**degraded mode** — recommend endpoints fall back to tag-only
similarity.

```
New track discovered (Subsonic poll)
  → fetch first ~120 s of audio from Navidrome (range request)
  → POST /embed/audio to the sidecar
  → sidecar returns L2-normalized 512-dim float32 vector
  → upsert into content-ANN index (mmap'd HNSW, cosine)
  → mark track ready in catalog
```

Ingest is single-worker for now. Crash recovery is built in: any rows
flagged `in_progress` at startup get reset to `not_started` so the
worker re-attempts. The ANN is a derived cache — it's rebuildable
from SQLite, so you can wipe the file freely.

See [components/music-recommend.md](./components/music-recommend.md)
and [components/embedder.md](./components/embedder.md).

## Event log

`POST /v1/events` is append-only. Captures user-interaction signal
(scrobble, skip, like, seek) for the behavioural index. Events are
persisted to `gateway-state.recommend.sqlite` but **not yet
consumed** — the recommender will read them when the behavioural
index lands.

Two timestamps per event:
- `occurred_at` — client-supplied unix ms (when the user did the thing)
- `received_at` — gateway-side unix ms (when we persisted)

The diff is clock skew + queue delay. Clients are expected to coalesce
events into batches every few seconds (or on app background) and POST
them in one go.

## Responsiveness budget

Targets:
- Input → feedback: p99 < 50 ms
- Start of playback: < 200 ms warm-cached, < 1 s cold

Tactics that load-bear:

1. **Local-first metadata.** Browse/search reads from the SQLite
   cache; network is the refresh path, never the read path. Search
   latency = SQLite FTS5 query time (will land with full-text search
   in a later phase).
2. **Optimistic UI.** Likes, queue reorders, skips apply locally
   before server ack. The sync layer reconciles; rollback with a
   toast on rejection.
3. **Predictive prefetch.** When a track starts, the gateway already
   knows the probable next 3 (recommender output). Speculatively
   prefetch first 256 KB into L3.
4. **Gapless playback.** Decoder pipeline keeps the next track
   pre-rolled. Sample-accurate hand-off.
5. **Coalesced writes.** Scrobbles/seeks/likes batch to one
   `POST /v1/events` per few seconds.

## Phasing

Vertical slices, each end-to-end usable. Current phase is **P3 in
flight**, with the **P6 recommender minimum-viable scope shipped** in
parallel.

| Phase | Deliverable | Status |
|---|---|---|
| P0 | CLI + `music-core` + `music-subsonic`. Lists albums, plays a track via rodio. No cache, no gateway. | Done |
| P1 | Gateway + L2 metadata cache. ETag refresh. | Done |
| P2 | L3 audio cache, pinning, gapless playback. | Done |
| P3 | Web UI. TS/React on gateway API. OAuth login. | In progress |
| P4 | Mobile. UniFFI bindings, Compose Multiplatform, Media3. | Not started |
| P5 | WebSocket sync. Queue CRDT. Cross-device state. | Done |
| P6 | Recommender. CLAP ingest + content ANN + event log. | Minimum viable shipped |
| P6.8 | Behavioural index + nightly track2vec retrain. | Deferred |
| P6.9 | Text-query stations. | Deferred |

The full phase plan with rationale is in [`/CLAUDE.md`](../CLAUDE.md).
