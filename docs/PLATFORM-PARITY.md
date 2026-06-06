# Platform feature parity

What each client can actually do today. The three clients sit at very
different maturity levels because features were built in *vertical
slices* (see the phasing table in [../CLAUDE.md](../CLAUDE.md)) and each
slice landed on one client first:

- **Web** — the P3/P6 vertical. Has the full recommender, ratings,
  station, autoplay, and diagnostics surface.
- **CLI** — stalled at its P0–P2 + P5 scope (browse, native playback,
  cache/pin, sync). **Next focus** for parity work.
- **Android** — P4, **not started**. `apps/mobile/` and
  `crates/music-ffi/` do not exist yet; every row below is "planned".

## How to read this

Almost every capability lives behind a gateway `/v1/*` endpoint (the
source of truth). So a gap on a client usually means *"no UI/command
calls that endpoint yet"*, not *"impossible here"*. The legend keeps
that distinction:

| Symbol | Meaning |
|---|---|
| ✅ | Available |
| ⚠️ | Partial / limited |
| ❌ | Not implemented on this client, **but the gateway endpoint exists** — purely a client-surface gap |
| 🚫 | Not applicable to this client by design |
| 🔭 | Planned (Android: nothing built yet) |

## Matrix

### Browsing & library

| Capability | Web | CLI | Android |
|---|:--:|:--:|:--:|
| Server connectivity check (`ping`) | 🚫 | ✅ | 🔭 |
| Browse albums | ✅ | ✅ | 🔭 |
| Album detail + track list | ✅ | ✅ | 🔭 |
| Browse artists | ✅ | ✅ `artists` | 🔭 |
| Artist detail + discography | ✅ | ✅ `artist <id>` | 🔭 |
| Browse all tracks (paginated) | ✅ | ✅ `tracks` | 🔭 |
| Browse filters (recent / most-played / random) | ✅ | ⚠️ albums only | 🔭 |
| Global search (artists / albums / tracks) | ✅ | ✅ `search <q>` | 🔭 |
| Home / overview page | ✅ | 🚫 | 🔭 |

### Playback

| Capability | Web | CLI | Android |
|---|:--:|:--:|:--:|
| Stream & play a track | ✅ `<audio>` | ✅ rodio/symphonia | 🔭 Media3 |
| Gapless playback | ⚠️ deferred (MSE) | ✅ | 🔭 |
| Offline playback (cache-only) | ✅ (IndexedDB) | ✅ `play --offline` | 🔭 |
| Installable / offline launch (PWA) | ✅ service worker | 🚫 | 🔭 |
| Transport: play / pause / seek | ✅ | ⚠️ no transport UI | 🔭 |
| Skip forward / back | ✅ | ❌ | 🔭 |
| Volume control | ✅ | ❌ | 🔭 |
| Background / foreground-service playback | 🚫 | 🚫 | 🔭 |

### Queue

| Capability | Web | CLI | Android |
|---|:--:|:--:|:--:|
| View queue + now-playing | ✅ | ⚠️ `sync state` (JSON) | 🔭 |
| Append to queue | ✅ | ✅ `sync push` | 🔭 |
| Reorder / move | ✅ | ❌ | 🔭 |
| Remove / clear upcoming | ✅ | ❌ | 🔭 |
| Jump to track | ✅ | ❌ | 🔭 |

### Cache & pinning (client-local)

| Capability | Web | CLI | Android |
|---|:--:|:--:|:--:|
| Pin / unpin tracks | ✅ "save for offline" | ✅ | 🔭 |
| List pinned | ✅ `/downloads` | ✅ `pinned` | 🔭 |
| Cache stats | ✅ `/downloads` | ✅ `cache stats` | 🔭 |
| Force eviction | ✅ "free up space" | ✅ `cache evict` | 🔭 |
| Bulk download album/playlist | ✅ | ⚠️ per-track `pin` | 🔭 |

> The web client now mirrors the CLI's two-budget L3 cache in the browser
> (IndexedDB blobs + `URL.createObjectURL`), reusing the exact `music-cache`
> contract: content-addressed `(trackId, bitrate, codec)`, a regular LRU
> budget (auto-cached recents) and a separate never-evicted pinned budget.
> See `apps/web/src/cache/`. Android (P4) is expected to reuse this same
> browser cache if it ships as a PWA, or the Rust `music-cache` crate via
> UniFFI if it ships native.

### Stations & recommendations

| Capability | Web | CLI | Android |
|---|:--:|:--:|:--:|
| Text-prompt station (`/v1/recommend/station?text=`) | ✅ | ✅ `station` | 🔭 |
| Station from album / artist (seed) | ✅ | ❌ | 🔭 |
| "Recommend next" / autoplay refill (`/v1/recommend/next`) | ✅ | ✅ `recommend next` | 🔭 |
| Similar albums / artists | ✅ | ❌ | 🔭 |
| Playlist "suggest more tracks" | ✅ | ❌ | 🔭 |

### Autoplay (tethered-drift)

| Capability | Web | CLI | Android |
|---|:--:|:--:|:--:|
| Autoplay on/off ("keep queue topped up") | ✅ | ❌ | 🔭 |
| Tuning knobs (vibe radius, leash, travel, diversity) | ✅ `/settings` | ❌ | 🔭 |

### Ratings & feedback

| Capability | Web | CLI | Android |
|---|:--:|:--:|:--:|
| Like / dislike track | ✅ | ✅ `like`/`dislike` | 🔭 |
| Like / dislike album / artist | ✅ | ✅ `--kind album\|artist` | 🔭 |
| Liked page (tracks / albums / artists) | ✅ | ✅ `liked` | 🔭 |
| Clear a rating | ✅ | ✅ `unrate` | 🔭 |
| Recommendation feedback (thumbs, session-scoped) | ✅ | ❌ | 🔭 |

> Ratings have **no Navidrome writeback** by design — they are
> gateway-owned. Any CLI/Android implementation must keep that constraint.

### Playlists

| Capability | Web | CLI | Android |
|---|:--:|:--:|:--:|
| View playlist detail | ✅ | ❌ | 🔭 |
| Create / rename / delete | ✅ | ❌ | 🔭 |
| Add tracks / add suggestions | ✅ | ❌ | 🔭 |

### Sync (cross-device)

| Capability | Web | CLI | Android |
|---|:--:|:--:|:--:|
| Read sync snapshot | ✅ | ✅ `sync state` | 🔭 |
| Push ops (append, etc.) | ✅ | ⚠️ append only | 🔭 |
| Live WebSocket updates | ✅ | ✅ `sync watch` | 🔭 |
| Optimistic UI + rollback | ✅ | 🚫 | 🔭 |

### Diagnostics

| Capability | Web | CLI | Android |
|---|:--:|:--:|:--:|
| Recommender metrics | ✅ | ❌ | 🚫 |
| Latent-space visualization (2-D/3-D) | ✅ | 🚫 | 🚫 |
| Ingest backlog | ✅ | ❌ | 🚫 |
| Listening history | ✅ | ❌ | 🚫 |
| Tracing waterfalls | ✅ | ❌ | 🚫 |
| Browser RUM / web-vitals | ✅ | 🚫 | 🚫 |

### Auth

| Capability | Web | CLI | Android |
|---|:--:|:--:|:--:|
| OAuth flow | ✅ Auth Code + PKCE | 🔭 Device Grant (RFC 8628) | 🔭 PKCE via Custom Tabs |
| Direct Subsonic creds (no gateway) | 🚫 | ✅ `[server]` config | 🔭 |

> The CLI currently authenticates with the gateway via a **static
> bearer token** in `[gateway]` config, or talks to Navidrome directly
> with `[server]` creds. The Device Authorization Grant flow described
> in CLAUDE.md is not yet wired into the CLI.

## CLI parity backlog (the ❌ rows)

Bringing the CLI toward the web UI is mostly mechanical — wiring clap
subcommands onto endpoints that already exist. Rough priority:

1. ~~**Browse parity** — `artists`, `tracks`, `search`.~~ **Done** —
   `artists`, `artist <id>`, `tracks`, `search <q>` via new typed
   `music-subsonic` methods (`get_artists`/`get_artist`/`search3`). Works
   in both direct and gateway mode; no gateway change.
2. ~~**Ratings** — `like` / `dislike` / `liked`.~~ **Done** — `like`,
   `dislike`, `unrate`, `liked` against `PUT/GET /v1/library/rating(s)`,
   with `--kind track|album|artist`. Gateway-owned; no Navidrome
   writeback. Shared gateway HTTP plumbing extracted to `gateway.rs`.
3. ~~**Stations** — `station "<prompt>"`.~~ **Done** — `station
   "<prompt>"` → `GET /v1/recommend/station`, resolving ranked ids to
   titles via the Subsonic client. (Seed-from-album/artist station still
   open.)
4. ~~**Recommend** — `recommend next <seed>`.~~ **Done** — `recommend
   next <seed> [-n N]` → `GET /v1/recommend/next`, resolving ids to
   titles; notes degraded mode. (A queue-fill loop mirroring web autoplay
   is still open.)
5. **Queue management** — reorder / remove / jump via sync ops (CLI
   currently only appends).
6. **Auth** — Device Authorization Grant (RFC 8628) to replace the
   static bearer token.

## Android (P4) — not started

`apps/mobile/` (Compose Multiplatform) and `crates/music-ffi` (UniFFI
bindings) do not exist yet. Two viable roads:

- **PWA-as-mobile** (favoured for a single-user LAN app): install the web
  app (now a PWA — service worker app shell + manifest, see
  `apps/web/vite.config.ts`) to the home screen, or wrap it as a TWA. This
  reuses 100% of the web client, **including the offline audio cache and
  service worker built here** — offline playback "just works" on Android
  Chrome. Caveat: the device must trust the mkcert CA for a secure context.
- **Native Compose** (the original CLAUDE.md plan): UniFFI exposes
  `music-cache`, `music-sync`, `music-subsonic` to Kotlin (**not**
  `music-player` — mobile playback uses Media3 directly), Compose
  Multiplatform UI, foreground service for background playback. Offline
  cache would come from the Rust `music-cache` crate, not the web TS cache.
