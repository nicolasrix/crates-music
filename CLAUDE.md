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

Inference runtime: the Python embedder sidecar (PyTorch, ROCm). The
**GPU lives on the GPU host** (RDNA4 GPU, 16 GB VRAM), which hosts
the embedder sidecar; the **gateway host (the NAS host) is CPU-only** and
reaches the sidecar over the LAN — see the deployment topology in the
status section below. CPU fallback always available — required because
ROCm coverage for newest AMD generations sometimes lags.

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
(P6.8). The event log is in place to capture signal for P6.8 when it
lands. **P6.9 (text-query stations) now done on the CLaMP 3 branch** —
`embed_text` is implemented in `Clamp3Embedder` and the gateway's
`GET /v1/recommend/station?text=…` was already wired; see the CLaMP 3
migration notes below.

**CLaMP 3 migration — in flight (`feat/clamp3-migration`).** Swapping
the content embedder from LAION CLAP (512-dim) to
[CLaMP 3](https://github.com/sanderwood/clamp3) (768-dim) for stronger
music-specific acoustic similarity. Done so far:

- Upstream inference code vendored under
  `services/embedder/embedder/_clamp3/` (slim `model.py` / `audio_io.py`
  / `feature_extractor.py` / MusicHuBERT, pinned at upstream `9016d2b`,
  `LICENSES/` + `VENDORED.md`).
- `Clamp3Embedder` backend (`embedder/clamp3_backend.py`): MERT-v1-95M
  frontend → mean-over-13-layers → BOS/EOS markers → CLaMP 3 audio
  encoder → L2-norm, producing 768-dim vectors. Selected via
  `EMBEDDER_BACKEND=clamp3` (`CLAMP3_CHECKPOINT` + `MERT_FOLDER`), behind
  the new `[clamp3]` extra. `embed_text` is wired too: xlm-roberta-base
  tokenize → MAX_TEXT_LENGTH-windowed CLaMP 3 text encoder →
  token-count-weighted mean → L2-norm, into the *same* 768-dim joint
  space as audio. Drives `GET /v1/recommend/station?text=…` (P6.9).
- **ABTT whitening** (`music-recommend/src/whitening.rs`): CLaMP 3
  embeddings are anisotropic (a narrow cone → inflated, poorly-separated
  cosines, esp. for text-query stations). All-but-the-Top fixes this:
  subtract the corpus mean, project out the top ~dim/100 principal
  directions (power-iteration + deflation, no linalg dep), renormalize.
  Fit *post-hoc* over the existing audio embeddings — **no re-embedding**.
  The transform lives in `AnnIndex` (whitens on `upsert`/`rebuild_from`/
  `query`; `None` = identity), so the ANN holds de-coned vectors and the
  single rule "whiten a raw vector exactly once on entry" holds — the only
  consumer change is `lookup_seed_vector` returning the raw SQLite row.
  Cached per-model in `embedding_whitening` (migration 0011), fit-or-load
  at boot, gated by `[recommend].whitening_enabled` (default true). Refit
  via `POST /v1/recommend/refit_whitening`.
- **Cross-modal text mean** (migration 0012, `whitening_text.rs`): ABTT is
  fit on audio, but CLaMP 3 text embeddings sit at a modality-gap offset —
  so audio-fit whitening *collapses* text station queries (verified live:
  thrash vs ballad went 3/10 → 9/10 overlap). Fix: estimate `μ_text` by
  embedding a fixed prompt corpus through the sidecar, center text queries
  by it (not the audio mean) before the shared de-coning. `Whitening` holds
  an optional `text_mean`; `AnnIndex::query_text` (station path) centers by
  it, `query`/`upsert` use the audio mean. Text-mean only affects queries,
  so attaching it needs **no ANN rebuild**. Fit lazily at boot when absent
  + the embedder is up, and on every refit. `/next` (audio→audio) is
  unaffected and clearly improved (two seeds → 0/10 overlap).
- **Station collapse was actually a tokenizer bug, not geometry (fixed
  2026-05-31, `e0e2955`).** The whitening/text-mean work above improved
  but never resolved station collapse (thrash vs ballad stuck at 7-9/10,
  some pairs 10/10 identical) because the real cause was upstream: the
  embedder image's **xlm-roberta-base tokenizer loaded a degenerate
  5-token vocab** (specials only) — every word tokenized to `<unk>`, so
  "death metal" and "smooth jazz" produced identical token streams and
  identical embeddings. Two packaging gaps: `sentencepiece` missing from
  the `[clamp3]` extra *and* the explicit `RUN pip install` layers (those
  images use `pip install --no-deps .`, so the extra alone never lands),
  and the Dockerfile HF prebake fetched `AutoModel` but never
  `AutoTokenizer` (runtime is `TRANSFORMERS_OFFLINE=1`). Fix adds
  sentencepiece to both, prebakes the tokenizer, a build-time assert, and
  a fail-loud runtime guard in `Clamp3Embedder.__init__`. Audio was never
  affected (no tokenizer) — only text stations. Verified live after an
  embedder rebuild + `text_mean` refit (no track re-embedding, text is
  query-time): every collapsing pair → 0/10 overlap, results genre-coherent
  (e.g. "smooth jazz" → Jazz×5/Funk×2; "boom bap hip hop" → Rap/Hip-Hop
  ×8), `/next` unchanged. NB: with the tokenizer fixed, station collapse
  disappears even with **no** whitening on the text path; the audio-fit
  ABTT de-coning applied to text is marginally *worse* than not de-coning
  (offline genre purity 0.44 vs ~0.57) — a possible follow-up to route the
  station query path around de-coning. The audio whitening that drives
  `/next` stays as-is.
- Dim is now a per-backend property (not an app constant);
  `EMBEDDER_STUB_DIM` lets the stub mimic the 768 wire shape in dev.
- Deployment: `docker/embedder/Dockerfile.clamp3` (CPU) bakes MERT +
  xlm-roberta-base into the HF cache; `docker-compose.clamp3.yml` flips
  the gateway to 768 via `gen_config.py`'s new optional `[recommend]`
  section. GPU twins `docker/embedder/Dockerfile.clamp3-rocm` +
  `docker-compose.clamp3-rocm.yml` (rocm6.4 wheels, MIOpen kernel cache,
  HF prebake) — the split-host shape, since the embedder runs on the
  GPU host's GPU (RDNA4) and the gateway reaches it over the LAN.
- **GPU image built + smoke-tested + swapped live (2026-05-30).** Real
  forward pass on RDNA4 verified end-to-end: `/healthz` →
  `dim:768, device:cuda`, saas `state_dict` aligns, output L2-normed
  (norm=1.0), warm ~230 ms/clip (cold first call ~5 s = MIOpen JIT,
  then cached). The GPU box's `:9000` embedder is now CLaMP 3 (was CLAP),
  same container name + port + shared bearer token.

**Deployment topology** (optionally split across two hosts):

- **the NAS host** is the **gateway host** and is **CPU-only** (no GPU). It
  runs `crates-gateway` (the live recommender consumer) + `crates-caddy`.
  Its `EMBEDDER_URL` dials the GPU host's GPU sidecar over the LAN.
  the NAS host's own `crates-embedder` (CLAP CPU) is vestigial — unused while
  `EMBEDDER_URL` points off-box.
- **The GPU host** (`192.0.2.53`) has the **AMD RDNA4
  XT** and runs the **GPU embedder sidecar** (`crates-embedder`,
  `embedder-clamp3-rocm:dev`) on `:9000`. It additionally runs a
  caddy+gateway *cert/proxy test* instance with a deliberately-dead
  embedder URL — not a recommender, ignore its health.

**Production cutover on the NAS host — DONE (2026-05-31).** Verified live:

- `crates-gateway` image rebuilt from this branch (built 2026-05-31
  00:58, baked `gen_config.py` has full `RECOMMEND_EMBEDDING_DIM`
  support); `RECOMMEND_EMBEDDING_DIM=768` set in the Custom App YAML and
  reflected in the live `gateway.toml` (`[recommend] embedding_dim =
  768`); the 512-dim ANN sidecar was wiped and rebuilt at 768 from
  SQLite (**7349 embedded tracks**). Boot log: embedder probe
  `dim=768 device=cuda`, whitening loaded (`k=7`), cross-modal text mean
  fitted + persisted, plus a post-tokenizer-fix `refit_whitening`.
- The GPU sidecar on the GPU host was rebuilt with the tokenizer fix
  (`embedder-clamp3-rocm:dev`, built 2026-05-31 01:44): `sentencepiece`
  present, full xlm-roberta vocab (`vocab_size=250002`, distinct token
  streams per genre, 0 `<unk>`), `dim=768 device=cuda`.

Cutover mechanics for reference (e.g. future model bumps): (1) rebuild
`crates-music/gateway:dev` from the branch and ship to the NAS host
(`scripts/ship-image.sh … nas-host`, `REMOTE_DOCKER="sudo docker"`);
(2) set `RECOMMEND_EMBEDDING_DIM` in the NAS Custom App YAML;
(3) wipe the old-dim ANN sidecar (`gateway-state.ann` + `.ann.keys`) —
a dim change is non-migratable; (4) Save/restart; (5) re-embed via
`scripts/enqueue_all_tracks.py` — recommender runs degraded until the
GPU drains the queue. The cached ABTT whitening (`embedding_whitening`
table) does **not** need a manual wipe — `load_or_fit_whitening` detects
a stale-dim cached row at boot, logs a warning, and refits from the
corpus automatically.

**Diagnostics surface (M2.1 + M2.2 + M3) done.** Authenticated
endpoints read the M0 trace store and the new client-events ring,
plus a web `/diagnostics` page that renders them and a browser RUM
emitter that feeds it.

Endpoints:

- `GET /v1/diagnostics/traces?limit=&name=&since_ms=` — recent closed
  spans, newest first. `fields_json` is parsed back to a JSON object;
  parse failure surfaces under `_raw`. Limit clamped server-side to
  1000.
- `GET /v1/diagnostics/histogram?since_ms=` — per-name duration
  histogram. SQL aggregates `count/min/max/sum`; quantiles computed
  in Rust via nearest-rank on the sorted slice (sub-millisecond at
  the 100k-row ring cap).
- `GET /v1/diagnostics/queue_depth?model_version=` — embedding ingest
  queue counts. Defaults to `recommend_model_version`; explicit
  override useful during rolling model upgrades.
- `POST /v1/diagnostics/client_events` — browser RUM batch upload.
  Body: `{events: [{session_id, occurred_ms, name, value_ms?,
  rating?, page_path, fields?}, ...]}`. Server stamps `received_ms`
  + `user_agent` (truncated to 256 chars). 422 on schema mismatch,
  413 if `events.len() > 50`. Returns `{accepted: N}`.
- `GET /v1/diagnostics/client_events?limit=&name=` — most recent
  events, newest received first. Two timestamps preserved: client's
  `occurred_ms` and gateway-stamped `received_ms`.

Browser RUM (`apps/web/src/rum/`):

- `web-vitals` 4.x — LCP / INP / CLS / FCP / TTFB → marks named
  `web-vital.<NAME>` with the library's `rating` bucket attached.
- `markEvent(name, {value_ms?, rating?, fields?})` — public API for
  custom marks. Already wired from `PlayerContext`: emits
  `playback.start` with `{value_ms, fields:{track_id}}` measured
  from `src` set → first `playing` event (the user-perceived
  latency, not `loadedmetadata` which fires too early).
- Batched: 10s interval flush via `fetch`, plus pagehide /
  visibilitychange→hidden flush via `fetch(..., {keepalive:true})`.
  In-memory cap of 50 events drops oldest. Session id is one per
  page-load, persisted in `sessionStorage`.

Web (`apps/web/src/pages/Diagnostics.tsx`):

- Four sections — queue depth (count tiles), histogram (table with
  inline distribution bars at p50/p95/p99/max), client events (RUM
  table with rating pill + page_path + truncated session id), traces
  (grouped by trace_id, expandable with a top-down waterfall colored
  by stable hash of span name).
- 5-second `refetchInterval` via TanStack Query keeps the page live
  without a websocket. Filterable by span name (dropdown sourced
  from the histogram response).
- Reachable via a `/diagnostics` nav link in the header. Type-checks
  and builds (~80 KB gzipped after web-vitals + RUM glue, +4 KB
  over the M2 baseline).
- *Browser verification pending* — the page compiles and builds but
  has not been clicked through end-to-end against a running gateway.

Diagnostics SQLite layout (`gateway-state.traces.sqlite`):

- `spans` table — closed `tracing` spans, ring-trimmed by the
  drainer (M0).
- `client_events` table — browser RUM ring with two timestamps
  (`occurred_ms` from client, `received_ms` stamped server-side)
  and an indexed `session_id` for grouping. No automatic trimming
  yet; the table grows. **Follow-up:** mirror the `spans` ring trim
  policy when the table starts mattering for disk usage.

P1/P2 still hold: gateway + L2 metadata cache + ETag refresh, audio
cache + pinning + gapless CLI playback.

**Web PWA + offline playback (in flight, `feat/web-pwa-offline`).**
Brings the CLI's L3 audio cache + pinning to the web client and makes the
SPA an installable, offline-capable PWA:

- `apps/web/src/cache/` — an IndexedDB reimplementation of the
  `crates/music-cache` contract (content-addressed `(trackId, bitrate,
  codec)`; two-budget LRU: a regular auto-cached budget + a separate
  never-evicted pinned budget; `put/get/touch/pin/unpin/listPinned/stats/
  evict`). Audio is stored as whole-file blobs and served to `<audio>` via
  `URL.createObjectURL` — chosen over a Service-Worker + Cache-API approach
  because the gateway stream endpoint has **no HTTP Range support**, so the
  browser must seek locally against a stored file. 15 vitest unit tests
  (`audioCache.test.ts`, fake-indexeddb).
- `AudioCacheContext` bridges cache↔playback. An in-memory
  `trackId → blob:URL` map, warmed for the queue window, lets the
  gesture-critical `primePlayback` resolve a src **synchronously**; the
  natural-advance effect (no live gesture) awaits the cache and prefers the
  local blob. Played tracks are auto-cached (regular budget); "save for
  offline" pins (pinned budget) — mirrors CLI semantics.
- UI: per-track "save for offline" in the row menu, bulk "download
  album/playlist" buttons, a `/downloads` page (stats + pinned list + "free
  up space" = evict), budget sliders in Settings (lowering evicts
  immediately), and a topbar offline indicator.
- PWA: `vite-plugin-pwa` (`registerType:autoUpdate`) precaches the app
  shell so it boots with no network; `/v1`, `/rest`, `/oauth` are
  NetworkOnly and audio never touches the SW. Manifest + maskable SVG icon
  make it installable. The gateway already serves `sw.js` /
  `manifest.webmanifest` from the static dir root (no gateway change).
  `autoUpdate` also fixes the stale-bundle white-screen seen on deploys.
- **Not yet browser-verified end-to-end** (offline airplane-mode launch +
  play) — needs a real device against the gateway; install requires the
  mkcert CA trusted on the device. Deferred: transcode-to-fit caching
  (opus@128 to pack the ~500 MB budget; needs gateway stream-param wiring).

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

### Microbenchmarks (`cargo bench`)

Three Criterion bench suites cover the hot paths surfaced by the
diagnostics work. They are *regression detectors*, not load tests —
the goal is "did this PR make X slower than the baseline?", not "what
is our peak QPS?". For end-to-end load, use the trace store (M0) on a
running gateway.

| Bench | Crate | Measures |
|---|---|---|
| `server_timing` | `music-recommend` | `parse_server_timing` per-call cost across realistic header shapes |
| `ann` | `music-recommend` | `AnnIndex::upsert` (fresh-insert curve) and `query` (top-10 latency) at N=100/1000/5000 |
| `embedder_client` | `music-recommend` | `EmbedderClient::embed_audio`/`embed_text` against a wiremock fake — HTTP roundtrip + JSON parse + Server-Timing extraction. *Not inference cost* — that's measured Python-side (see below) |
| `trace_store` | `music-gateway` | `insert_batch` at batch=1/50/200 with 10k pre-existing rows; `trim_to_capacity` no-op vs over-budget |

Run all benches:

```bash
cargo bench --workspace
```

Run one suite (faster feedback during work on a specific module):

```bash
cargo bench --bench ann -p music-recommend
```

Quick mode (≈10× faster, less statistical confidence — useful while
iterating, never for "is this PR a regression?" verdicts):

```bash
cargo bench --bench server_timing -p music-recommend -- --quick
```

HTML reports land at `target/criterion/<group>/report/index.html`.
Criterion remembers the previous run automatically and prints a
`change: [+X% -Y%] (p = …)` line on the next run — that's the
regression signal.

Approximate baselines on a Ryzen-class dev machine (for sanity
checks; do not commit hardware-specific numbers as gates):

- `parse_server_timing` (3 stages): ~115 ns
- `ann_query_top10` at N=5000: ~100 µs
- `ann_upsert_from_empty` at N=5000: ~1.1 s (≈225 µs/insert at the high end)
- `embedder_client_embed_audio` 1 KB: ~27 µs; 1 MB: ~390 µs
- `embedder_client_embed_text` 16 chars: ~26 µs; 16 KB: ~33 µs
- `trace_store_insert_batch` at batch=50, table=10k: ~250 µs
- `trace_store_trim` no-op: ~7 µs

### Python embedder benchmarks (`pytest -m benchmark`)

Two pytest-benchmark suites in `services/embedder/tests/`:

- `test_benchmarks.py` — stub backend (SHA-256 + numpy PRNG). Always runs.
- `test_benchmarks_clap.py` — real CLAP inference. Skipped unless the
  `clap` extra is installed *and* `CLAP_CHECKPOINT` points at an
  on-disk checkpoint.

Run only the benchmarks (regular `pytest` excludes them via the
`benchmark` mark):

```bash
cd services/embedder
uv run --extra dev pytest -m benchmark
```

Compare against a previous run (auto-saves to `.benchmarks/`):

```bash
uv run --extra dev pytest -m benchmark --benchmark-autosave
uv run --extra dev pytest -m benchmark --benchmark-compare
```

Approximate stub baselines (will vary by CPU):

- `test_stub_embed_text[short]`: ~10 µs (SHA-256 dominates)
- `test_stub_embed_audio[1mb]`: ~440 µs (linear in input bytes)
- `test_stub_embed_audio[5mb]`: ~2.1 ms

CLAP baselines depend on hardware and which device is active
(check `/healthz` `device` field). Run the suite once after a fresh
`uv sync --extra clap` to capture a baseline before changing the
preprocessing pipeline or upgrading torch.
