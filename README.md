# crates-music

A self-hosted music player built around an unmodified
[Navidrome](https://www.navidrome.org/) library: a Rust gateway that speaks the
[Subsonic API](https://opensubsonic.netlify.app/), plus two clients — a terminal
UI and a web app that installs as a PWA.

Local-network-first. Everything runs on your own hardware; nothing phones home.

```
                    ┌──────────────────┐
                    │  Navidrome       │   (unmodified — catalog source of truth)
                    └────────▲─────────┘
                             │ Subsonic /rest/*
                    ┌────────┴─────────────────────┐
                    │  music-gateway (Rust, axum)  │
                    │  • Subsonic proxy + augments │
                    │  • OAuth 2.1 server          │
                    │  • Recommender (CLaMP 3+ANN) │
                    │  • WebSocket queue sync      │
                    │  • SQLite state              │
                    └──────┬───────────────┬───────┘
                           │               │
                ┌──────────▼──────┐ ┌──────▼────────────────────┐
                │  crates-cli     │ │  Web (React 19)           │
                │  (ratatui TUI)  │ │  └─ installable PWA       │
                └─────────────────┘ └───────────────────────────┘
```

The gateway holds anything worth sharing between clients — metadata cache,
recommender, cross-device sync. The clients stay thin.

## What it does

**Playback** — gapless native playback in the CLI (rodio + symphonia); an
`<audio>`-based player in the browser with Media Session integration for
lock-screen controls.

**Offline** — audio is cached content-addressed by `(trackId, bitrate, codec)`,
with a separate never-evicted budget for pinned tracks. The web client
reimplements the same contract in IndexedDB, so an installed PWA plays in
airplane mode.

**Recommendations** — every track is embedded once with
[CLaMP 3](https://github.com/sanderwood/clamp3) (768-dim) by a Python sidecar,
indexed in a memory-mapped [usearch](https://github.com/unum-cloud/usearch) HNSW
graph. That drives similar-track autoplay, similar albums/artists, and
natural-language stations ("boom bap hip hop") through the model's shared
text/audio embedding space. Rules-based affinity re-scores candidates from
accumulated like/skip signal.

**Multi-user** — three roles (admin / user / guest) on a hand-rolled OAuth 2.1
server: Authorization Code + PKCE for the browser, Device Authorization Grant
(RFC 8628) for the CLI, and shared-code guest grants. The Navidrome catalog is
shared; queues, taste, ratings, playlists, and history partition per user.
Guests join a host's room as ephemeral principals and never train the
recommender.

**Sync** — a single-linearizer state machine fans queue and playback state
across devices over WebSocket, with optimistic local updates.

**Diagnostics** — span ring, browser RUM (web vitals + playback latency), ingest
queue depth, and a UMAP projection of the embedding space, all behind a
`/diagnostics` page.

## Repo layout

Cargo workspace. `ls crates/` for the crate list — each crate's `lib.rs` header
says what it does.

| Path | What |
|---|---|
| `crates/` | Rust workspace: gateway, CLI, and the shared libraries |
| `apps/web/` | React 19 + Vite + Tailwind SPA (also the mobile client, as a PWA) |
| `services/embedder/` | Python FastAPI sidecar — CLaMP 3 audio + text embeddings |
| `docker/`, `docker-compose*.yml` | Container images and deployment topologies |
| `docs/` | Full documentation — start at [`docs/README.md`](./docs/README.md) |
| `scripts/` | Operational helpers (certs, backup/restore, bulk re-embed) |

Two boundaries that matter more than the tree: `music-recommend` is
**server-only** and must never be linked into a client, and clients consume
`music-core` types while talking to the gateway over HTTP/WS.

## Just want to run it?

Follow the [**tutorials**](./docs/tutorials/README.md) — a step-by-step path
from nothing to music on your phone, using containers, assuming no prior
experience. Start with
[01 — Get your music library online](./docs/tutorials/01-navidrome.md).

The rest of this README, and everything under `docs/` outside `tutorials/`
and `explainers/`, is written for developers.

## Getting started (developers)

Requires Rust 1.95 (pinned in `rust-toolchain.toml`), a current Node LTS for the
web app (Vite 8), and a reachable Navidrome instance. The recommender
additionally wants Python 3.11+ for the embedder sidecar — that sidecar is
optional, and the gateway boots in a degraded tag-similarity mode without it.

```bash
cargo build --workspace
cargo test --workspace
```

Full setup — dev certificates, gateway config, the CLI device-login flow, and
the Vite dev server — is in
[`docs/GETTING-STARTED.md`](./docs/GETTING-STARTED.md).

## Documentation

| Doc | What you'll learn |
|---|---|
| [tutorials/](./docs/tutorials/README.md) | Non-technical step-by-step setup guides |
| [explainers/](./docs/explainers/) | Plain-language background — how it works, certificates, glossary |
| [ARCHITECTURE.md](./docs/ARCHITECTURE.md) | Gateway, clients, cache layers, auth, recommender |
| [GETTING-STARTED.md](./docs/GETTING-STARTED.md) | Bring the stack up locally |
| [CONFIGURATION.md](./docs/CONFIGURATION.md) | Every config block and environment variable |
| [API.md](./docs/API.md) | Every endpoint, with example requests |
| [DEPLOYMENT.md](./docs/DEPLOYMENT.md) | Containers, reverse proxy, split-host embedder |
| [RUNBOOK.md](./docs/RUNBOOK.md) | Health checks, backups, model bumps, common failures |
| [CONTRIBUTING.md](./docs/CONTRIBUTING.md) | Dev setup, commands, branch workflow, PR checklist |
| [PLATFORM-PARITY.md](./docs/PLATFORM-PARITY.md) | Feature matrix across web/PWA and CLI |

## Status

Personal project, built in vertical slices that are each end-to-end usable.
Working today: the gateway and both clients, offline audio caching, cross-device
sync, the recommender with text stations, multi-user roles, and gateway-owned
playlists.

Not built: behavioural (track2vec) embeddings alongside the content index, a
gateway-side transcoded-audio cache, and MSE-based gapless playback on the web
(the plain `<audio>` boundary handoff has been good enough). The native Android
client was retired in favour of the PWA.

Interfaces are not stable and there is no release process — this is built for
one household's hardware, published in case the shape is useful to someone
else.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](./LICENSE-APACHE))
- MIT license ([LICENSE-MIT](./LICENSE-MIT))

at your option.

Vendored third-party code under `services/embedder/embedder/_clamp3/` carries
its own licenses — see that directory's `LICENSES/` and `VENDORED.md`.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
