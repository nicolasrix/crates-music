# music-cli

**Path:** `crates/music-cli/`
**Type:** binary `music`, with library crate for tests
**Test count:** 31

The `music` CLI. Talks to either the gateway (default) or directly
to Navidrome (diagnostic mode). Manages local audio cache, plays
tracks, syncs queue across devices.

## Install (as a client against a running gateway)

The CLI is a thin client — it needs a reachable gateway (your production
box, or a local dev one). This is the end-user install path; for bringing
up the whole dev stack from scratch see
[../GETTING-STARTED.md](../GETTING-STARTED.md).

**1. Build the binary.** From the repo root:

```bash
cargo build -p music-cli               # debug build — fine for everyday use
# …or an optimized copy on your PATH that survives `cargo clean`:
cargo install --path crates/music-cli  # installs `music` into ~/.cargo/bin
```

`cargo build` drops the binary at `<target>/debug/music`. `<target>` is
usually `target/`, but this workspace can redirect it via
`.cargo/config.toml` (`build.target-dir`) — if `./target/debug/music`
isn't there, find it with:

```bash
echo "$(cargo metadata --format-version 1 | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')/debug/music"
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
music auth login
```

Prints a short code and a URL (`…/oauth/device`). Open the URL in a
browser already signed into the gateway, enter the code, approve. The CLI
stores rotating tokens in `cli-tokens.json` next to the config (`0600`)
and refreshes the access token on its own. `music auth status` shows the
state; `music auth logout` revokes + clears it.

**4. (Optional) Add a shorter alias.** The binary path can be long:

```fish
# fish:
alias crates-cli '<path-to>/music'
funcsave crates-cli
```

```bash
# bash / zsh — add to ~/.bashrc or ~/.zshrc:
alias crates-cli='<path-to>/music'
```

### Install gotchas

- **The token store format can change across versions.** If a command
  errors with `missing field … at cli-tokens.json`, the stored tokens
  predate a schema change — delete
  `~/.config/crates-music/cli-tokens.json` and `music auth login` again.
- **`recommend next <seed>` needs the *seed* track embedded.** Only tracks
  the ingest pipeline has processed are in the content index; a
  not-yet-embedded seed returns *"the seed track isn't embedded yet."*
  Seed from a track the recommender already surfaced (e.g. an ID from a
  `station` result) — those are guaranteed embedded. Free-text
  `station "…"` prompts have no such constraint.

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
# ca_cert_path = "~/.local/share/mkcert/rootCA.pem"  # trust the gateway's CA
# insecure_tls = false                               # debug-only TLS bypass
```

There is **no** `bearer_token` field. Gateway auth is the OAuth 2.1
Device Authorization Grant (RFC 8628): run `music auth login` once — it
prints a short code + URL, you approve it in a logged-in browser, and the
CLI stores rotating tokens in `cli-tokens.json` next to the config
(`0600`), refreshing them automatically. `music auth status` /
`music auth logout` inspect and clear that store.

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

## Auth: OAuth Device Authorization Grant (RFC 8628)

The CLI authenticates to the gateway with the **Device Authorization
Grant**. `music auth login` prints a short code + URL; the user approves
it in a logged-in browser; the CLI receives a per-device refresh token
and stores rotating tokens in `cli-tokens.json` next to the config
(`0600`). The access token refreshes automatically; `music auth status`
shows token state and `music auth logout` revokes + clears them.

The old static `[gateway].bearer_token` shared secret has been
**removed** — each device now holds its own revocable refresh token.

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
