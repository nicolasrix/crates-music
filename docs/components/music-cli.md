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

Five sections (sidebar, keys `1`–`5` or `Tab`): **Library** (albums with
a kind switcher, drill into an album), **Search** (the gateway's
typo-tolerant `/v1/search`), **Queue** (the play queue),
**Stations** (natural-language prompts) and **Liked** (your ratings).
Playback is local (rodio) through the same L3 audio cache as
`crates-cli play`, with the next track prefetched for near-gapless
handoff.

| Key | Action |
|---|---|
| `q` / `ctrl-c` | quit · `?` help overlay |
| `1..5`, `Tab` / `Shift-Tab` | switch section |
| `j` `k` / `↓` `↑`, `g` / `G`, `ctrl-d` / `ctrl-u` | list navigation |
| `h` / `l` | album-list kind (Library) · result bucket (Search) |
| `Enter` | open album · play from here · jump (context) |
| `e` | enqueue track / album · `P` play next |
| `/` | search · `i` edit station prompt · `Esc` back/unfocus |
| `Space` | play/pause · `n` / `p` next/prev · `,` / `.` seek ∓10 s · `-` / `=` volume |
| `L` / `D` / `u` | like / dislike / unrate selection |
| `r` | recommend from now playing → enqueue |
| `x` / `c` | queue: remove / clear upcoming |
| `J` / `K` / `T` | queue: move row down / up / to top |
| `o` | toggle audio output on this device (sync rooms) |

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
│   ├── api.rs        # typed /v1 fetchers (ratings, recommend, search) shared
│   │                 #   by classic commands and the TUI
│   ├── config.rs     # ~/.config/crates-music/config.toml loader
│   ├── format.rs     # plain-text table formatters
│   ├── gateway.rs    # gateway HTTP plumbing (TLS, endpoints, ws URLs)
│   ├── ratings.rs / recommend.rs / sync.rs   # printing wrappers per command
│   ├── auth/         # OAuth device flow + token store
│   └── tui/          # interactive mode (see "Interactive mode")
│       ├── mod.rs        # event loop (crossterm EventStream + tokio select)
│       ├── keymap.rs     # key → semantic Msg table (renders the ? overlay)
│       ├── update.rs     # pure reducer: (App, Msg) → Vec<Effect>
│       ├── effects.rs    # tokio tasks per Effect, completions come back as Msgs
│       ├── state.rs / msg.rs / render.rs / theme.rs / terminal.rs
│       ├── views/        # library · search · queue · stations · liked · help
│       └── widgets/      # now-playing bar, sidebar, input field
└── tests/            # clap parsing, config, formatters
```

## Why a thin binary + library

`main.rs` only decides entry mode (TUI vs subcommand) and where tracing
goes; everything else lives in the library crate so tests can drive the
same code paths without spawning a subprocess.

## Config

Loads from `~/.config/crates-music/config.toml` (XDG) by default;
overridable via `--config <path>`. Two sections:

```toml
[server]                    # for direct-to-Navidrome mode
url = "http://nav.lan:4533"
username = "alice"
password = "wonderland"

[gateway]                   # for through-the-gateway mode (default)
url = "https://gateway.local:8443"
# ca_cert_path = "~/.local/share/mkcert/rootCA.pem"  # trust the gateway's CA
# insecure_tls = false                               # debug-only TLS bypass
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

## Tests

- `tests/cli_parser.rs` — clap parsing, including bare invocation
  parsing to "no subcommand".
- `tests/config.rs` — config file parsing edge cases and defaults.
- `tests/format.rs` — table formatter output.
- Inline unit tests — `api.rs` DTO parsing, `gateway.rs` URL helpers,
  and the whole TUI core: reducer state transitions (stale-generation
  drops, prefetch handoff, rating rollback), keymap dispatch
  (input-focus swallowing), the input field, and a `TestBackend`
  render smoke test.

Notable: tests don't actually play audio or open a real terminal.
Transport-queue logic lives in `music-player::PlayQueue` (tested
there); the audio thread and real rendering stay manual.

## Known gaps

- **No completions yet.** `clap` supports `clap_complete` for shell
  completions; we just haven't wired it.
- **TUI has no artist view** — Enter on a search-result artist just
  points you at their albums.
