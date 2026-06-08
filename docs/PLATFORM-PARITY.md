# Platform feature parity

What each client can actually do today. There are **two** clients, not
three:

- **Web / PWA** — the P3/P6 vertical, and **also the mobile client**: the
  same React app installed to a phone home screen as a PWA. Has the full
  recommender, ratings, station, autoplay, diagnostics surface, and an
  offline audio cache. One column below covers both desktop browser and
  installed-on-phone use.
- **CLI** — its P0–P2 + P5 scope (browse, native playback, cache/pin,
  sync) plus the CLI-parity backlog, which is now **cleared** (see bottom).

> **Native mobile (P4) was retired.** `apps/mobile/` (Compose
> Multiplatform) and `crates/music-ffi/` (UniFFI bindings) were never built
> and will not be — the installable PWA is the mobile client. Anything in
> older notes about a Kotlin/Media3 app is historical. See
> [../CLAUDE.md](../CLAUDE.md) → "Mobile is the PWA".

## How to read this

Almost every capability lives behind a gateway `/v1/*` endpoint (the
source of truth). So a gap on the CLI usually means *"no command calls
that endpoint yet"*, not *"impossible here"*. The legend keeps that
distinction:

| Symbol | Meaning |
|---|---|
| ✅ | Available |
| ⚠️ | Partial / limited |
| ❌ | Not implemented on this client, **but the gateway endpoint exists** — purely a client-surface gap |
| 🚫 | Not applicable to this client by design |

Because mobile *is* the Web/PWA client, the Web column applies on a phone
too. The handful of capabilities that behave differently on a phone
(install, background audio, lock-screen controls, touch reachability) are
called out in **Mobile specifics** below the matrix.

## Matrix

### Browsing & library

| Capability | Web / PWA | CLI |
|---|:--:|:--:|
| Server connectivity check (`ping`) | 🚫 | ✅ |
| Browse albums | ✅ | ✅ |
| Album detail + track list | ✅ | ✅ |
| Browse artists | ✅ | ✅ `artists` |
| Artist detail + discography | ✅ | ✅ `artist <id>` |
| Browse all tracks (paginated) | ✅ | ✅ `tracks` |
| Browse filters (recent / most-played / random) | ✅ | ⚠️ albums only |
| Global search (artists / albums / tracks) | ✅ | ✅ `search <q>` |
| Home / overview page | ✅ | 🚫 |

### Playback

| Capability | Web / PWA | CLI |
|---|:--:|:--:|
| Stream & play a track | ✅ `<audio>` | ✅ rodio/symphonia |
| Gapless playback | ⚠️ deferred (MSE) | ✅ |
| Offline playback (cache-only) | ✅ (IndexedDB) | ✅ `play --offline` |
| Installable / offline launch (PWA) | ✅ service worker | 🚫 |
| Transport: play / pause / seek | ✅ | ⚠️ no transport UI |
| Skip forward / back | ✅ | ❌ |
| Volume control | ✅ | ❌ |
| Lock-screen / Media Session controls | ✅ (phone) | 🚫 |
| Background playback | ⚠️ Android: yes; iOS: limited | 🚫 |

### Queue

| Capability | Web / PWA | CLI |
|---|:--:|:--:|
| View queue + now-playing | ✅ | ✅ `sync queue` (resolved titles + ▶) |
| Append to queue | ✅ | ✅ `sync push` |
| Reorder / move | ✅ (buttons + row-menu on phones) | ✅ `sync move` (alias `reorder`) |
| Remove a queue item | ✅ | ✅ `sync remove` |
| Clear whole queue | ✅ | ✅ `sync clear` |
| Jump to track | ✅ | ✅ `sync jump <index>` |

### Cache & pinning (client-local)

| Capability | Web / PWA | CLI |
|---|:--:|:--:|
| Pin / unpin tracks | ✅ "save for offline" | ✅ |
| List pinned | ✅ `/downloads` | ✅ `pinned` |
| Cache stats | ✅ `/downloads` | ✅ `cache stats` |
| Force eviction | ✅ "free up space" | ✅ `cache evict` |
| Bulk download album/playlist | ✅ | ⚠️ per-track `pin` |
| Transcode-to-fit downloads | ✅ original / opus128 / mp3128 | ❌ |

> The web client mirrors the CLI's two-budget L3 cache in the browser
> (IndexedDB blobs + `URL.createObjectURL`), reusing the exact `music-cache`
> contract: content-addressed `(trackId, bitrate, codec)`, a regular LRU
> budget (auto-cached recents) and a separate never-evicted pinned budget.
> See `apps/web/src/cache/`. Because mobile is this same PWA, the phone gets
> this cache for free — there is no separate native cache. Transcode-to-fit
> (`downloadQuality` in `cacheSettings.ts`) forwards `format`/`maxBitRate`
> on the verbatim `/rest/*` proxy to Navidrome; no gateway endpoint was
> needed.

### Stations & recommendations

| Capability | Web / PWA | CLI |
|---|:--:|:--:|
| Text-prompt station (`/v1/recommend/station?text=`) | ✅ | ✅ `station` |
| Station from album / artist (seed) | ✅ | ❌ |
| "Recommend next" / autoplay refill (`/v1/recommend/next`) | ✅ | ✅ `recommend next` |
| Similar albums / artists | ✅ | ❌ |
| Playlist "suggest more tracks" | ✅ | ❌ |

### Autoplay (tethered-drift)

| Capability | Web / PWA | CLI |
|---|:--:|:--:|
| Autoplay on/off ("keep queue topped up") | ✅ | ❌ |
| Tuning knobs (vibe radius, leash, travel, diversity) | ✅ `/settings` | ❌ |

### Ratings & feedback

| Capability | Web / PWA | CLI |
|---|:--:|:--:|
| Like / dislike track | ✅ | ✅ `like`/`dislike` |
| Like / dislike album / artist | ✅ | ✅ `--kind album\|artist` |
| Liked page (tracks / albums / artists) | ✅ | ✅ `liked` |
| Clear a rating | ✅ | ✅ `unrate` |
| Recommendation feedback (thumbs, session-scoped) | ✅ | ❌ |

> Ratings have **no Navidrome writeback** by design — they are
> gateway-owned. Any CLI implementation must keep that constraint.

### Playlists

| Capability | Web / PWA | CLI |
|---|:--:|:--:|
| View playlist detail | ✅ | ❌ |
| Create / rename / delete | ✅ | ❌ |
| Add tracks / add suggestions | ✅ | ❌ |

### Sync (cross-device)

| Capability | Web / PWA | CLI |
|---|:--:|:--:|
| Read sync snapshot | ✅ | ✅ `sync state` / `sync queue` |
| Push ops (append, etc.) | ✅ | ✅ append / remove / move / jump / clear |
| Live WebSocket updates | ✅ | ✅ `sync watch` |
| Optimistic UI + rollback | ✅ | 🚫 |

### Diagnostics

| Capability | Web / PWA | CLI |
|---|:--:|:--:|
| Recommender metrics | ✅ | ❌ |
| Latent-space visualization (2-D/3-D) | ✅ | 🚫 |
| Ingest backlog | ✅ | ❌ |
| Listening history | ✅ | ❌ |
| Tracing waterfalls | ✅ | ❌ |
| Browser RUM / web-vitals | ✅ | 🚫 |

### Auth

| Capability | Web / PWA | CLI |
|---|:--:|:--:|
| OAuth flow | ✅ Auth Code + PKCE | ✅ `auth login` (Device Grant, RFC 8628) |
| Direct Subsonic creds (no gateway) | 🚫 | ✅ `[server]` config |

> The CLI authenticates with the gateway via the **Device Authorization
> Grant** (RFC 8628): `music auth login` prints a short code + URL, you
> approve it in a logged-in browser, and the CLI stores rotating
> per-device tokens in `cli-tokens.json` (next to the config, `0600`).
> `auth status` / `auth logout` manage that store; the access token
> refreshes automatically. There is no static `[gateway].bearer_token`.
> Direct mode still uses `[server]` creds against Navidrome.

## Mobile specifics (the PWA on a phone)

The mobile client is the Web column installed as a PWA, so it inherits
every ✅ above. What's *phone-specific* (and shipped, merged to `dev`
2026-06-07):

- **Install** — Chromium (Android Chrome/Edge/Brave/Samsung Internet)
  shows an in-app "Install as app" button on the Settings page (captured
  `beforeinstallprompt`) and a browser-menu "Install app" entry; iOS Safari
  installs via Share → Add to Home Screen (the UI shows a hint, since Safari
  never fires `beforeinstallprompt`). DuckDuckGo and other non-Chromium
  Android browsers cannot install PWAs.
- **Secure context** — install/offline/Media Session all require HTTPS. The
  production gateway serves a Let's Encrypt cert (trusted by default), so no
  manual CA trust is needed on the device. The `gateway.local` mkcert dev
  cert *does* need the CA trusted to install from the dev origin.
- **Background audio** — Android keeps the PWA's `<audio>` playing in the
  background with lock-screen controls via the Media Session API. iOS is
  more restrictive (background audio for a home-screen PWA works but is
  flakier than a native app).
- **Lock-screen controls** — Media Session action handlers (play/pause/
  next/prev/seek) + `setPositionState` drive the OS scrubber.
- **Responsive UI / touch** — sidebar→drawer, two-row phone player,
  reflowing tables, enlarged touch targets, queue reorder reachable from the
  row menu (the desktop chevron buttons are hidden on phones), and a
  phone-correct viewport on the server-rendered OAuth pages.

**Open (real-device only, can't be verified headless):** install to home
screen, airplane-mode offline launch + playback, screen-off background
audio.

## CLI parity backlog (the ❌ rows)

Bringing the CLI toward the web UI is mostly mechanical — wiring clap
subcommands onto endpoints that already exist. The original backlog is
**cleared**:

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
5. ~~**Queue management** — reorder / remove / jump via sync ops.~~
   **Done** — `sync queue` (readable view with resolved titles + ▶
   now-playing marker), `sync remove`, `sync move` (alias `reorder`),
   `sync jump <index>`, `sync clear`. All ops already existed in
   `music-sync`; this was pure client surface.
6. ~~**Auth** — Device Authorization Grant (RFC 8628) to replace the
   static bearer token.~~ **Done** — `music auth login|logout|status`.
   Gateway gained `POST /oauth/device_authorization`, a session-gated
   `GET/POST /oauth/device` approval page, and the `device_code` token
   grant; the CLI polls, persists rotating tokens to `cli-tokens.json`
   (`0600`), and auto-refreshes. The static `[gateway].bearer_token` was
   removed.

Remaining ❌ rows are smaller, lower-priority items (seed-from-album
station, similar albums/artists, playlist CRUD, recommendation thumbs,
diagnostics views).
