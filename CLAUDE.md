# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project goal

Build a music player for a self-hosted [Navidrome](https://www.navidrome.org/) backend, served to three clients: **CLI**, **web UI**, and **Android** (via Compose Multiplatform; iOS may follow). Single-user, local-network-first.

Navidrome speaks the **Subsonic API** (with OpenSubsonic extensions). Treat the [Subsonic](http://www.subsonic.org/pages/api.jsp) / [OpenSubsonic](https://opensubsonic.netlify.app/) spec as the source of truth for endpoint shapes and error codes. We do not fork Navidrome; it remains the catalog source of truth.

## Architecture

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
                    │  • Recommender (CLAP + track2vec)│
                    │  • Event log + WebSocket sync    │
                    │  • SQLite (gateway state)        │
                    └──┬──────────────┬─────────────┬──┘
                       │              │             │
                ┌──────▼─────┐ ┌──────▼─────┐ ┌─────▼─────┐
                │  CLI       │ │  Web       │ │  Mobile   │
                │  (Rust)    │ │  (TS+React │ │  (Compose │
                │            │ │   + WASM)  │ │   MP+KMP) │
                └────────────┘ └────────────┘ └───────────┘
                       └──────────────┴─────────────┘
                              shared Rust core (UniFFI / WASM)
```

**Key shape:** thin clients, gateway holds anything that benefits from being shared (cache, recommender, sync). A **shared Rust core** is consumed by the CLI directly, by the web as WASM, and by the mobile app as Kotlin via [UniFFI](https://mozilla.github.io/uniffi-rs/) bindings. UI is per-platform: native CLI, React for web, Compose for mobile.

## Repo layout (Cargo workspace monorepo)

```
crates/
  music-core/        # domain types (Track, Album, Queue, PlaybackState)
  music-subsonic/    # typed Subsonic / OpenSubsonic client (reqwest)
  music-cache/       # SQLite metadata cache + on-disk LRU audio cache
  music-player/      # native playback (rodio + symphonia)
  music-sync/        # WebSocket client, optimistic state, queue merge
  music-recommend/   # SERVER-ONLY: CLAP embedder, ANN index, track2vec
  music-gateway/     # the gateway binary
  music-cli/         # the CLI binary
  music-ffi/         # UniFFI bindings consumed by the mobile app

apps/
  web/               # TS + React + WASM-compiled core
  mobile/            # Compose Multiplatform, Android first
```

`music-ffi` exposes `music-cache`, `music-sync`, and `music-subsonic` to Kotlin — **not** `music-player`, since mobile playback uses Media3 directly. Web playback uses MediaSource Extensions (MSE), not symphonia-in-WASM.

## Caching (multi-layer)

| Layer | Where | Stores | Eviction |
|---|---|---|---|
| L1 — UI state | RAM, each client | Currently-displayed views | LRU, small |
| L2 — Metadata | SQLite, each client | Tracks/albums/artists/playlists | TTL + ETag refresh |
| L3 — Audio | Disk, each client | Encoded audio files | LRU by bytes; **pinned tracks have a separate budget and are never LRU-evicted** |
| L4 — Transcoded | Disk, gateway | Pre-transcoded variants | LRU by bytes |

Two strict invariants:

1. **Audio cache is content-addressed** by `(trackId, bitrate, codec)`, never by URL.
2. **Metadata cache uses ETags**, not just TTLs — `If-None-Match` is cheaper than either staleness or full refetch.

Cache budgets are user-configurable per client (sensible defaults: ~2 GB Android, ~500 MB web, ~10 GB CLI). Lowering the cap evicts immediately and visibly, not lazily.

## Recommender

Backend-only. Two indices, blended at query time — they capture different things:

- **Content embeddings** (CLAP, [LAION-AI/CLAP](https://github.com/LAION-AI/CLAP)) — computed once per track at ingest. Captures "these tracks sound similar." Bonus: text-aligned, so natural-language queries ("rainy sunday afternoon") work via the same index.
- **Behavioural embeddings** (track2vec on listening sessions) — retrained nightly. Captures "this user plays these together," which can diverge from acoustic similarity.

Both stored as **mmap'd HNSW files** via [`usearch`](https://github.com/unum-cloud/usearch) or [`hnsw_rs`](https://crates.io/crates/hnsw_rs). At our scale (single user, ~10⁴ tracks) an in-process index is sufficient — no separate vector DB.

Inference runtime: **ONNX Runtime via the [`ort`](https://crates.io/crates/ort) crate**, ROCm execution provider (gateway has an AMD RDNA4, 16 GB VRAM). CPU fallback always available — required because ROCm coverage for newest AMD generations sometimes lags.

### Ingest pipeline

```
New track discovered (Subsonic poll)
  → fetch first ~120s of audio from Navidrome (range request)
  → decode + resample to CLAP's expected rate (symphonia + rubato)
  → CLAP forward pass (ort, batched 32–64)
  → upsert into content-ANN index (mmap'd HNSW)
  → also extract: BPM, key, duration  ← cheap; exposed as filterable metadata
  → mark track ready in catalog
```

Ingest runs in a background queue at low priority. Recommender works in **degraded mode** (tag-only similarity) for not-yet-embedded tracks. Worker is resumable; embeddings are content-addressed by `(track_id, model_version)` so retries are idempotent.

## Auth (OAuth 2.1, self-hosted in gateway)

Single-user means OAuth does the job of *device pairing + token rotation*, not user identification.

- **Bootstrap:** gateway prints a one-time setup URL on first run; user sets a master password.
- **Web:** Authorization Code + PKCE.
- **CLI:** [Device Authorization Grant (RFC 8628)](https://datatracker.ietf.org/doc/html/rfc8628). CLI prints a code; user confirms in browser.
- **Mobile:** Authorization Code + PKCE via system browser (Custom Tabs).
- Per-device refresh tokens; revocable individually from a "Devices" page.

Library: [`oxide-auth`](https://github.com/HeroicKatora/oxide-auth) or hand-rolled (surface is small for a single user).

## TLS

Local network only. Gateway uses **[mkcert](https://github.com/FiloSottile/mkcert)** for local certs:

- `mkcert -install` once on each device installs the local CA (the dev's machine, the phone, etc.).
- Gateway gets a cert for `gateway.local` (or whatever hostname is chosen).
- Annual cert regeneration; not automatic.

Plain HTTP is **not** acceptable — Service Workers, `navigator.storage.persist()`, MSE, and mobile system-browser OAuth all require a secure context, and `localhost` is the only non-HTTPS origin browsers treat as secure (which doesn't help phone-to-gateway access).

## Gateway API surface

- **Pass-through:** anything under `/rest/*` we don't override forwards to Navidrome verbatim. Lets clients use existing Subsonic SDK shapes.
- **Augmentations** (versioned under `/v1/`):
  - `GET /v1/recommend/next?seed=<trackId>&n=20`
  - `GET /v1/recommend/station?seed=…|text=…`  ← text query uses CLAP's text encoder
  - `POST /v1/events` (scrobble, skip, like, seek — feeds the recommender; **batched**, not per-event)
  - `GET /v1/sync/snapshot` + `WS /v1/sync` (queue + playback state across devices)
  - `GET /v1/stream/<trackId>?bitrate=…` — wraps Navidrome's `/rest/stream` with the L4 cache

HTTP/2 throughout. Audio uses HTTP range requests. Events are coalesced client-side into batches every few seconds or on app background.

## Responsiveness budget

Targets: **input → feedback p99 < 50 ms**, **start-of-playback < 200 ms warm-cached, < 1 s cold**.

Tactics that load-bear on this:

1. **Local-first metadata.** Browse/search reads from the SQLite cache; network is the refresh path, never the read path. Search latency = SQLite FTS5 query time.
2. **Optimistic UI.** Likes, queue reorders, skips apply locally before server ack. Sync layer reconciles; rollback with a toast on rejection. **Never block on network for a UI gesture.**
3. **Predictive prefetch.** When a track starts, gateway already knows the probable next 3 (recommender output). Speculatively prefetch first 256 KB into L3.
4. **Gapless playback.** Decoder pipeline keeps next track pre-rolled. Sample-accurate hand-off.
5. **Coalesced writes.** Scrobbles/seeks/likes batch to one `POST /v1/events` per few seconds.

## Phasing

Vertical slices, each end-to-end usable:

| Phase | Deliverable |
|---|---|
| **P0** | CLI + `music-core` + `music-subsonic`. Lists albums, plays a track via rodio. No cache, no gateway. |
| **P1** | Gateway + L2 metadata cache. ETag refresh. CLI uses gateway. |
| **P2** | L3 audio cache, pinning, gapless playback. Single-client offline-capable. |
| **P3** | Web UI. TS/React on gateway API, WASM core for caching. MSE playback. |
| **P4** | Mobile. UniFFI bindings, Compose Multiplatform, Media3 + foreground service for background playback. |
| **P5** | WebSocket sync. Queue CRDT. Cross-device state. |
| **P6** | Recommender. CLAP ingest pipeline + content ANN. Behavioural index + nightly retrain. Text-query stations. |

## Status

For comprehensive onboarding documentation (architecture, getting
started, per-component reference, API), see [`docs/`](./docs/). This
section is the high-level "what's done, what isn't" view.

**P3 in flight.** OAuth 2.1 server complete (P3.1):

- Hand-rolled, single-tenant. Five tables in `gateway-state.sqlite` —
  `users` (Argon2id master password), `oauth_clients`, `auth_codes`,
  `refresh_tokens`, `access_tokens`. Plus a `sessions` table for the
  browser login flow.
- Endpoints: `POST /oauth/setup` (one-shot bootstrap), `GET/POST
  /oauth/login`, `GET /oauth/authorize` (Authorization Code + PKCE
  S256), `POST /oauth/token` (code grant + refresh-token rotation),
  `POST /oauth/revoke` (RFC 7009).
- `require_bearer` accepts OAuth-issued access tokens (sha256 lookup)
  in addition to the legacy static config bearer. Token can come via
  `Authorization: Bearer …` *or* `?access_token=…` (RFC 6750 §2.3 —
  needed for `<audio>` and `<img>` URLs that can't set headers).
- Pre-declared `[[oauth.clients]]` config blocks register at startup.

**Web app (P3.2/P3.3) — functionally complete, no automated UI tests
yet.** Vite + React 19 + TanStack Query + Tailwind in `apps/web/`.
Implements the full PKCE flow against the gateway, an albums list
(newest 60), an album detail with track list, a bottom-of-screen
player using a plain `<audio>` element, and the sync provider wiring
in P5's WebSocket fan-out. Type-checks and builds (~74 KB JS gzipped).
The dev server proxies `/oauth`, `/rest`, `/v1` to the gateway over
HTTPS — see `apps/web/vite.config.ts`.

**Descoped from P3:** the WASM core. Its primary purpose was a
client-side L2 metadata cache for browse-while-offline; that was
already deferred at P2, so the WASM module would have nothing to do.
Web client uses TanStack Query for caching against the gateway.

**Deferred to a later phase:** MSE-based gapless web playback
(in-scope only if the basic `<audio>` boundary handoff is audibly
gappy in real use), client-side L2 metadata cache for offline browse,
Vitest/Playwright suites for the web app.

**P5 done.** WebSocket sync — `crates/music-sync` is the pure state
machine; gateway hosts `/v1/sync/snapshot` (HTTP), `/v1/sync/ops`
(POST), and `/v1/sync` (WS fan-out). CLI has `music sync state|push|watch`;
web has the optimistic-update sync provider. Single-linearizer model
(no CRDTs) — gateway sequences all ops and broadcasts.

**P6 minimum viable done.** Recommender stack:

- `music-recommend` crate: SQLite-backed embedding store
  (content-addressed by `(track_id, model_version)`), ingest queue
  (status column doubles as the queue), append-only event log.
- `usearch` HNSW with cosine metric, persisted alongside a JSON
  sidecar for the `(TrackId ↔ u64)` map. ANN is a derived cache —
  rebuildable from SQLite at boot.
- Python sidecar (`services/embedder/`): FastAPI + LAION CLAP for
  audio + text embeddings. Stub backend for tests / dev. ROCm GPU
  inference enabled — `pyproject.toml` routes torch through PyTorch's
  ROCm wheel index; `/healthz` reports `device: "cuda" | "cpu"` so
  silent CPU fallback is visible. ~6× wall-clock speedup over CPU on a
  RDNA4 (5-track drain: 31 s CPU → 5 s GPU; pipeline now
  Subsonic-bound, not compute-bound).
- Gateway endpoints: `GET /v1/recommend/next`, `POST /v1/recommend/enqueue`,
  `POST /v1/events`. Boot-time embedder probe with degraded-mode
  fallback if unreachable.
- Storage: `gateway-state.recommend.sqlite` (separate file from OAuth
  state — sqlx tracks migrations per pool). ANN at `gateway-state.ann`
  + `.ann.keys` sidecar.

**Deferred from P6:** behavioural index + nightly track2vec retrain
(P6.8), text-query stations via CLAP's text encoder (P6.9). The event
log is in place to capture signal for P6.8 when it lands.

P1/P2 still hold: gateway + L2 metadata cache + ETag refresh, audio
cache + pinning + gapless CLI playback.

### Running the gateway locally

```
# 1. Generate TLS cert (one-time)
./scripts/dev-certs.sh

# 2. Write a gateway config — see crates/music-gateway/tests for shape

# 3. Start the gateway
cargo run -p music-gateway -- --config /path/to/gateway.toml

# 4. Configure CLI to use it (~/.config/crates-music/config.toml):
[server]
url = "http://nav.lan:4533"
username = "alice"
password = "wonderland"

[gateway]
url = "https://gateway.local:8443"
bearer_token = "<the same token in gateway.toml>"
```

`[server]` creds are kept so you can flip between gateway and direct mode without rewriting them. Add a `gateway.local → <gateway-ip>` entry to `/etc/hosts` on each client device, or run mDNS.

### Web client setup

```toml
# Add to gateway.toml
[[oauth.clients]]
client_id = "web"
name = "Web"
redirect_uris = [
    "http://localhost:5173/oauth/callback",      # Vite dev
    "https://gateway.local:8443/oauth/callback", # production same-origin
]
```

Run the dev stack:

```
# 1. Gateway (Rust, TLS + OAuth + cache + Subsonic proxy)
cargo run -p music-gateway -- --config /path/to/gateway.toml

# 2. Web app (Vite, http://localhost:5173)
cd apps/web && npm install && npm run dev
```

On first run, the gateway logs a one-time setup URL; visit it,
choose a master password, then the sign-in button on the web app
will work.

### Embedder sidecar (optional, for recommendations)

The recommender requires the Python sidecar. Without it, the gateway
boots in degraded mode and `/v1/recommend/next` returns 404 for every
seed. Stub backend works for local dev — no GPU, no PyTorch:

```bash
cd services/embedder
uv sync                # or: pip install -e '.[dev]'
uv run uvicorn embedder.app:app --port 9000
```

For real CLAP inference (production):

```bash
uv sync --extra clap   # pulls torch + laion-clap + librosa
EMBEDDER_BACKEND=clap CLAP_CHECKPOINT=/path/to/clap.pt \
  uv run uvicorn embedder.app:app --port 9000
```

Then add to `gateway.toml`:

```toml
[embedder]
url = "http://localhost:9000"
timeout_seconds = 30        # default; CLAP on CPU can take 10+ s
```

Restart the gateway; you should see
`embedder: probe ok model=… dim=512` in the logs. If the sidecar is
unreachable at boot, the gateway logs a warning and continues without
it (no auto-retry — restart the gateway after starting the sidecar).

### Audio cache (`[cache]` block, optional)

```
[cache]
# path = "/var/cache/crates-music/audio"   # default: $XDG_CACHE_HOME/crates-music/audio
regular_budget_bytes = 10737418240          # 10 GB — LRU-evicted
pinned_budget_bytes  = 5368709120           # 5 GB  — never LRU-evicted
```

Inspect with `music cache stats`. Force a fit-to-budget eviction with
`music cache evict`. Pin tracks with `music pin <id>` (auto-fetches if
not yet cached); see them with `music pinned`.
