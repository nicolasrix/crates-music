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

P1 complete. Workspace has `music-core`, `music-subsonic`, `music-cache`, `music-gateway`, and `music-cli`. The gateway proxies `/rest/*` to Navidrome with auth-param injection, caches catalog browse responses in SQLite (sqlx), and supports `If-None-Match` → `304`. CLI gains a `[gateway]` config block to route through the gateway with bearer auth; direct-Navidrome mode is preserved as a fallback when no `[gateway]` block is set.

P2 is next: client-side L3 audio cache + pinning + gapless playback.

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
