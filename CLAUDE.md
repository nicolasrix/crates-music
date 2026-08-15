# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project goal

Build a music player for a self-hosted [Navidrome](https://www.navidrome.org/) backend, served to three clients: **CLI**, **web UI**, and **mobile**. Local-network-first, with a lightweight three-role model (admin / user / guest) layered on the gateway's hand-rolled OAuth — see "Multi-user & roles" in Status. Mobile is the **installable PWA** (the web client, added to the home screen), not a native app — see "Mobile is the PWA" below.

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
                    │  • ETag metadata cache (L2)      │
                    │  • OAuth 2.1 server              │
                    │  • Recommender (CLaMP 3 + ANN)   │
                    │  • Event log + WebSocket sync    │
                    │  • SQLite (gateway state)        │
                    └──────┬───────────────────┬───────────┘
                           │                   │
                ┌──────────▼───────┐ ┌─────────▼───────────────────┐
                │  CLI  (Rust)     │ │  Web  (TS+React)            │
                │                  │ │  └─ installable PWA = mobile│
                └──────────────────┘ └─────────────────────────────┘
```

**Key shape:** thin clients, gateway holds anything that benefits from being shared (cache, recommender, sync). The CLI consumes the Rust crates directly; the web is a React SPA talking to the gateway over HTTP/WS. **Mobile is that same web app installed as a PWA** — no separate codebase. The original plan of a shared Rust core compiled to WASM (web) and Kotlin via UniFFI (native mobile) was **dropped**: the WASM core was descoped at P3, and native mobile (P4) was retired in favour of the PWA — see "Mobile is the PWA" below.

## Repo layout (Cargo workspace monorepo)

`ls crates/` for the crate list; each crate's `lib.rs` header says what it
does. Two things the tree does not tell you:

- `music-recommend` is **server-only** — never link it into a client.
- Cross-crate boundaries matter more than the layout: clients consume
  `music-core` types and talk to the gateway; only the gateway touches
  `music-recommend`.

> **Planned but never built:** `crates/music-ffi` (UniFFI bindings) and
> `apps/mobile` (Compose Multiplatform). Both belonged to the native-mobile
> plan (P4), which was retired — the PWA is the mobile client. They do not
> exist in the tree; references to them in older notes are historical.

Web playback uses a plain `<audio>` element (MediaSource Extensions remain a deferred option if gapless boundaries are audibly gappy). The web client's offline audio cache is a TypeScript reimplementation of the `music-cache` contract in IndexedDB (`apps/web/src/cache/`), not the Rust crate via WASM.

## Caching (multi-layer)

| Layer | Where | Stores | Eviction |
|---|---|---|---|
| L1 — UI state | RAM, each client | Currently-displayed views | LRU, small |
| L2 — Metadata | SQLite, each client | Tracks/albums/artists/playlists | TTL + ETag refresh |
| L3 — Audio | Disk, each client | Encoded audio files | LRU by bytes; **pinned tracks have a separate budget and are never LRU-evicted** |
| L4 — Transcoded | Disk, gateway | Pre-transcoded variants | **Planned, never built** — audio rides the verbatim `/rest/*` proxy and Navidrome transcodes on demand (`format`/`maxBitRate`) |

Two strict invariants:

1. **Audio cache is content-addressed** by `(trackId, bitrate, codec)`, never by URL.
2. **Metadata cache uses ETags**, not just TTLs — `If-None-Match` is cheaper than either staleness or full refetch.

Cache budgets are user-configurable per client (sensible defaults: ~2 GB Android, ~500 MB web, ~10 GB CLI). Lowering the cap evicts immediately and visibly, not lazily.

## Recommender

Backend-only. Two indices, blended at query time — they capture different things:

- **Content embeddings** (CLaMP 3, [sanderwood/clamp3](https://github.com/sanderwood/clamp3), 768-dim — LAION CLAP, 512-dim, was the original and is now the legacy/fallback backend) — computed once per track at ingest. Captures "these tracks sound similar." Bonus: text-aligned, so natural-language queries ("rainy sunday afternoon") work via the same index.
- **Behavioural embeddings** (track2vec on listening sessions) — retrained nightly. Captures "this user plays these together," which can diverge from acoustic similarity.

Both stored as **mmap'd HNSW files** via [`usearch`](https://github.com/unum-cloud/usearch) or [`hnsw_rs`](https://crates.io/crates/hnsw_rs). At our scale (a household sharing one ~10⁴-track catalog) an in-process index is sufficient — no separate vector DB.

Inference runtime: the Python embedder sidecar (PyTorch, ROCm). The
**GPU lives on the embedder host** (an RDNA4-class card, 16 GB VRAM), which hosts
the embedder sidecar; the **gateway host can be CPU-only** and
reaches the sidecar over the LAN — see the deployment topology in the
status section below. CPU fallback always available — required because
ROCm coverage for newest AMD generations sometimes lags.

### Ingest pipeline

```
New track discovered (Subsonic poll)
  → fetch first ~120s of audio from Navidrome (range request)
  → decode + resample to the model's expected rate (symphonia + rubato)
  → embed via the Python sidecar over HTTP (CLaMP 3 / MERT, PyTorch — not ort)
  → upsert into content-ANN index (mmap'd HNSW)
  → also extract: BPM, key, duration  ← cheap; exposed as filterable metadata
  → mark track ready in catalog
```

Ingest runs in a background queue at low priority. Recommender works in **degraded mode** (tag-only similarity) for not-yet-embedded tracks. Worker is resumable; embeddings are content-addressed by `(track_id, model_version)` so retries are idempotent.

The "new track discovered" step is `crates/music-gateway/src/discovery.rs`
(`[discovery]` config). It is **stateless** — it re-offers catalog ids to
`EmbeddingStore::enqueue_many` and lets `INSERT OR IGNORE` decide what's
new, so there's no cursor to persist. Two tiers, because Navidrome's
`newest` ordering keys on *album* creation: a recent scan
(`getAlbumList2?type=newest` + `getAlbum`, every 5 min) catches new
albums, and a full `search3` sweep (boot + daily) catches tracks added to
pre-existing albums and self-seeds a fresh install. `POST
/v1/admin/discovery/scan` forces a sweep. This is why
`scripts/enqueue_all_tracks.py` is now a fallback, not the normal path.

## Auth (OAuth 2.1, self-hosted in gateway)

OAuth does device pairing + token rotation **and** carries identity:
every authenticated request resolves to a `Principal { user_id, role,
host_user_id }` injected by `require_bearer`. Roles are **admin / user /
guest** (see "Multi-user & roles" in Status). The legacy static bearer
and any NULL-`user_id` token resolve to the owner (`id=1`, admin), so the
identity layer is backward-compatible.

- **Bootstrap:** gateway prints a one-time setup URL on first run; the owner sets a master password (becomes `id=1`, role `admin`).
- **Web / PWA (mobile):** Authorization Code + PKCE, with a `username` field at login. Mobile is the installed PWA, so it uses the same browser-based flow — no separate native/Custom-Tabs path.
- **CLI:** [Device Authorization Grant (RFC 8628)](https://datatracker.ietf.org/doc/html/rfc8628). CLI prints a code; user confirms in browser.
- **Guests:** redeem a shared code via `POST /oauth/guest` (no password, no PKCE) → an ephemeral, expiring guest principal attached to its host's room.
- Per-device refresh tokens; revocable individually from a "Devices" page.
- **Authorization tiers:** any-authenticated (browse/play/recommend reads/room control) · write-capable admin+user (playlists, persisted ratings/taste) · admin-only (`/v1/admin/*`, `/v1/diagnostics/*`, recommender maintenance). Guests are 403'd on write/admin tiers and their taste signal is dropped from training.
- **Account recovery (no email):** an admin resets a user's password (`POST /v1/admin/users/:id/password`); the *owner's* master password is reset out-of-band on the gateway host (`music-gateway … reset-master-password`). Both rewrite the Argon2 hash on `users.id=1` in place — never delete/recreate the row.

Library: hand-rolled (the surface is small; piggybacking identity on the existing opaque-token store preserves instant revocation + the device-grant flow — see "Multi-user & roles").

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
  - `GET /v1/recommend/station?seed=…|text=…`  ← text query uses the embedder's text encoder (CLaMP 3)
  - `POST /v1/events` (scrobble, skip, like, seek — feeds the recommender; **batched**, not per-event)
  - `PUT /v1/library/rating` + `GET /v1/library/ratings` (per-track/album/artist like/dislike — gateway-owned, no Navidrome writeback)
  - `GET /v1/sync/snapshot` + `POST /v1/sync/ops` + `WS /v1/sync` (queue + playback state across devices)

The actual `/v1` surface is much larger than this short-list — there are
also `/v1/recommend/{from-seeds,from-any,similar_albums,similar_artists,
refit_whitening,feedback}`, a `/v1/diagnostics/*` family (traces,
histogram, queue_depth, client_events, and a dozen `recommend/*`
inspectors), and `/v1/admin/cache/*`. The router in
`crates/music-gateway/src/app.rs` is the source of truth; see
[`docs/`](./docs/) for the full reference.

**No audio `/v1` route exists.** The originally-planned
`GET /v1/stream/<trackId>` + L4 transcoded cache was **never built** —
audio rides the verbatim `/rest/*` proxy and Navidrome transcodes on
demand (`format`/`maxBitRate`).

HTTP/2 throughout. Events are coalesced client-side into batches every few seconds or on app background.

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
| ~~**P4**~~ | ~~Native mobile (UniFFI + Compose Multiplatform + Media3).~~ **Retired** — mobile is the installable PWA (the P3 web client). See "Mobile is the PWA". |
| **P5** | WebSocket sync. Queue CRDT. Cross-device state. |
| **P6** | Recommender. CLaMP 3 ingest pipeline + content ANN. Behavioural index + nightly retrain. Text-query stations. |

## Status

For comprehensive onboarding documentation (architecture, getting
started, per-component reference, API), see [`docs/`](./docs/). This
section is the high-level "what's done, what isn't" view.

**P3 done.** OAuth 2.1 server complete (P3.1):

- Hand-rolled. Six tables in `gateway-state.sqlite` —
  `users` (Argon2id; originally the single master-password row, now the
  multi-user `id/username/role/host_user_id/expires_at` table — see
  "Multi-user & roles"), `oauth_clients`, `auth_codes`,
  `refresh_tokens`, `access_tokens`, `device_codes` (RFC 8628). Plus a
  `sessions` table for the browser login flow.
- Endpoints: `POST /oauth/setup` (one-shot bootstrap), `GET/POST
  /oauth/login`, `GET /oauth/authorize` (Authorization Code + PKCE
  S256), `POST /oauth/token` (code grant + refresh-token rotation +
  `device_code` grant), `POST /oauth/revoke` (RFC 7009),
  `POST /oauth/device_authorization` + session-gated `GET/POST
  /oauth/device` (Device Authorization Grant — the CLI's auth path).
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
(POST), and `/v1/sync` (WS fan-out). CLI has `crates-cli sync state|push|watch`;
web has the optimistic-update sync provider. Single-linearizer model
(no CRDTs) — gateway sequences all ops and broadcasts.

**P6 minimum viable done.** Recommender stack:

- `music-recommend` crate: SQLite-backed embedding store
  (content-addressed by `(track_id, model_version)`), ingest queue
  (status column doubles as the queue), append-only event log.
- `usearch` HNSW with cosine metric, persisted alongside a JSON
  sidecar for the `(TrackId ↔ u64)` map. ANN is a derived cache —
  rebuildable from SQLite at boot.
- Python sidecar (`services/embedder/`): FastAPI; backend selected via
  `EMBEDDER_BACKEND` — **CLaMP 3 (768-dim) is the default
  backend**, LAION CLAP (512-dim) is the legacy fallback, plus a stub
  backend for tests / dev. ROCm GPU inference enabled —
  `pyproject.toml` routes torch through PyTorch's ROCm wheel index;
  `/healthz` reports `device: "cuda" | "cpu"` and `dim` so silent CPU
  fallback is visible. ~6× wall-clock speedup over CPU on an RDNA4 GPU
  (5-track drain: 31 s CPU → 5 s GPU; pipeline now Subsonic-bound, not
  compute-bound).
- Gateway endpoints: `GET /v1/recommend/next`, `POST /v1/recommend/enqueue`,
  `POST /v1/events`. Boot-time embedder probe with degraded-mode
  fallback if unreachable.
- Storage: `gateway-state.recommend.sqlite` (separate file from OAuth
  state — sqlx tracks migrations per pool). ANN at `gateway-state.ann`
  + `.ann.keys` sidecar.

**Deferred from P6:** behavioural index + nightly track2vec retrain
(P6.8). The event log is in place to capture signal for P6.8 when it
lands. **P6.9 (text-query stations) done (CLaMP 3)** — `embed_text` is
implemented in `Clamp3Embedder` and the gateway's
`GET /v1/recommend/station?text=…` is wired; see the CLaMP 3 migration
notes below.

**CLaMP 3 migration — DONE (merged to `dev`, PR #13).** Swapped
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
- **Cross-modal text mean** (migration 0012; code in `music-gateway/src/whitening_text.rs`): ABTT is
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
  HF prebake) — for the split-host shape, where the embedder runs on a
  GPU host the gateway reaches over the LAN.
- **The GPU image is built and smoke-tested.** Real forward pass on
  RDNA4 verified end-to-end: `/healthz` → `dim:768, device:cuda`, saas
  `state_dict` aligns, output L2-normed (norm=1.0), warm ~230 ms/clip
  (cold first call ~5 s = MIOpen JIT, then cached). Swapping CLAP → CLaMP 3
  keeps the same container name, port and shared bearer token.

**Deployment topology.** Single-host (everything in one compose stack) is
the simple case. A split-host layout is also supported and is what the
`gateway-only` / `embedder-only` compose files exist for:

- The **gateway host** runs `crates-gateway` + `crates-caddy` and may be
  **CPU-only**; its primary `EMBEDDER_URL` dials whichever host runs the
  sidecar. Since the failover work (PR #22) it can also run a local
  **CPU CLaMP 3** embedder registered as a `fallback_urls` entry — same
  checkpoint, so same `model_version`/768-dim and ANN-compatible — which
  carries text-station traffic when the GPU host is unreachable.
- The optional **embedder host** has the GPU (RDNA4/gfx1201 is the
  tested target) and runs the sidecar on `:9000`. Only the gateway and
  caddy images need shipping to the gateway host; the GPU host builds
  and runs the embedder locally.

**The 512→768 cutover has been performed end-to-end**, so the mechanics
below are known-good rather than theoretical: `RECOMMEND_EMBEDDING_DIM`
reaches `gateway.toml` via `gen_config.py`, the ANN rebuilds at the new
dim from SQLite, and a healthy boot logs the embedder probe dim/device,
the loaded whitening `k`, and a fitted cross-modal text mean.

Cutover mechanics (e.g. future model bumps): (1) rebuild
`crates-music/gateway:dev` from the branch and ship to the gateway host
(`scripts/ship-image.sh … nas-host`, `REMOTE_DOCKER="sudo docker"`);
(2) set `RECOMMEND_EMBEDDING_DIM` in the platform's env config;
(3) wipe the old-dim ANN sidecar (`gateway-state.ann` + `.ann.keys`) —
a dim change is non-migratable; (4) Save/restart; (5) re-embedding needs
no manual step — the discovery watcher's boot sweep enqueues the whole
catalog at the new `model_version` (force it early with
`POST /v1/admin/discovery/scan`; `scripts/enqueue_all_tracks.py` is the
out-of-band fallback) — recommender runs degraded until the GPU drains
the queue. The cached ABTT whitening (`embedding_whitening`
table) does **not** need a manual wipe — `load_or_fit_whitening` detects
a stale-dim cached row at boot, logs a warning, and refits from the
corpus automatically.

**Post-P6-MVP recommender + ratings work — all merged to `dev`.** Built
on top of the CLaMP 3 base after the P6 minimum-viable slice:

- **Track / entity ratings (PR #16, migrations 0014 `track_rating` +
  0015 `entity_rating`).** Gateway-owned per-song/album/artist
  like/dislike with **no Navidrome writeback** (deliberate constraint):
  dislike = exclude from recommendations + auto-skip, like = score boost
  + a Liked page. Always-on. Endpoints `PUT /v1/library/rating` +
  `GET /v1/library/ratings`; the web skip producer feeds `/v1/events`.
- **Rules-based preference affinity (migration 0013 `track_affinity`).**
  Server-side re-scoring of recommendation candidates from accumulated
  like/skip signal; `HardCap` is the live default blend mode.
- **Tethered-drift autoplay (PR #19).** Fixes the ~20-track single-anchor
  ceiling with a recency frontier (travel) + anchor leash (boundary);
  all params user-tunable in the web `/settings`.
- **Recommendation provenance (migration 0016 `recommendation_log`).**
  Logs what was served + context + scores as a training substrate for a
  future ranking model; readable at `GET /v1/diagnostics/recommendations`.
- Migrations in `crates/music-recommend/migrations/` now run through
  **0016** (the P6-MVP notes above only cover 0011–0012).

**Automatic catalog discovery — DONE (2026-08-02, on `dev`, not yet
deployed).** Closes the last manual step in the ingest pipeline: newly-
added Navidrome tracks are now enqueued for embedding on their own, so
"new music is browsable" and "new music is recommendable" no longer drift
apart. `crates/music-gateway/src/discovery.rs` + `[discovery]` config +
`EmbeddingStore::enqueue_many` + `POST /v1/admin/discovery/scan`. Design
notes in the "Ingest pipeline" section above; the load-bearing property
is that it's stateless (re-offer + `INSERT OR IGNORE`), so a scan can
fail, overlap, or double-run with no consequence.

**Embedder failover + watchdog (PR #22, merged to `dev` 2026-06-09;
deployed).** Text-prompt **stations** now survive the GPU
embedder sidecar being down (`/v1/recommend/next` was never affected —
it reads stored vectors). Root cause was a boot-latched health flag
(`record_health` was dead code). Fix: `[embedder]` gained
`fallback_urls` + a `probe_interval_seconds` re-probe loop that switches
the active client to the first healthy URL. A CPU CLaMP 3 fallback runs
alongside the gateway (`http://embedder:9000`) behind the GPU primary; an optional
docker watchdog (`docker-compose.embedder-failover.yml` +
`docker/embedder/watchdog.sh`) can start/stop a local CPU fallback on
demand. See `docs/DEPLOYMENT.md` "Embedder failover".

**Multi-user & roles — DONE** (three-role model, shipped across PRs A–F,
all merged). Retires the original single-user assumption: **admin / user /
guest** with full per-user isolation of gateway-owned state, piggybacked on
the existing hand-rolled OAuth (opaque tokens, no JWT — preserves instant
revocation + the device-grant CLI flow). Locked decisions + full design live
in `docs/plans/user-system.md`, which is deliberately **untracked** (working
notes, not shipped).

The load-bearing invariant is the **ownership boundary**: the gateway talks to
one Navidrome account, so the *catalog* (albums/artists/tracks, scrobble
counts) is shared, while gateway-owned state — queue/playback, taste, ratings,
affinity, recommendations, event log, playlists, tokens/sessions — partitions
by user. Anything new that stores per-user signal belongs on the partitioned
side.

Consequences worth knowing before touching this area:

- Guests are ephemeral principals attached to a **host user's room** (shared
  jukebox), are 403'd on write/admin tiers, and their taste signal is dropped
  from training — don't let guest events reach the recommender.
- Sync is per-room: `SyncStore` is a `HashMap<room_id, RoomSync>` and a WS
  subscriber only ever sees its own room.
- Playlists are gateway-owned (`/v1/playlists/*`), private per user with an
  opt-in `shared` flag and existence-hiding 404s for non-owners. They store
  only Navidrome track ids; clients hydrate via `/rest/getSong`.
- Migration numbering is a real hazard when authoring on parallel branches —
  two branches both claiming `0006` apply out-of-order on an already-migrated
  DB. Check the highest applied number before adding one.

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

## Mobile is the PWA (P4 native app retired)

The original plan had a native Android client (Compose Multiplatform +
Media3, Rust core via UniFFI) as phase P4. That was **retired** — the
mobile client is the **web app installed as a PWA**. Reasoning: a
self-hosted LAN player gets ~100% of what a native app would give it
(home-screen install, offline playback, background audio, lock-screen
controls) from the PWA, at zero additional codebase. The web client is
therefore *the* cross-platform client; "mobile parity" means making the
web UI good on phone viewports + wiring the mobile-browser web APIs, not
shipping a second app.

**The PWA + offline slice is done** (merged 2026-06-07). What matters for
future work, rather than the feature inventory (see `git log` and
`docs/components/`):

- `apps/web/src/cache/` reimplements the `music-cache` contract in IndexedDB.
  Audio is stored as **whole-file blobs** served via `URL.createObjectURL`,
  *not* a Service-Worker + Cache-API setup — because the stream endpoint has
  **no HTTP Range support**, so the browser has to seek locally against a
  stored file. Don't "modernize" this to the SW approach without fixing Range
  first.
- `AudioCacheContext` resolves a src **synchronously** for the
  gesture-critical `primePlayback` path (an in-memory `trackId → blob:URL`
  map warmed over the queue window); only the natural-advance path, which has
  no live user gesture, is allowed to await the cache. Breaking that
  distinction breaks playback on iOS.
- The four OAuth pages are server-rendered **outside** the SPA
  (`oauth/handlers.rs`), so SPA-level viewport/meta fixes never reach them —
  they need their own.
- An installed PWA's normal lifecycle is relaunch-from-snapshot, so queue
  items arrive with ids but no metadata; `SyncContext` backfills via `getSong`
  and consumers must key on its `metaTick` to re-render.
- Transcode-to-fit needed no gateway work: the verbatim `/rest/*` proxy passes
  `format`/`maxBitRate` straight to Navidrome.

- **Open (real-device only, can't be done headless):** install to home
  screen, airplane-mode offline launch + playback, screen-off background
  audio. The prod gateway (`crates.example.com:8443`) serves a Let's
  Encrypt cert, so **no mkcert CA trust is needed on the device** for
  install — the earlier mkcert caveat only applies to the `gateway.local`
  dev cert.

### Running it locally

Setup is documented once, in `docs/`, so it can't drift from this file:

- [`docs/GETTING-STARTED.md`](./docs/GETTING-STARTED.md) — dev certs, gateway
  config, CLI config, the `crates-cli auth login` device flow, Vite dev server.
- [`docs/CONFIGURATION.md`](./docs/CONFIGURATION.md) — every config block,
  including `[embedder]` (backends, `fallback_urls`) and `[cache]` budgets.
- [`docs/DEPLOYMENT.md`](./docs/DEPLOYMENT.md) — containers, reverse proxy,
  split-host embedder, model/dim bumps.
- Benchmarks: the `benchmarks` skill (`.claude/skills/benchmarks/`).

Two things worth knowing before you start: the CLI keeps `[server]` creds
alongside `[gateway]` so you can flip between gateway and direct mode without
rewriting config, and the gateway's `[recommend] embedding_dim` **must** match
the embedder's dim — a mismatch is non-migratable and needs the ANN sidecar
wiped.
