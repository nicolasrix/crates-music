# crates-music documentation

A music-player gateway for self-hosted [Navidrome](https://www.navidrome.org/),
served to CLI, web, and (eventually) Android clients. Single-user,
local-network-first.

This directory is the onboarding entry point. Follow the docs in this order:

## Start here

| Doc | What you'll learn |
|---|---|
| [ARCHITECTURE.md](./ARCHITECTURE.md) | The big picture: gateway, clients, caching layers, auth, recommender |
| [GETTING-STARTED.md](./GETTING-STARTED.md) | Bring up the gateway, the web app, and (optionally) the embedder on your machine |
| [CONFIGURATION.md](./CONFIGURATION.md) | Every field of `gateway.toml`, all environment variables, and what each script in `scripts/` does |
| [API.md](./API.md) | Every endpoint the gateway exposes, with example requests |
| [TESTING.md](./TESTING.md) | How tests are organised; how to run a single test or the whole suite |

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
- [embedder](./components/embedder.md) — Python FastAPI sidecar (CLAP audio + text embeddings)
- [web](./components/web.md) — React 19 + Vite single-page app

## Cross-cutting topics

The architecture doc covers all of these in context, but they each get
their own canonical statement:

- **Caching** — L1 (in-memory) → L2 (SQLite, ETag-keyed) → L3 (audio file cache, content-addressed) → L4 (gateway transcoded LRU). See [ARCHITECTURE.md#caching](./ARCHITECTURE.md#caching).
- **Auth** — bearer token (CLI, transitional) + OAuth 2.1 with PKCE (web; device-grant for CLI later). See [ARCHITECTURE.md#auth](./ARCHITECTURE.md#auth).
- **Recommender** — content embeddings (CLAP) + ANN (usearch, cosine), with post-retrieval queue filter (MMR, per-artist cap, dedup), per-session downvote exclusion, and a 2D UMAP projection for visual debugging. See [components/music-recommend.md](./components/music-recommend.md).
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
