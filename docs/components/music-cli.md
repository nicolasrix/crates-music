# music-cli

**Path:** `crates/music-cli/`
**Type:** binary `crates-cli`, with library crate for tests

The `crates-cli` binary. Talks to either the gateway (default) or directly
to Navidrome (diagnostic mode). Manages local audio cache, plays
tracks, syncs queue across devices. Two faces:

- **Subcommands** (`crates-cli albums`, `crates-cli play <id>`, …) —
  flat, pipeable, script/agent-friendly. Plain text on a pipe.
- **Interactive TUI** — run `crates-cli` with no subcommand on a
  terminal. Full-screen browse/search/queue/stations with local
  playback and transport controls. See "Interactive mode" below.

## Install (as a client against a running gateway)

The CLI is a thin client — it needs a reachable gateway (your production
box, or a local dev one). This is the end-user install path; for bringing
up the whole dev stack from scratch see
[../GETTING-STARTED.md](../GETTING-STARTED.md).

**1. Build the binary.** From the repo root:

```bash
cargo build -p music-cli               # debug build — fine for everyday use
# …or an optimized copy on your PATH that survives `cargo clean`:
cargo install --path crates/music-cli  # installs `crates-cli` into ~/.cargo/bin
```

`cargo build` drops the binary at `<target>/debug/crates-cli`. `<target>`
is usually `target/`, but this workspace can redirect it via
`.cargo/config.toml` (`build.target-dir`) — if `./target/debug/crates-cli`
isn't there, find it with:

```bash
echo "$(cargo metadata --format-version 1 | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/crates-cli"
```

**2. Point it at a gateway.** Create `~/.config/crates-music/config.toml`:

```toml
[gateway]
url = "https://crates.example.com:8443"   # your gateway

[server]                     # optional; only used in --no-gateway direct mode
url = "http://nav.lan:4533"
username = "placeholder"
password = "placeholder"
```

TLS trust depends on the gateway's certificate:

- **Public/LAN gateway with a real cert** (e.g. Let's Encrypt, like the
  production `crates.example.com`): nothing to configure — the CLI's
  bundled webpki roots already trust it.
- **Dev gateway with an mkcert cert** (`gateway.local`): add
  `ca_cert_path = "<mkcert -CAROOT>/rootCA.pem"` under `[gateway]`. The
  CLI's HTTP stack trusts **only** its bundled roots, so running
  `mkcert -install` system-wide does *not* help here — you must name the
  CA file explicitly.

**3. Log in (once).**

```bash
crates-cli auth login
```

Prints a short code and a URL (`…/oauth/device`). Open the URL in a
browser already signed into the gateway, enter the code, approve. The CLI
stores rotating tokens in `cli-tokens.json` next to the config (`0600`)
and refreshes the access token on its own. `crates-cli auth status` shows
the state; `crates-cli auth logout` revokes + clears it.

**4. Start it.** `crates-cli` (bare, on a terminal) opens the interactive
UI; `crates-cli <subcommand>` runs the classic one-shot commands.

### Install gotchas

- **The token store format can change across versions.** If a command
  errors with `missing field … at cli-tokens.json`, the stored tokens
  predate a schema change — delete
  `~/.config/crates-music/cli-tokens.json` and `crates-cli auth login`
  again.
- **`recommend next <seed>` needs the *seed* track embedded.** Only tracks
  the ingest pipeline has processed are in the content index; a
  not-yet-embedded seed returns *"the seed track isn't embedded yet."*
  Seed from a track the recommender already surfaced (e.g. an ID from a
  `station` result) — those are guaranteed embedded. Free-text
  `station "…"` prompts have no such constraint.

## Interactive mode (TUI)

Running `crates-cli` with **no subcommand on a terminal** opens a
full-screen interactive player (ratatui). On a pipe the same invocation
prints help and exits 2, so scripts and agents never hang on it.

Up to nine sections (sidebar, keys `1`–`9` or `Tab`): **Library** (a
browse list with three modes — albums / artists / tracks, cycled with
`[`/`]` — that drills into album and artist detail panes), **Search**
(the gateway's typo-tolerant `/v1/search`), **Queue** (the play queue),
**Playlists** (gateway-owned playlist CRUD), **Stations**
(natural-language prompts), **Liked** (your ratings), **Downloads**
(the offline audio cache — budgets + pinned tracks), **Settings**
(a form for playback quality, cache budgets, autoplay drift, the account
card + sign-out, and admin cache invalidation), and — **admins only** —
**Diagnostics** (the gateway's `/v1/diagnostics/*` inspectors). The
Diagnostics slot (`9`) is hidden for non-admins: absent from the sidebar,
inert on the `9` key, skipped by `Tab`. Playback is local (rodio) through
the same L3 audio cache as `crates-cli play`, with the next track
prefetched for near-gapless handoff.

| Key | Action |
|---|---|
| `q` / `ctrl-c` | quit · `?` help overlay |
| `1..9`, `Tab` / `Shift-Tab` | switch section |
| `j` `k` / `↓` `↑`, `g` / `G`, `ctrl-d` / `ctrl-u` | list navigation |
| `[` / `]` | library browse mode: albums / artists / tracks |
| `h` / `l` | album-list kind (Library, albums mode) · result bucket (Search) |
| `Enter` | open album/artist/playlist · play from here · jump (context) |
| `e` | enqueue track / album · `P` play next |
| `S` | start a station from the open album / artist (replaces the queue) |
| `a` | add the selected track to a playlist (picker overlay) |
| `/` | search · `i` edit station prompt · `Esc` back/unfocus |
| `Space` | play/pause · `n` / `p` next/prev · `,` / `.` seek ∓10 s · `-` / `=` volume · `M` mute |
| `L` / `D` / `u` | like / dislike / unrate selection |
| `r` | recommend from now playing → enqueue |
| `y` | lyrics pane for the now-playing track (`j` `k` read · `Enter` seek to line · `R` look again) |
| `x` / `c` | queue: remove / clear upcoming |
| `J` / `K` / `T` | queue: move row down / up / to top |
| `o` | toggle audio output on this device (sync rooms) |
| `A` | toggle autoplay (keep the queue topped up with recommendations) |
| `f` / `F` | thumbs up / down on the now-playing autoplay pick |
| `d` | save the selected track offline (toggle pin; fetches if not cached) |
| `W` | bulk-download the open album/playlist · warm from liked (Downloads) |
| `E` | Downloads: evict the regular cache down to its budget |
| `N` `s` `m` `R` `X` `x` | playlists: new · shuffle-play · suggest · rename · delete · remove-track |
| `Enter` / `h` `l` | Settings: change (cycle enum / flip toggle / trigger) · adjust a number |
| `h` `l` / `[` `]` | Diagnostics: cycle sub-tab · cycle the time window |
| `Enter` | Diagnostics (Latent space): play the selected scatter point |

Notes:

- **No audio device** (headless box, no PipeWire/ALSA): the TUI still
  runs in browse-only mode with a visible notice.
- **Recommender down/degraded**: Stations shows a friendly "warming up /
  embedder offline" panel instead of an error.
- **Listening signal**: the TUI scrobbles like the web player —
  a now-playing hint at track start and a submission at min(50 %,
  4 min); tracks under 30 s never scrobble. In gateway mode, manual
  skips (next/prev, activating another track, removing/clearing the
  playing one) are batched to `POST /v1/events` with `played_ms`
  (~5 s cadence, best-effort flush on quit) to feed preference
  affinity. Natural end-of-track is not a skip.
- **Dislike auto-skip**: when the queue *advances onto* a track that is
  disliked (itself, its album, or its artist), it is skipped
  automatically — but explicitly activating a row always plays it.
- **Library** modes (`[`/`]`): **albums** (with the `h`/`l` kind
  switcher — newest / random / frequent / …), **artists** (`getArtists`,
  `Enter` opens an artist), and **tracks** (the whole library's first
  page via an empty `search3`; deeper paging is a follow-up). The
  **artist detail** pane lists the artist's albums then their top songs
  (`getArtist` + `getTopSongs`) as one selectable list — `Enter` opens
  an album or plays from a top song, `e` enqueues, `S` starts an artist
  station, `L`/`D`/`u` rate. The **album detail** pane appends a "you
  might like" footer (`similar_albums` + `similar_artists`, hydrated to
  names) as navigable rows, and `S` starts a station from the album
  (`/v1/recommend/from-any` → replaces the queue). A degraded/warming
  recommender just yields an empty footer, never an error.
- **Liked** rows for albums/artists resolve their names
  (`getAlbum`/`getArtist`) and are activatable — `Enter` opens the
  album/artist detail pane; track rows play the liked list from there.
- **Playlists** (gateway mode): the section browses gateway-owned
  playlists (`/v1/playlists/*`, private per-user; shared ones are
  read-only). List → detail → suggestions panes. In detail: `Enter`
  plays from the row, `s` shuffle-plays, `e` enqueues, `x` removes the
  selected track (optimistic; a failed write resyncs), `m` fetches
  "suggest more" tracks (`/v1/recommend/from-seeds` over the
  membership), `R` renames, `X` deletes (press twice to confirm). `a`
  on any track row (in *any* section, including the queue) opens the
  add-to-playlist picker — choose an owned playlist or create a new one.
  `N` creates an empty playlist. Guests/non-owners get an honest
  "not permitted" status line on writes; the server is the enforcement
  point.
- **Autoplay** (`A`, gateway mode — the web's "tethered drift"): when on,
  the queue never drains. A poll every tick refills whenever fewer than
  `min_upcoming` (default 5) tracks sit after the cursor, requesting the
  shortfall from `/v1/recommend/from-seeds`. Seeds are weighted like the
  web — the session anchor (3×) > user-picked queue items (2×) > earlier
  scrobbles (1×), plus a decaying "travel frontier" over the last few
  played tracks — and the leash (τ/λ) + MMR knobs ride the request so the
  drift stays tethered to what you chose. With no such seeds it falls back
  to `from-any` over the queue. An empty/degraded recommender arms a 30 s
  cooldown so it can't hot-loop; a full delivery only pauses ~1.5 s.
  Autoplay-added tracks are tracked, and `f`/`F` send a thumbs up/down
  (`/v1/recommend/feedback`, session-scoped) — a no-op with a status line
  on tracks you queued yourself; pressing the active thumb again clears it.
  A `✦` badge in the now-playing bar shows autoplay is on, and the pick's
  current vote shows next to the title. All drift params live in
  `[tui.autoplay]` and are editable in the Settings section (below).
- **Lyrics** (`y`, gateway mode): the now-playing track's lyrics over the
  main pane, the line being sung highlighted, `Enter` seeking to the line
  under the cursor. No LRC parsing here — the gateway normalizes every
  source into `{start_ms, text}` lines, so `/v1/lyrics/:id` is the whole
  client. Three properties worth knowing:
  - **The pane is permeable**, unlike the help and picker overlays. It
    claims only `j`/`k`/`Enter`/`R`; everything else (transport, volume,
    rating) falls through to the global keymap, because the pane is meant
    to be left open while you listen.
  - **It follows the playhead until you scroll.** `j`/`k` start from
    whatever line is lit, and stop it following; `Enter` seeks there and
    resumes. A track change resumes following from the top.
  - **The highlight is derived, not stored.** It is recomputed from the
    playback snapshot each 250 ms tick — four binary searches a second
    over a list of at most a few hundred lines, which is cheaper than
    keeping a cached index correct across seeks. The lead constant
    (`HIGHLIGHT_LEAD_MS`) is half a tick, so the error is centred rather
    than always late.

  Fetching happens only while the pane is open (the web gates prefetch the
  same way): resolving lyrics for every track played would send the whole
  listening history's titles to lrclib.net. A confirmed absence, an
  unreachable provider and a disabled gateway are three different screens,
  and `R` re-resolves for the case where a fuzzy match landed on the wrong
  song. **No timing-offset nudge here** — that one is web-only, since the
  TUI has no per-track local store to persist it in.
- **Sync room** (gateway mode): the TUI joins the account's `/v1/sync`
  room on start, so its queue is the *same* queue the web/PWA shows —
  reorder, skip, and play-from-here converge live across devices. Queue
  gestures submit ops over the WebSocket (echo-driven, mirroring the
  web); the local queue is a projection of the server-confirmed state.
  A header badge shows the state: `◉ synced`, `◉ synced · silent` (this
  device is a remote making no sound — toggle with `o`), or `⚠ sync
  offline`. **Degraded mode**: if the WS drops, the queue keeps working
  locally and the task reconnects with backoff (capped at 30 s),
  adopting the server snapshot on reconnect — better than the web, which
  has no reconnect. Picking a track on this device turns its audio on;
  `o` toggles "play audio on this device" so a terminal can drive the
  room as a silent remote. In direct-Subsonic mode (no `[gateway]`)
  there's no room and the queue is purely local.
- **Downloads / offline** (section 7): the same L3 audio cache as the
  classic `pin`/`pinned`/`cache` subcommands, surfaced as two budget
  gauges (regular vs pinned, styled like the progress bar) over storage
  totals and a pinned-track table (`Enter` plays it — offline-capable,
  keyed on the track id, so unhydrated rows still play). `d` on *any*
  track row toggles save-offline (fetch-if-missing, then pin; a second
  `d` un-saves); `W` bulk-downloads the open album or playlist (tolerant
  per track — the status line tallies saved / already-offline / failed),
  or warms the cache from every liked track when pressed in Downloads;
  `E` evicts the regular cache to its budget. Pinned tracks live in a
  separate budget and are never LRU-evicted; auto-cached (played) tracks
  ride the regular budget. When the sync WS is down, a `⚠ offline` badge
  appears at the bottom of the sidebar — a reminder that pinned tracks
  still play with no gateway.
- **Settings** (section 8): a form grouped into Playback / Storage /
  Autoplay / Account / Admin. `Enter` cycles an enum, flips a toggle, or
  triggers an action row; `h`/`l` adjust a number. Every edit persists to
  the config file (`[playback]`, `[cache]`, `[tui.autoplay]`) *and*
  applies live — a stream-quality change hits the next fetch, a lowered
  budget evicts immediately, drift params reach the next refill.
  Quality is `original` / `opus @128k` / `mp3 @128k`, riding the
  `/rest/*` proxy's `format`/`maxBitRate` (Navidrome transcodes on
  demand); stream and download quality are separate (content-addressed
  into distinct cache entries). The Account card shows the whoami
  identity + server URL and a two-step **sign out** that revokes the
  refresh token, clears the token store, and drops back to the shell with
  a `crates-cli auth login` reminder. Admins additionally get **invalidate
  gateway cache** (`POST /v1/admin/cache/invalidate`); non-admins never
  see that row (role from `/v1/whoami`, refreshed on entry).
- **Diagnostics** (section 9, **admin-only**): a live read-only view over
  the gateway's `/v1/diagnostics/*` inspectors, mirroring the web
  `/diagnostics` page. Six sub-tabs cycled with `h`/`l`: **Ingest**
  (`queue_depth` counts), **Recommender** (`recommend/queue_fill` bars +
  `shortfall` reasons + `similarity` quantiles + `top_results`
  leaderboard), **Listening** (`recently_played` with repeat counts),
  **Tracing** (`histogram` with inline p50/95/99 bars + a `traces` span
  tree), **Latent space** (`recommend/latent_space` as a braille scatter
  coloured by genre; `j`/`k` move the highlighted point, its
  `latent_neighbours` show in a side list, `Enter` plays it), and
  **Client events** (the browser RUM ring). `[`/`]` cycle the time window
  (15m / 1h / 24h / all) for the `since_ms` inspectors. The active tab
  auto-refreshes on the 250 ms tick, throttled to ~5 s (matching the web
  `refetchInterval`), keeping the last data on screen during a refetch.
  The whole section is hidden for non-admins — the gateway 403s the
  endpoints regardless, so the UI gate is UX, not security.
- **MPRIS media keys** (Linux): the TUI exports
  `org.mpris.MediaPlayer2.cratesmusic` over the D-Bus session bus, so
  desktop play/pause/next/previous media keys and now-playing widgets
  (GNOME Shell, KDE Plasma, `playerctl`, lock screens) drive and observe
  it — the desktop analog of the PWA's Media Session. Incoming D-Bus calls
  become the same reducer messages as keypresses; the current track,
  status, and seek position are published outward. Runs on a dedicated
  thread (the `mpris-server` `Player` is `!Send`), degrades silently with
  no session bus (headless / SSH), and is compiled out entirely on
  non-Linux targets. No configuration needed.
- **Grapheme-aware editing**: text fields (search, rename, guest code)
  move and delete by extended grapheme cluster, so combining marks and
  emoji ZWJ sequences behave as one character.
- **Logs**: the TUI silences stderr logging (it would corrupt the
  screen). Set `CRATES_CLI_LOG=/path/to/file` to capture tracing output.
- `NO_COLOR=1` switches the TUI to a monochrome theme.

## Usage (subcommands)

```bash
crates-cli ping                                      # smoke-test the connection
crates-cli albums [--size 20] [--kind newest|...]    # list albums
crates-cli album <album_id>                          # show one album with its tracks

crates-cli play <track_id> [<track_id>...]           # stream + play; multiple IDs play gaplessly
crates-cli play <track_id> --offline                 # play only if every track is in L3

crates-cli pin <track_id>                            # never-evict; auto-fetches if not cached
crates-cli unpin <track_id>
crates-cli pinned                                    # list pinned tracks

crates-cli cache stats                               # bytes used vs budgets
crates-cli cache evict                               # force fit-to-budget

crates-cli sync state                                # GET /v1/sync/snapshot, print as JSON
crates-cli sync push <track_id> [<track_id>...]      # append to the synced queue
crates-cli sync watch                                # WebSocket sub; print every frame as JSON

crates-cli playlist list                             # your playlists (+ shared), newest first
crates-cli playlist show <id>                        # playlist tracks (hydrated via getSong)
crates-cli playlist create <name>                    # create empty; prints the new id
crates-cli playlist rename <id> <name>               # rename (owner-only)
crates-cli playlist delete <id>                      # delete (owner-only)
crates-cli playlist add <id> <track_id>...           # append tracks (already-present ids are skipped, and reported)
crates-cli playlist remove <id> <track_id>           # drop every occurrence of a track
crates-cli playlist play <id> [--shuffle]            # stream + play locally, gaplessly
```

Subcommands are added via `clap` with `#[derive(Parser)]`. The
canonical definition is in `src/cli.rs`.

## Layout

```
crates/music-cli/
├── src/
│   ├── main.rs       # entry: bare-TTY → TUI, else subcommands; tracing routing
│   ├── lib.rs        # re-exports for tests
│   ├── app.rs        # subcommand dispatcher; client/cache constructors
│   ├── cli.rs        # clap definitions
│   ├── api/          # typed /v1 fetchers shared by classic commands + TUI
│   │                 #   mod (ratings/events/search/whoami/sync) · recommend · playlists · lyrics · diagnostics
│   ├── config.rs     # ~/.config/crates-music/config.toml loader
│   ├── format.rs     # plain-text table formatters
│   ├── gateway.rs    # gateway HTTP plumbing (TLS, endpoints, ws URLs)
│   ├── ratings.rs / recommend.rs / playlist.rs / sync.rs  # printing wrappers per command
│   ├── auth/         # OAuth device flow + token store
│   └── tui/          # interactive mode (see "Interactive mode")
│       ├── mod.rs        # event loop (crossterm EventStream + tokio select)
│       ├── keymap.rs     # key → semantic Msg table (renders the ? overlay)
│       ├── autoplay.rs   # pure tethered-drift seed weighting (port of autoplaySeeds.ts)
│       ├── update/       # pure reducer: (App, Msg) → Vec<Effect>
│       │                 #   mod (dispatch) · browse · library · playback · room · playlists · autoplay · downloads · settings · diagnostics · lyrics
│       ├── effects.rs    # tokio tasks per Effect, completions come back as Msgs
│       │   ├── refill.rs # autoplay from-seeds/from-any refill orchestration
│       │   ├── downloads.rs # pin / unpin / bulk / warm / evict cache ops
│       │   └── settings.rs # persist+apply config edits · sign-out · cache invalidate · LiveSettings cell
│       ├── sync_ws.rs    # session-lived sync WebSocket task (reconnect/backoff)
│       ├── lyrics.rs    # pure active-line search + pane window (port of activeLine.ts)
│       ├── state.rs / msg.rs / render.rs / theme.rs / terminal.rs
│       ├── views/        # library · search · queue · playlists · stations · liked · downloads · settings · diagnostics · lyrics · help
│       └── widgets/      # now-playing bar, sidebar (offline badge), input field
└── tests/            # clap parsing, config, formatters
```

## Why a thin binary + library

`main.rs` only decides entry mode (TUI vs subcommand) and where tracing
goes; everything else lives in the library crate so tests can drive the
same code paths without spawning a subprocess.

## Config

Loads from `~/.config/crates-music/config.toml` (XDG) by default;
overridable via `--config <path>`. Sections:

```toml
[server]                    # for direct-to-Navidrome mode
url = "http://nav.lan:4533"
username = "alice"
password = "wonderland"

[gateway]                   # for through-the-gateway mode (default)
url = "https://gateway.local:8443"
# ca_cert_path = "~/.local/share/mkcert/rootCA.pem"  # trust the gateway's CA
# insecure_tls = false                               # debug-only TLS bypass

[playback]                  # transcode quality (optional; edited by the Settings section)
# stream_quality = "original"    # original | opus128 | mp3128 — for tracks fetched to play
# download_quality = "original"  # …and for tracks fetched to save offline

[cache]                     # on-disk audio cache budgets (edited by the Settings section)
# regular_budget_bytes = 10737418240   # 10 GiB — LRU-evicted
# pinned_budget_bytes  = 5368709120    # 5 GiB  — never LRU-evicted

[tui.autoplay]              # tethered-drift autoplay (all optional; web defaults)
# enabled = false          # start with autoplay on
# min_upcoming = 5         # refill when fewer than this sit after the cursor
# leash_tau = 0.28         # boundary leash radius τ / strength λ
# leash_lambda = 16.0
# frontier_weight = 0.15   # travel frontier base weight / decay / window
# frontier_decay = 0.55
# frontier_window = 3
# mmr_lambda = 0.8         # server-side MMR relevance/novelty tradeoff
```

There is **no** `bearer_token` field. Gateway auth is the OAuth 2.1
Device Authorization Grant (RFC 8628): run `crates-cli auth login` once —
it prints a short code + URL, you approve it in a logged-in browser, and
the CLI stores rotating tokens in `cli-tokens.json` next to the config
(`0600`), refreshing them automatically. `crates-cli auth status` /
`crates-cli auth logout` inspect and clear that store.

If both sections are present, `[gateway]` wins. To force direct
mode, pass `--no-gateway`.

**Gateway TLS trust.** The gateway serves an mkcert-issued cert. Rather
than running `mkcert -install` system-wide on every client, point
`ca_cert_path` at the mkcert root CA (`mkcert -CAROOT`/`rootCA.pem`) — the
CLI adds it as an extra trust anchor on top of the system store and
verifies the cert normally. `insecure_tls = true` disables verification
entirely (loud warning, re-exposes the bearer token to a MITM) and exists
only for throwaway/debug setups — prefer `ca_cert_path`.

> "[server] creds are kept so you can flip between gateway and
> direct mode without rewriting them."
> *— `CLAUDE.md`*

The cache directory is similarly XDG: `~/.cache/crates-music/`.

## Output formatting

`format.rs` builds hand-padded plain-text tables (`albums_table`,
`tracks_table`, `artists_table`, plus one-line headers). No colour, no
extra deps — pipe-friendly first, and the string-in/string-out functions
are trivially testable. (The interactive TUI has its own ratatui
rendering and does not go through `format.rs`.)

## Sync subcommand

`crates-cli sync` mirrors the gateway's shared queue + playback state:

- `sync state` — `GET /v1/sync/snapshot`, printed as JSON.
- `sync queue` — the synced queue as a table (position, now-playing
  marker, item id, resolved title).
- `sync push <ids...>` — append tracks to the shared queue.
- `sync remove <item_id>` / `sync move <item_id> <idx>` (alias
  `reorder`) — edit by item id.
- `sync jump <index>` — move the shared now-playing cursor.
- `sync clear` — empty the queue and reset shared playback.
- `sync watch` — WebSocket subscription; prints every server frame as
  JSON, one per line.

Implementation in `sync.rs` uses `tokio-tungstenite`.

## Auth: OAuth Device Authorization Grant (RFC 8628)

The CLI authenticates to the gateway with the **Device Authorization
Grant**. `crates-cli auth login` prints a short code + URL; the user
approves it in a logged-in browser; the CLI receives a per-device refresh
token and stores rotating tokens in `cli-tokens.json` next to the config
(`0600`). The access token refreshes automatically; `crates-cli auth
status` shows token state and `crates-cli auth logout` revokes + clears
them.

The old static `[gateway].bearer_token` shared secret has been
**removed** — each device now holds its own revocable refresh token.

### Guest join

`crates-cli auth guest <code> [--name <display>]` redeems a shared guest
code (`POST /oauth/guest`) to join a host's room. No browser and no prior
login — the code *is* the credential. The grant returns a **single access
token with no refresh** (see D4), stored in `cli-tokens.json` with a
`kind: "guest"` marker so the token store never attempts a refresh grant
for it. When the session lapses at its TTL, `resolve_bearer` fails with a
friendly *"guest session expired — redeem a new code"* instead of a
refresh loop; the visitor runs `auth guest` again. `auth status`
distinguishes a guest session (and its remaining minutes); `auth logout`
just clears the local store (a guest has no refresh token to revoke, and
its server-side row is GC'd at expiry).

Guests join the host's sync room server-side (nothing client-side to do).
Role-gated writes (ratings, playlist edits) are refused by the gateway
with `403 → ApiError::Forbidden`, which the CLI and TUI surface as an
honest "not permitted" message — the server is the enforcement point; the
UI gating is UX, not security.

## Tests

- `tests/cli_parser.rs` — clap parsing, including bare invocation
  parsing to "no subcommand".
- `tests/config.rs` — config file parsing edge cases and defaults.
- `tests/format.rs` — table formatter output.
- Inline unit tests — `api/` DTO parsing, `gateway.rs` URL helpers,
  and the whole TUI core: reducer state transitions (stale-generation
  drops, prefetch handoff, rating rollback, lyric follow/read/seek),
  keymap dispatch (input-focus swallowing), the input field, the pure
  active-line search, and a `TestBackend` render smoke test (which
  includes the lyric pane at 20×6, where the window arithmetic would
  panic if it could).

Notable: tests don't actually play audio or open a real terminal.
Transport-queue logic lives in `music-player::PlayQueue` (tested
there); the audio thread and real rendering stay manual.

## Known gaps

- **No completions yet.** `clap` supports `clap_complete` for shell
  completions; we just haven't wired it.
- **Tracks mode is one page** — the library-browse "tracks" mode loads
  only the first `search3` page; deeper paging and the web's
  recent/most-played/random highlight sub-kinds are a follow-up.
- **Autoplay provenance is by track id**, not the web's per-queue-item
  id (the terminal client doesn't carry stable item ids). The set is
  pruned to what's still in the queue on each refill, so a track only
  counts as an autoplay pick while it (and no user-queued copy) sits in
  the queue — a small fidelity gap versus item-id keying.
- **Autoplay tops up a *playing* queue.** If the queue fully drains in
  local (WS-offline) mode — e.g. during a sustained recommender outage —
  there's no cursor to seed from and refills pause; press play to
  resume and autoplay resumes with it. Online (sync-room) the cursor
  stays on the last track, so this doesn't arise.
- **Autoplay drift params are config-only** until Phase 7 adds the
  Settings view; runtime `A` toggling isn't persisted back to disk.
- **Lyrics have no offline copy and no timing nudge.** The web persists a
  downloaded track's lyrics into IndexedDB and remembers a per-track
  ±0.25 s offset; the TUI does neither, so its pane needs the gateway and
  cannot correct an LRC that drifts. Both want somewhere per-track and
  local to write, which the CLI doesn't have yet.
