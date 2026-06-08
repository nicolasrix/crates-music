# crates-music documentation

A music-player gateway for self-hosted [Navidrome](https://www.navidrome.org/),
served to a CLI and a web client. The web client doubles as the **mobile**
client — it's an installable PWA (the native-mobile plan, P4, was retired).
Single-user, local-network-first.

This directory is the onboarding entry point. Follow the docs in this order:

## Start here

| Doc | What you'll learn |
|---|---|
| [ARCHITECTURE.md](./ARCHITECTURE.md) | The big picture: gateway, clients, caching layers, auth, recommender |
| [GETTING-STARTED.md](./GETTING-STARTED.md) | Bring up the gateway, the web app, and (optionally) the embedder on your machine |
| [CONFIGURATION.md](./CONFIGURATION.md) | Every field of `gateway.toml`, all environment variables, and what each script in `scripts/` does |
| [API.md](./API.md) | Every endpoint the gateway exposes, with example requests |
| [PLATFORM-PARITY.md](./PLATFORM-PARITY.md) | What each client (web/PWA / CLI) can do today — the feature matrix, plus mobile-PWA specifics |
| [TESTING.md](./TESTING.md) | How tests are organised; how to run a single test or the whole suite |
| [CONTRIBUTING.md](./CONTRIBUTING.md) | Dev environment setup, every command (build / test / lint / bench), branch workflow, PR checklist |
| [DEPLOYMENT.md](./DEPLOYMENT.md) | Run the gateway + embedder in containers (`docker compose`). Operator guide. |
| [RUNBOOK.md](./RUNBOOK.md) | Operational quick-reference: health checks, deploy/update, model bumps, backup/restore, common issues |

## Per-component reference

Each crate / service has a focused doc covering purpose, public API,
side effects, and notable design decisions. Use these when you're
about to touch a component:

- [music-core](./components/music-core.md) — pure domain types
- [music-subsonic](./components/music-subsonic.md) — typed Subsonic client
- [music-cache](./components/music-cache.md) — L2 metadata cache + L3 audio cache
- [music-player](./components/music-player.md) — native playback (rodio + symphonia)
- [music-sync](./components/music-sync.md) — cross-device queue/playback state machine
- [music-recommend](./components/music-recommend.md) — embedding store, ANN, ingest pipeline, event log
- [music-gateway](./components/music-gateway.md) — the HTTP gateway binary
- [music-cli](./components/music-cli.md) — the `music` CLI binary
- [embedder](./components/embedder.md) — Python FastAPI sidecar (CLaMP 3 audio + text embeddings; CLAP legacy)
- [web](./components/web.md) — React 19 + Vite single-page app

## Cross-cutting topics

The architecture doc covers all of these in context, but they each get
their own canonical statement:

- **Caching** — L1 (in-memory) → L2 (SQLite, ETag-keyed) → L3 (audio file cache, content-addressed) → L4 (gateway transcoded LRU). See [ARCHITECTURE.md#caching](./ARCHITECTURE.md#caching).
- **Auth** — OAuth 2.1: Authorization Code + PKCE (web), Device Authorization Grant / RFC 8628 (CLI, via `music auth login`). See [ARCHITECTURE.md#auth](./ARCHITECTURE.md#auth).
- **Recommender** — content embeddings (CLaMP 3, 768-dim; CLAP legacy) + ABTT whitening + ANN (usearch, cosine), with post-retrieval queue filter (MMR, per-artist cap, dedup), per-session downvote exclusion, text-query stations, and 2D/3D UMAP projections for visual debugging. See [components/music-recommend.md](./components/music-recommend.md).
- **Diagnostics** — span ring (`gateway-state.traces.sqlite`) + browser RUM + per-feature dashboards backing the `/diagnostics` page. See [ARCHITECTURE.md#diagnostics](./ARCHITECTURE.md#diagnostics) and [API.md#diagnostics](./API.md#diagnostics).

## Conventions

- The **source of truth** for endpoint shapes is the gateway code under `crates/music-gateway/src/`. The docs here describe behaviour; if you find a mismatch, the code wins and the doc is wrong.
- The **canonical project plan** (phases P0-P6 and what's in flight) lives in [`/CLAUDE.md`](../CLAUDE.md) at the repo root, not here. That file is read by Claude Code automatically; humans are welcome to read it too.
- All file paths in the docs are relative to the repo root unless stated otherwise.

## Reading order for first-time contributors

1. [ARCHITECTURE.md](./ARCHITECTURE.md) — system mental model (~10 min)
2. [GETTING-STARTED.md](./GETTING-STARTED.md) — get something running (~30 min including cert generation)
3. [components/music-gateway.md](./components/music-gateway.md) — where most code changes land
4. The component doc for whatever you're touching today
5. [API.md](./API.md) when you need to call something from a client
