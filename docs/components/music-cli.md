# music-cli

**Path:** `crates/music-cli/`
**Type:** binary `music`, with library crate for tests
**Test count:** 31

The `music` CLI. Talks to either the gateway (default) or directly
to Navidrome (diagnostic mode). Manages local audio cache, plays
tracks, syncs queue across devices.

## Usage

```bash
music ping                                      # smoke-test the connection
music albums [--size 20] [--kind newest|...]    # list albums
music album <album_id>                          # show one album with its tracks

music play <track_id> [<track_id>...]           # stream + play; multiple IDs play gaplessly
music play <track_id> --offline                 # play only if every track is in L3

music pin <track_id>                            # never-evict; auto-fetches if not cached
music unpin <track_id>
music pinned                                    # list pinned tracks

music cache stats                               # bytes used vs budgets
music cache evict                               # force fit-to-budget

music sync state                                # GET /v1/sync/snapshot, print as JSON
music sync push <track_id> [<track_id>...]      # append to the synced queue
music sync watch                                # WebSocket sub; print every frame as JSON
```

Subcommands are added via `clap` with `#[derive(Parser)]`. The
canonical definition is in `src/cli.rs`.

## Layout

```
crates/music-cli/
├── src/
│   ├── main.rs       # 5-line wrapper that calls app::run
│   ├── lib.rs        # re-exports for tests
│   ├── app.rs        # the actual entrypoint: dispatches subcommands
│   ├── cli.rs        # clap definitions
│   ├── config.rs     # ~/.config/crates-music/config.toml loader
│   ├── format.rs     # output formatters (table, JSON, plain)
│   └── sync.rs       # WebSocket sync client
└── tests/
    ├── config.rs
    ├── format.rs
    └── …             # 3 files
```

## Why a thin binary + library

`main.rs` is just:

```rust
fn main() -> anyhow::Result<()> {
    music_cli::app::run()
}
```

Everything else lives in the library crate so integration tests can
drive the same code path without spawning a subprocess. Tests
construct args programmatically and call `app::run_with_args(args)`.

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
bearer_token = "..."
```

If both sections are present, `[gateway]` wins. To force direct
mode, pass `--no-gateway`.

> "[server] creds are kept so you can flip between gateway and
> direct mode without rewriting them."
> *— `CLAUDE.md`*

The cache directory is similarly XDG: `~/.cache/crates-music/`.

## Output formatting

`format.rs` exposes three formatters:

- `format::table` — `tabled`-backed pretty tables. Default.
- `format::json` — for scripts (`music albums list -o json | jq ...`).
- `format::plain` — line-oriented, one record per line, tab-separated
  fields.

Selected via `-o {table,json,plain}` (or via the implicit "is stdout
a TTY" check if you don't pass `-o`).

## Sync subcommand

Three subcommands under `music sync`:

- `music sync state` — `GET /v1/sync/snapshot`, prints the current
  snapshot as JSON. Quick "what does the gateway think the state is"
  check.
- `music sync push <ids...>` — appends tracks to the shared queue.
  Useful from a CLI on one device when you want playback to start
  from another.
- `music sync watch` — opens a WebSocket to `/v1/sync` and prints
  every server message as JSON, one frame per line. Useful for
  debugging sync issues from another device, or piping into `jq`.

Implementation in `sync.rs` uses `tokio-tungstenite`.

## Bearer token only (for now)

The CLI uses the static bearer from `gateway.toml` (and
`~/.config/crates-music/config.toml`). OAuth Device Grant (RFC 8628)
is the planned replacement at P4 — the user runs `music auth login`,
the CLI prints a URL + code, the user confirms in their browser, the
CLI gets a per-device refresh token.

Until then, the bearer is a shared secret. Every device the user
uses the CLI on has the same token. Acceptable at single-user
home-network scale; not acceptable at multi-user scale.

## Tests

31 tests, all in `tests/`:

- `config.rs` — config file parsing edge cases, defaults, env-var
  overrides.
- `format.rs` — formatter output for each format flag.
- `app.rs`-style tests — drive `app::run_with_args(["music",
  "albums", "list"])` against a `wiremock`-stubbed gateway, assert
  on stdout.

Notable: tests don't actually play audio. Player tests live in
`music-player`'s suite; CLI tests stop at "the right gateway calls
were made" or "the formatter produced the right string."

## Known gaps

- **No interactive UI.** `music play` is fire-and-forget. There's no
  TUI for browsing-while-playing. A `ratatui`-based mode is plausible
  future work.
- **No completions yet.** `clap` supports `clap_complete` for shell
  completions; we just haven't wired it.
- **`music auth` doesn't exist.** Login is "edit your config file."
  This becomes a real subcommand at P4.
