# web

**Path:** `apps/web/`
**Type:** Vite + React 19 single-page app, installable as a PWA
**Test count:** 207 (Vitest, 19 files)

The browser client — and, installed to a phone's home screen as a
PWA, *the* mobile client (native mobile was retired; see
[PLATFORM-PARITY.md](../PLATFORM-PARITY.md)). Talks to the gateway
over HTTPS for both REST calls (`/rest/*`, `/v1/*`) and the sync
WebSocket (`/v1/sync`).

## Stack

- **React 19** — concurrent rendering, server components not used (we're a client SPA).
- **TypeScript 6** — strict mode.
- **Vite 8** — build tool. Dev server on port 5173 with HMR.
- **TanStack Query 5** — request caching + retries + invalidation.
- **Tailwind 3** — styling; **lucide-react** for icons.
- **Plain `<audio>` element** — playback. MSE (Media Source Extensions)
  for gapless playback is deferred until basic boundary handoff
  proves audibly gappy in real use.
- **vite-plugin-pwa** (`registerType: autoUpdate`) — service worker
  precaches the app shell; `/v1`, `/rest`, `/oauth` are NetworkOnly
  and audio never touches the SW.
- **idb** — IndexedDB offline audio cache (`src/cache/`), a TS
  reimplementation of the `music-cache` contract.
- **three / @react-three/fiber** — the 3-D latent-space view only,
  code-split into a lazy chunk so it never weighs down the main bundle.

No state management library beyond TanStack Query + React context.
No routing library — `router.tsx` is ~30 lines of `useState` +
`useEffect` over the History API (`location.pathname` +
`pushState`). We'll lift in `react-router` if routing complexity
grows.

## Layout

```
apps/web/
├── package.json
├── vite.config.ts
├── tsconfig.json
├── tailwind.config.js
├── index.html
└── src/
    ├── main.tsx              # entry: ReactDOM.createRoot
    ├── App.tsx               # provider tree
    ├── router.tsx            # History API routing
    ├── api/                  # gateway client (REST, playlists, users, diagnostics)
    ├── auth/                 # OAuth flow + AuthContext + whoami/role hooks
    ├── cache/                # IndexedDB offline audio cache + AudioCacheContext
    ├── components/           # shared UI primitives
    ├── pages/                # route components — see below
    ├── player/               # PlayerProvider + bottom-bar UI (thumbs feedback)
    ├── pwa/                  # beforeinstallprompt capture + install button
    ├── rum/                  # web-vitals + markEvent + flush loop
    ├── settings/             # settings shell: nav model, rail, panels
    ├── styles/               # Tailwind + design-bundle CSS
    ├── sync/                 # WebSocket client + SyncContext
    └── utils/                # shared helpers
```

Pages today (`apps/web/src/pages/`):

- `Home`, `Albums`, `Album`, `Artists`, `Artist`, `Tracks` — catalog.
  `Artist` carries a **"most played"** chart above the discography,
  ranked by our own play counts and hidden entirely for an artist
  that's never been played. It does *not* come from `getTopSongs`:
  Navidrome backs that endpoint with Last.fm's top-tracks chart mapped
  onto local files, and its rows mostly carry no `playCount` at all.
  Real counts only ride `search3` song rows, so the section pulls the
  artist's catalog via `searchArtistSongs` and ranks client-side in
  `sync/mostPlayed.ts` — Subsonic has no "this artist's songs, by
  plays" endpoint. The hero's play button still uses `getTopSongs`,
  which is the right source for "start with the hits."
- `Search`, `SearchBucket`, `searchRanking.ts`, `listMode.ts` —
  search with bucketed top-results re-ranking.
- `Playlist` — playlist view + management.
- `Queue` — current play queue with reorder / remove.
- `Station` — natural-language "playlist for a prompt." Posts to
  `/v1/recommend/station`, hydrates the returned track ids, plays
  them through the shared `PlayerContext`. State machine is a
  discriminated union (`idle`/`loading`/`ready`/`empty`/
  `unavailable`/`error`); results are deliberately not cached via
  TanStack Query (a stale "sunny afternoon" from yesterday would
  hide newly-ingested tracks).
- `LikedSongs` (`/liked`) — liked tracks/albums/artists, with
  bulk-cache-into-the-auto-budget buttons.
- `Downloads` (`/downloads`) — offline cache stats, pinned list,
  "free up space" (evict).
- `LatentSpace` / `LatentSpace3D` + `latentSpace.ts`/`.test.ts` —
  2D and 3D UMAP plot views (read
  `/v1/diagnostics/recommend/latent_space`). 2-D / 3-D toggle is
  decoupled from the colour mode.
- `diagnostics/` — topical diagnostics subpages: `Recommender`
  (per-feature dashboards), `LatentSpace` (above), `Ingest` (queue
  + ingest worker state), `Tracing` (M0 span ring), `Rum` (browser
  RUM events), `Listening` (recommend-session reconstruction with
  per-session events panel). These no longer have their own
  top-level page — they render inside the settings shell (below);
  old `/diagnostics/*` URLs redirect to `/settings/*`.
- `SignIn`, `Callback` — OAuth PKCE flow. Sign-in passes
  `prompt=login` and offers a "sign in as a different user" link so
  a lingering gateway `gw_session` cookie can't silently re-auth
  the previous account.

## Settings shell (`settings/`)

`/settings/<panel>` renders every settings *and* diagnostics surface
inside one shell (`SettingsShell` + `SettingsRail`). `nav.tsx` is the
single source of truth: a grouped list of panels, each `kind:
"config"` (a knob) or `kind: "observe"` (a diagnostic), interleaved
by concern so e.g. the recommender dashboards sit next to the
autoplay knobs. The rail, the mobile `<select>`, and App.tsx's route
table all derive from it — adding a panel is a one-line change.

Config panels: `Account`, `Guests` (guest-code management),
`Playback` (streaming/download quality + the per-device audio-output
toggle), `Appearance` (theme), `Autoplay` (tethered-drift params),
`Storage` (cache budgets), `About`, and the admin-only `UsersAdmin`.
Observe (diagnostics) panels are admin-only and fail closed: while
`whoami` is still resolving, a deep-linked observe panel shows the
default panel, never the reverse.

## Provider tree

```tsx
<QueryClientProvider client={queryClient}>   {/* main.tsx */}
  <AuthProvider>            {/* AuthContext.tsx — OAuth tokens, refresh */}
    <SyncProvider>          {/* WebSocket sub, optimistic state */}
      <AudioCacheProvider>  {/* IndexedDB cache, trackId → blob: URL map */}
        <PlayerProvider>    {/* current track, audio element */}
          <AutoplayProvider>{/* tethered-drift autoplay refill */}
            <ArtworkProvider>{/* extracted cover palette */}
              ...
            </ArtworkProvider>
          </AutoplayProvider>
        </PlayerProvider>
      </AudioCacheProvider>
    </SyncProvider>
  </AuthProvider>
</QueryClientProvider>
```

`AuthProvider` is the outermost runtime provider — every other
provider needs an access token to do anything useful. Tokens are
stored in `localStorage` and refreshed automatically when they're
within 60 s of expiry.

The `auth/` module splits the work: `pkce.ts` generates verifier +
challenge, `oauth.ts` runs the redirect dance, `tokens.ts` handles
storage + refresh, `AuthContext.tsx` exposes the React surface.

## OAuth flow

Authorization Code + PKCE.

1. User clicks "Sign in" → `auth.startLogin()`.
2. We generate a `code_verifier` (base64url of 32 random bytes), a
   `code_challenge` (`base64url(sha256(verifier))`), and a `state`.
   All three are stashed in `sessionStorage`.
3. We redirect to `/oauth/authorize?client_id=web&...&code_challenge=<challenge>&state=<state>`.
4. Gateway shows the login form, then redirects back to
   `/oauth/callback?code=<code>&state=<state>`.
5. The callback page checks `state` matches what we stashed, then
   POSTs to `/oauth/token` with `code_verifier` to exchange the code
   for an access token + refresh token.

Tokens are stored in localStorage. **This is not the most secure
option** — XSS would let an attacker steal tokens. The mitigation is
that the SPA is self-hosted and we control every dependency. For a
public deployment we'd move to httpOnly cookies and CSRF tokens, but
that requires the gateway to serve the SPA from the same origin
(which the dev proxy already simulates).

## Vite dev server

`vite.config.ts` proxies API calls to the gateway:

```ts
server: {
  port: 5173,
  proxy: {
    '/oauth': { target: 'https://gateway.local:8443', secure: false, changeOrigin: true },
    '/rest':  { target: 'https://gateway.local:8443', secure: false },
    '/v1':    { target: 'https://gateway.local:8443', secure: false, ws: true },
  }
}
```

`secure: false` skips TLS cert verification on the proxy. The
gateway uses a mkcert cert; mkcert's CA is in your system trust
store, but Node has its own bundled trust store that doesn't see it.
Skipping verification on the dev proxy avoids dragging the user
through `NODE_EXTRA_CA_CERTS` setup.

## Player

`PlayerProvider` owns:
- The `<audio>` element (mounted once at the bottom of the layout).
- Current track + queue + position.
- "Time-to-skip" event throttling.
- The player-bar **"rate rec"** thumbs (distinct from the **"rate
  track"** EntityRating control beside them — see [Library
  ratings](#library-ratings-likedislike)). Clicks POST to
  `/v1/recommend/feedback` with the active recommend session id and
  re-render with the returned `(up, down)` counts. Clicking an
  already-active thumb clears the vote. Locked unless the current track
  was pushed by the autoplay refill (rating a self-picked track would
  feed misleading recommender signal).
- The `playback.start` RUM mark (measured from `src` set → first
  `playing` event — the user-perceived latency, not `loadedmetadata`
  which fires too early).

When you click play on a track:
1. Resolve the src: if the track is in the offline cache,
   `AudioCacheContext`'s warmed `trackId → blob:` URL map answers
   **synchronously** (gesture-critical — `audio.play()` must happen in
   the click handler); otherwise compute the stream URL
   `/rest/stream?id=<track>&access_token=<...>`. (The token goes via
   query param because `<audio src>` can't set headers — see
   [API.md](../API.md#auth-model).)
2. Set `audioEl.src`. The browser starts buffering (or seeks locally
   against the stored blob).
3. `audioEl.play()`.

Position updates fire on `timeupdate` (~4 Hz). Likes / skips /
scrobbles are batched into `/v1/events` every 5s. Media Session
action handlers (play/pause/next/prev/seek) + `setPositionState`
drive lock-screen / notification controls on phones.

### Two notions of "the current track"

`PlayerContext` tracks these separately, and the distinction is
load-bearing:

- **`currentTrackId`** — `queue.items[now_playing_index]`, i.e. what
  *sync state* says the room is on. Drives queue mechanics: dislike
  auto-skip, next/prev bounds, the auto-advance index.
- **`loadedTrackId`** (`claimTrack`) — what this device's `<audio>`
  element is actually pointed at. Drives everything that describes
  what you can *hear*: player bar, Media Session metadata, scrobbles,
  the skip signal, the `playback.start` mark.

They agree whenever sync is healthy. They diverge whenever it isn't,
because step 1 above is synchronous while the matching sync op takes a
WS round-trip — so a click during an outage plays the right audio
against a stale cursor. Reading the cursor for playback identity is how
a play of one track ended up on another track's play count (and in the
recommender's recency clock) on 2026-08-03. `claimTrack` is called at
every `src` assignment and also resets the per-track scrobble flags, so
those two can't drift apart either.

A silent remote (`outputEnabled` false) never loads audio, so
`loadedTrackId` stays null and both notions fall back to the cursor —
which is correct: it should display and report the room's track.

### Per-device audio output (`player/outputDevice.ts`)

Two devices signed into the same account share one room and both obey
`is_playing`, so they already play in lockstep. A per-device
**"Play audio on this device"** toggle (PlayerBar button + Playback
settings panel, persisted in `localStorage`, default on) lets either
device opt out of producing sound while still driving the shared
queue — a silenced phone becomes a remote control; both on = synced
multi-room playback. Implemented purely client-side as a gate in
front of `audio.play()`; crucially the divergence-sync listeners
(which submit `set_playing` on local `<audio>` pause/play) are guarded
by the same flag, so a silenced remote never broadcasts its mute and
pauses the actual speaker. No gateway or sync-protocol changes.

## Library ratings (like/dislike)

Distinct from the player-bar *recommendation* thumbs above: this is the
durable **like/dislike of an entity itself** (track, album, or artist),
backed by `PUT`/`GET /v1/library/rating(s)`. The UI uses thumbs-up /
thumbs-down icons (a filled heart read as a like even for a dislike, so
the earlier `Heart`/`HeartCrack` pair was replaced).

- **`components/EntityRating.tsx`** — the shared tri-state control
  (like / neutral / dislike; clicking the active verdict clears it).
  Kind-aware copy and aria labels, reused by the player bar (track) and
  the Album / Artist hero headers.
- **`player/useRatings.ts`** — `useRatingsMaps()` fetches all ratings once
  (TanStack Query key `["library","ratings"]`) and splits them into three
  `Map<id, "like"|"dislike">` (tracks / albums / artists);
  `useEntityRating(kind, id)` is the optimistic mutation (patches the maps
  on `onMutate`, rolls back on error, invalidates on settle).
- **Auto-skip** — `player/autoSkip.ts` holds the pure predicates
  `isDislikedEntity(trackId, parents, maps)` (disliked if the track **or**
  its album **or** its artist is disliked) and `nextPlayableIndex(...)`.
  An effect in `PlayerContext.tsx` fires only on a genuine queue *advance*
  onto a new track — disliking the **currently-playing** track leaves it
  playing rather than yanking it. A direct single-track click overrides the
  skip (`directPlayRef`).
- **`pages/LikedSongs.tsx`** (`/liked`) — liked tracks, albums, and artists
  in sections, hydrated client-side from the id-only ratings list.

Enforcement is *also* server-side and always-on (dislikes are excluded from
recommendations, likes boost them) — the client maps only drive the UI and
the optimistic auto-skip.

## Row menus (`components/RowMenu.tsx`)

The "⋯" popover carried by every result surface — track rows, album rows,
artist rows, and all three search hero cards. One shell, three action
lists.

- **`RowMenu`** owns the trigger, placement, portalling, and dismissal.
  Callers pass entries through a render prop that receives `close`.
- **`rowMenuCoords.ts`** is the placement rule, split out so it can be
  unit-tested as arithmetic. It right-aligns the panel to the trigger,
  flips it *above* when it wouldn't fit below, and clamps into the
  viewport. The flip anchors by `bottom` rather than `top` specifically
  so the panel's real height never has to be predicted — it varies from
  3 entries (player) to ~9 (album menu), and one `MENU_H_GUESS` can't
  serve both.
- **`RowMenuItem` / `RowMenuSep` / `RowMenuExtras`** are the entry
  primitives. `RowMenuExtras` renders caller-supplied entries plus their
  trailing separator, or nothing — the Queue page uses it for reorder
  actions, which are the only touch-reachable path on phones.
- **`PlaylistPicker.tsx`** holds the "add to playlist…" submenu *and*
  `usePlaylistAdd`, the mutation shared by all three menus. The submenu
  replaces the root body in place rather than opening a second floating
  panel — nested fixed-position elements fight both outside-click
  detection and the viewport clamping above.

Two things that look incidental but aren't:

- **The panel body mounts only while open.** That's what makes the
  playlist submenu reset to the root view on every open without the
  shell knowing such state exists.
- **The menus resolve their tracklist at click time, not at render.**
  An album row only holds an `Album`; every action needs songs. Each one
  fetches through the shared `["album", id]` query key, which is the
  album page's own — so a warm cache resolves in a microtask and the
  click's transient user activation survives into `audio.play()`.
  Resolving eagerly instead would fire a `getAlbum` per visible row.

Artist menus are deliberately shorter than album menus: an artist has no
canonical tracklist (`sync/artistTracks.ts` synthesises one from top
songs, falling back to a bounded slice of the discography), which is
right for "play" and "queue" but a surprising thing to silently pin to
disk or paste into a playlist. Those stay album-level.

## RUM (`rum/`)

Browser-side performance telemetry. Two emitters feed
`POST /v1/diagnostics/client_events`:

- `web-vitals` 4.x — LCP / INP / CLS / FCP / TTFB → marks named
  `web-vital.<NAME>` with the library's `rating` bucket attached.
- `markEvent(name, {value_ms?, rating?, fields?})` — public API for
  custom marks. Currently used for `playback.start`.

Batched: 10 s interval flush via `fetch`, plus `pagehide` /
`visibilitychange→hidden` flush via `fetch(..., {keepalive: true})`.
In-memory cap of 50 events drops oldest. Session id is one per
page-load, persisted in `sessionStorage`.

## Sync

`SyncProvider` opens a WebSocket to `/v1/sync`. It receives
`Snapshot` and `Update` messages, applies them to local React
state, and re-emits via context.

Optimistic updates: when the user clicks "Like", we update local
state immediately, then fire `POST /v1/sync/ops` to the gateway.
When the gateway broadcasts the resulting `Update`, we reconcile
versions:

- Same version we have? No-op.
- Higher version? Adopt.
- Lower version? Server is behind us — should never happen, but log
  if it does.

After a PWA relaunch-from-snapshot, queue items arrive as bare ids; a
`getSong` backfill effect re-hydrates `trackMeta` so the player bar,
Media Session, and row menus aren't blank.

**Reconnect.** `onclose` schedules a retry on the backoff in
`sync/reconnect.ts` (500 ms doubling to a 30 s cap, with equal jitter so
devices that dropped together don't return together), and `online` /
`visibilitychange→visible` reconnect immediately rather than waiting the
backoff out. This is not optional polish: a phone changes network
constantly, and without it a single WiFi→cellular handover wedged the tab
on a stale snapshot permanently. Ops submitted while the socket is down
buffer in `outboxRef` and flush on the next `onopen`.

## Offline cache + PWA (`cache/`, `pwa/`)

`cache/audioCache.ts` is an IndexedDB reimplementation of the
`crates/music-cache` contract: content-addressed by `(trackId,
bitrate, codec)`, two-budget LRU (a regular auto-cached budget plus a
separate never-evicted pinned budget), `put/get/touch/pin/unpin/
listPinned/stats/evict`. Audio is stored as whole-file blobs and
served to `<audio>` via `URL.createObjectURL` — chosen over a
Service-Worker + Cache-API approach because the gateway stream
endpoint has no HTTP Range support, so the browser must seek locally
against a stored file.

Tracks around the cursor are auto-cached (regular budget); "save for
offline" pins (pinned budget) — mirroring CLI semantics.
`downloadQuality` (original | opus128 | mp3128, in `cacheSettings.ts`)
transcodes-to-fit via the `/rest/*` proxy's `format`/`maxBitRate`
params. Budget sliders live in the Storage settings panel; lowering a
cap evicts immediately.

### The download pass, and why it isn't cache-on-play

Auto-caching runs as a debounced pass over the queue window
(`DOWNLOAD_AHEAD` items ahead, `PREFETCH_BEHIND` behind), not on the
track you just started. The selection rule is
`cache/prefetchWindow.ts:downloadTargets`.

It used to fetch on play, which meant an uncached track was pulled
**twice at once** — the `<audio>` element streaming it, and the cache
downloading it — racing each other for the same link. On mobile data
that showed up as two 2,026,005-byte requests for one 2 MB song, one
carrying `access_token` (the element) and one not (the cache).

So the pass deliberately **skips the track at the cursor while it is not
blob-backed**: that means the element is streaming it right now, and
downloading it is precisely the duplicate. It becomes eligible on the
next advance, when it falls into the behind-window and its stream has
finished. Fetching one track *ahead* costs the same bytes as fetching
the current one late, except those bytes replace the next streaming
fetch instead of duplicating this one — so steady-state playback through
a queue is one fetch per track instead of two.

`DOWNLOAD_SETTLE_MS` debounces the pass so skipping through six tracks
downloads only where you land, and `cacheTrack` is inflight-deduped so
the pass, a bulk "cache liked" sweep and a manual download can't each
pull the same file.

The service worker (vite-plugin-pwa, `autoUpdate`) precaches the app
shell only — API routes are NetworkOnly and audio bypasses the SW
entirely. `pwa/installPrompt.ts` captures `beforeinstallprompt` at
module load (Chromium fires it once, early) and surfaces an "Install
as app" button in Settings; iOS gets a Share → Add to Home Screen
hint instead.

### System bars and `--safe-b`

`index.html` sets `viewport-fit=cover`, so the app paints **behind** the
system chrome — the iPhone notch and home indicator, Android's
gesture-nav pill. Nothing in the layout shrinks to avoid them: `.shell`
is a full `100dvh` grid and the player bar pads *itself* back out. That
makes the inset value load-bearing.

Every bottom-anchored rule reads `--safe-b` (`tokens.css`), never raw
`env(safe-area-inset-bottom)` — the player bar's height and padding, the
toast stack's offset, the mobile sidebar drawer. Change the token, not
the call sites.

The token exists because **Chrome on Android reports
`env(safe-area-inset-bottom): 0` for the gesture bar in an installed
PWA** while still honouring `viewport-fit=cover`: we opt into
edge-to-edge and then compensate by zero, so the pill lands on top of the
player bar (reported on a Pixel, 2026-08-03 — the whole bar was
unreachable). No API reports the real height when `env()` lies, so
`--safe-b` applies a **floor** of 32px instead, scoped to
`(display-mode: standalone) and (pointer: coarse)`:

- iOS reports a true 34px, which is larger, so `max()` leaves it alone.
- A desktop browser or an ordinary mobile tab — where Chrome insets the
  viewport itself and 0 *is* correct — never matches the query.
- 32px covers Android gesture nav (24–32dp). A device on three-button
  navigation (48dp) would still clip; bump the floor if that turns up.

The top inset is deliberately not compensated — Chrome lays the PWA out
below the status bar in practice, and padding it too would double the
gap. If content ever appears under the status bar, that's the same bug
at the other end and wants a matching `--safe-t`.

## Build

```bash
npm run build
# Outputs to apps/web/dist/
# Main bundle: ~151 KB JS gzipped; the 3-D latent-space view is
# code-split into a lazy ~246 KB chunk (three.js) loaded on demand.
```

In production the gateway serves `dist/` itself: set
`server.static_dir` in `gateway.toml` and the gateway hosts the SPA
(with an SPA fallback for client-routed paths) from the same origin —
which is what the OAuth redirect URIs and the PWA manifest/SW assume.
The docker gateway image bakes the built SPA in.

## Tests

289 Vitest tests across 32 files at last count — pure-logic helpers
(search ranking, latent-space binning, sync reducer, recommend
filter shape, scrobble/skip producers, autoplay seeds + settings,
auto-skip predicates, output-device preference, install prompt,
settings nav, row-menu placement, bulk-download outcomes,
most-played ranking) plus the IndexedDB audio-cache suite
(fake-indexeddb).
React-component tests and Playwright end-to-end suites are not yet
in. The build still runs `tsc -b` which catches refactor breakage.

```bash
cd apps/web && npx vitest run
```

## Known gaps

- **Real-device PWA verification.** Install-to-home-screen,
  airplane-mode offline launch + playback, and screen-off background
  audio can't be tested headless and are still open.
- **No keyboard shortcuts.** No spacebar-to-play, no arrow-key
  seek. Listenable to-do.
- **No accessibility audit.** Tailwind's default focus rings are
  retained, but we haven't audited keyboard navigation or
  screen-reader behaviour.
- **No internationalization.** Strings are hardcoded English.
- **No analytics in the marketing sense.** RUM is internal-only
  (feeds `/diagnostics`); no third-party trackers.
- **No React-component tests.** Vitest covers logic helpers; the
  components are eyeball-tested.
