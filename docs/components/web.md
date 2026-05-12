# web

**Path:** `apps/web/`
**Type:** Vite + React 19 single-page app
**Test count:** 72 (Vitest)

The browser client. Talks to the gateway over HTTPS for both REST
calls (`/rest/*`, `/v1/*`) and the sync WebSocket (`/v1/sync`).

## Stack

- **React 19** — concurrent rendering, server components not used (we're a client SPA).
- **TypeScript 6** — strict mode.
- **Vite 8** — build tool. Dev server on port 5173 with HMR.
- **TanStack Query 5** — request caching + retries + invalidation.
- **Tailwind 3** — styling.
- **Plain `<audio>` element** — playback. MSE (Media Source Extensions)
  for gapless playback is deferred until basic boundary handoff
  proves audibly gappy in real use.

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
    ├── api/                  # gateway client (REST + diagnostics)
    ├── auth/                 # OAuth flow + AuthContext
    ├── components/           # shared UI primitives
    ├── pages/                # route components — see below
    ├── player/               # PlayerProvider + bottom-bar UI (thumbs feedback)
    ├── rum/                  # web-vitals + markEvent + flush loop
    ├── styles/               # Tailwind + design-bundle CSS
    ├── sync/                 # WebSocket client + SyncContext
    └── utils/                # shared helpers
```

Pages today (`apps/web/src/pages/`):

- `Home`, `Albums`, `Album`, `Artists`, `Artist`, `Tracks` — catalog.
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
- `LatentSpace` / `LatentSpace3D` + `latentSpace.ts`/`.test.ts` —
  2D and 3D UMAP plot views (read
  `/v1/diagnostics/recommend/latent_space`). 2-D / 3-D toggle is
  decoupled from the colour mode.
- `diagnostics/` — the diagnostics page has been split into
  topical subpages: `DiagnosticsHome` (landing), `Recommender`
  (per-feature dashboards), `LatentSpace` (above), `Ingest` (queue
  + ingest worker state), `Tracing` (M0 span ring), `Rum` (browser
  RUM events), `Listening` (recommend-session reconstruction with
  per-session events panel).
- `SignIn`, `Callback` — OAuth PKCE flow.

## Provider tree

```tsx
<QueryClientProvider client={queryClient}>
  <AuthProvider>          {/* AuthContext.tsx — OAuth tokens, refresh */}
    <SyncProvider>        {/* WebSocket sub, optimistic state */}
      <PlayerProvider>    {/* current track, audio element */}
        <App />
      </PlayerProvider>
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
- The player-bar thumbs-up / thumbs-down buttons. Clicks POST to
  `/v1/recommend/feedback` with the active recommend session id and
  re-render with the returned `(up, down)` counts. Clicking an
  already-active thumb clears the vote.
- The `playback.start` RUM mark (measured from `src` set → first
  `playing` event — the user-perceived latency, not `loadedmetadata`
  which fires too early).

When you click play on a track:
1. Compute the stream URL: `/rest/stream?id=<track>&access_token=<...>`.
   (The token goes via query param because `<audio src>` can't set
   headers — see [API.md](../API.md#auth-model).)
2. Set `audioEl.src` to that URL. The browser starts buffering.
3. `audioEl.play()`.

Position updates fire on `timeupdate` (~4 Hz). Likes / skips /
scrobbles are batched into `/v1/events` every 5s.

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

## Build

```bash
npm run build
# Outputs to apps/web/dist/
# Bundle size: ~80 KB JS gzipped (web-vitals + diagnostics surface + station)
```

The production target is to be served by the gateway from the same
origin. That's not yet wired — the gateway only handles JSON APIs
today. To deploy, either:

1. Have the gateway serve `dist/` as static files (simple to add).
2. Serve the SPA from a separate origin (then the OAuth redirect
   URIs need updating, and CORS gets involved).

## Tests

72 Vitest tests across 6 files at last count — pure-logic helpers
(search ranking, latent-space binning, sync reducer, recommend
filter shape). React-component tests and Playwright end-to-end
suites are not yet in. The build still runs `tsc -b` which catches
refactor breakage.

```bash
cd apps/web && npx vitest run
```

## Known gaps

- **No service worker.** Offline mode, background prefetch, and
  push notifications are all blocked on a service worker. Planned
  with the L2-cache-on-the-client work.
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
