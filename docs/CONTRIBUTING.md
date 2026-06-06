# Contributing

How to set up a dev environment, which commands to run, and what to
check before opening a PR. For the system mental model read
[ARCHITECTURE.md](./ARCHITECTURE.md) first; for a guided first-run see
[GETTING-STARTED.md](./GETTING-STARTED.md).

## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | `1.95.0` (pinned) | `rust-toolchain.toml` pins the exact version with `rustfmt` + `clippy` — rustup picks it up automatically. Bump deliberately, paired with a `cargo fmt` + clippy pass. |
| Node.js | 20+ | For `apps/web` (Vite 8 / React 19). |
| Python | 3.11+ | For `services/embedder` and `docker/gateway/gen_config.py`. [`uv`](https://docs.astral.sh/uv/) recommended. |
| mkcert | any | One-time TLS cert generation: `./scripts/dev-certs.sh`. |
| Docker + compose | any recent | Only needed for container deploys (see [DEPLOYMENT.md](./DEPLOYMENT.md)). |

There is no CI — every check below runs locally, and the expectation is
that you run them before pushing.

## Commands

<!-- AUTO-GENERATED: sources = Cargo.toml, apps/web/package.json, services/embedder/pyproject.toml -->

### Rust workspace (repo root)

| Command | Description |
|---|---|
| `cargo build --workspace` | Build all eight crates. |
| `cargo test --workspace` | Unit + integration tests across the workspace. |
| `cargo fmt --all` | Format (config in `rustfmt.toml`: `max_width = 100`, field-init + try shorthand). |
| `cargo clippy --workspace --all-targets` | Lint. The workspace sets `clippy::pedantic = warn` and `unsafe_code = forbid` — new warnings are regressions. |
| `cargo bench --workspace` | All Criterion suites (`server_timing`, `ann`, `embedder_client`, `trace_store`). Criterion diffs against the previous run and prints a `change: …` regression line. |
| `cargo bench --bench ann -p music-recommend` | One suite, faster feedback. Append `-- --quick` while iterating (never for regression verdicts). |
| `cargo run -p music-gateway -- --config gateway.toml` | Run the gateway locally. |
| `cargo run -p music-cli -- <subcommand>` | Run the `music` CLI. |

### Web app (`apps/web/`)

| Command | Description |
|---|---|
| `npm run dev` | Vite dev server on `http://localhost:5173`, proxying `/oauth`, `/rest`, `/v1` to the gateway (see `vite.config.ts`). |
| `npm run build` | Type-check (`tsc -b`) + production build (includes the PWA service worker). |
| `npm run lint` | Type-check only (`tsc -b --noEmit`). |
| `npm run test` | Vitest, single run (e.g. the IndexedDB audio-cache suite, via `fake-indexeddb`). |
| `npm run test:watch` | Vitest in watch mode. |
| `npm run preview` | Serve the production build locally. |

### Embedder sidecar (`services/embedder/`)

| Command | Description |
|---|---|
| `uv sync` | Install with the stub backend only — no torch, fast. The default for dev/test. |
| `uv sync --extra clamp3` | Real CLaMP 3 backend (torch ROCm wheels, transformers, sentencepiece). |
| `uv sync --extra clap` | Legacy LAION CLAP backend. |
| `uv run uvicorn embedder.app:app --port 9000` | Run the sidecar (stub by default; select with `EMBEDDER_BACKEND`). |
| `uv run --extra dev pytest` | Test suite (benchmarks excluded by default). |
| `uv run --extra dev pytest -m benchmark` | pytest-benchmark suites. `--benchmark-autosave` / `--benchmark-compare` to diff runs. |

### Other test suites

| Command | Description |
|---|---|
| `cd docker/gateway && pytest` | Tests for `gen_config.py` (env → `gateway.toml` templating) and the container entrypoint. |
| `./scripts/tests/backup-roundtrip.sh` | Backup → restore → verify drill for `scripts/backup.sh` / `restore.sh`. |

<!-- END AUTO-GENERATED -->

## Development workflow

1. **Branch** off `dev`: feature branches merge into `dev` (QA), and
   `dev` merges into `main`. Don't push directly to `main`.
2. **Write tests with the change.** Test layout and conventions are in
   [TESTING.md](./TESTING.md). Integration tests live per-crate under
   `crates/<name>/tests/`; web unit tests sit next to the module they
   cover.
3. **Before pushing**, run the checks for everything you touched:

   ```bash
   cargo fmt --all && cargo clippy --workspace --all-targets && cargo test --workspace
   (cd apps/web && npm run lint && npm run test && npm run build)
   (cd services/embedder && uv run --extra dev pytest)
   ```

4. **Commit messages** follow conventional commits:
   `<type>(<scope>): <description>` with types
   `feat fix refactor docs test chore perf ci` — e.g.
   `fix(oauth): absolute verification_uri in device_authorization`.

## PR checklist

- [ ] `cargo fmt --all` produces no diff
- [ ] `cargo clippy --workspace --all-targets` introduces no new warnings
- [ ] `cargo test --workspace` passes
- [ ] Web touched → `npm run lint`, `npm run test`, `npm run build` pass
- [ ] Embedder touched → `uv run --extra dev pytest` passes
- [ ] Hot path touched (`music-recommend`, trace store) → relevant `cargo bench` suite shows no regression
- [ ] Endpoint shapes changed → [API.md](./API.md) updated
- [ ] Config / env vars changed → [CONFIGURATION.md](./CONFIGURATION.md) and `docker/.env.example` updated
- [ ] PR targets `dev`, summary covers the full branch diff (`git diff dev...HEAD`)

## Docs

`docs/` is the onboarding entry point — see
[README.md](./README.md) for the map. The source of truth for endpoint
shapes is the gateway code; if a doc disagrees with the code, the code
wins and the doc should be fixed in the same PR.
