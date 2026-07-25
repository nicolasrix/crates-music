# Testing

The Rust workspace has ≈1000 tests across 8 crates (1001 passing at
last count). The web app has 185 Vitest tests across 14 files. The
Python embedder has 111 (plus 11 benchmark tests, deselected by
default).

This doc covers how the test suites are organised, what mocks/stubs
exist, and how to run a single test.

## Running tests

```bash
# Whole workspace
cargo test --workspace

# One crate
cargo test -p music-recommend
cargo test -p music-gateway

# One integration suite within a crate
cargo test -p music-gateway --test recommend
cargo test -p music-gateway --test events

# Filter by name (substring match across all tests in scope)
cargo test -p music-recommend events::          # unit tests in events module
cargo test recommend_next                       # all tests with "recommend_next" in name
```

Workspace tests typically take under ~10 seconds once compiled. The
slowest suites are the OAuth ones at 1–2 s each (Argon2 password
hashing is deliberately expensive).

## Test organisation

Each crate has two flavours:

**Unit tests** live inline with the code they exercise:

```rust
// crates/music-recommend/src/events.rs
#[cfg(test)]
mod tests {
    use super::*;
    // ...
}
```

These get an in-memory SQLite pool, exercise the public API, and run
in milliseconds.

**Integration tests** live in `crates/<crate>/tests/`. They drive the
crate from outside its `pub` surface, so they only see what's
actually exported. These are where the big-picture flows live —
"submit an OAuth code, get a token back."

For the gateway, integration tests use
`tower::ServiceExt::oneshot` to drive the router directly without
binding a TCP port:

```rust
let state = build_state(test_config()).await;
let app = build_router(state.clone());
let resp = app.oneshot(request).await.unwrap();
```

This is the pattern across all 32 of the gateway's integration test
files. It's why TLS termination lives in `main.rs` and not in
`build_router` — keeping it out of the router lets tests skip TLS.

## Per-crate breakdown

| Crate | Tests | What's notable |
|---|---|---|
| music-core | 24 | Pure data; no fixtures needed. |
| music-subsonic | 35 | Uses `wiremock` to stub Navidrome HTTP responses. Covers the browse-parity endpoints (`getArtists`, `getArtist`, `search3`, `getSong`) and the bearer/TLS builder composition. |
| music-cache | 37 | Each test gets a `tempfile::tempdir()` for isolation. Includes the `clear_browse` / cover-art-prefix invalidation paths. |
| music-player | 8 | Most tests skip actual playback (no audio device in CI); they exercise the resolution / cache-read paths. |
| music-sync | 50 | Pure state machine; no fixtures. Property-style tests for op application, including the `StartSession`/`StopSession` lifetime and legacy-payload deserialization. |
| music-recommend | 337 | `wiremock` for the embedder client; in-memory SQLite for the store; in-memory `usearch` index for the ANN. Heavy unit coverage on the post-retrieval modules (queue_filter, mmr, aggregate, feedback, projection, sessions) plus whitening and the anchor leash. |
| music-gateway | 475 | Largest suite. Each integration test gets its own `AppState` via `common::build_state`. 32 integration files. |
| music-cli | 35 | Mostly config + format unit tests; CLI dispatch goes through the `app::run` library entrypoint so end-to-end behaviour can be asserted without spawning a subprocess. |
| **web** (Vitest) | 185 | 14 files — API clients (library, recommend, events, diagnostics), player logic (scrobble, skip, auto-skip, autoplay seeds + settings), sync reducer + playback helpers, latent-space binning, the IndexedDB audio cache (via `fake-indexeddb`), and format utils. Run with `cd apps/web && npm run test`. |

## Shared test fixtures

Crates with multiple integration test files share fixtures via a
`tests/common/mod.rs`:

```rust
// crates/music-gateway/tests/common/mod.rs
pub fn test_config() -> Config { ... }
pub async fn build_state(config: Config) -> AppState { ... }
pub const TEST_BEARER: &str = "test-bearer-token";
```

`build_state` constructs an in-memory cache, in-memory OAuth store,
in-memory embedding store, and in-memory ANN index. No filesystem
state escapes the test.

## Mock patterns

### HTTP — `wiremock`

Used for the Subsonic client and the embedder client. Spins up a
local HTTP server, lets you assert on requests and configure
responses.

```rust
let server = MockServer::start().await;
Mock::given(method("GET"))
    .and(path("/healthz"))
    .respond_with(ResponseTemplate::new(200).set_body_json(json!({...})))
    .mount(&server)
    .await;
let client = EmbedderClient::new(EmbedderConfig {
    url: server.uri(),
    timeout: Duration::from_secs(5),
});
```

### SQLite — in-memory

`EmbeddingStore::open_in_memory()`, `OauthStore::open_in_memory()`,
`Cache::open_in_memory()` all spin up a one-connection in-memory
SQLite pool and run their migrations. Single-connection because
in-memory SQLite isn't shared across connections.

### ANN — in-memory

`AnnIndex::open_in_memory(dim, connectivity)` constructs a
non-persistent index. Used in every gateway recommend test.

### Trait stubs — `async-trait`

The ingest worker takes an `AudioFetcher` trait. Tests provide a
stub that returns canned bytes:

```rust
struct FakeFetcher(Bytes);
#[async_trait]
impl AudioFetcher for FakeFetcher {
    async fn fetch_clip(&self, _: &TrackId) -> Result<Bytes, FetchError> {
        Ok(self.0.clone())
    }
}
```

This is the easiest pattern to extend when adding a new "depends on
external thing" component.

## Python embedder tests

```bash
cd services/embedder
uv run --extra dev pytest
# or with verbose output:
uv run --extra dev pytest -v
```

111 tests covering the API endpoints, audio IO, checkpoint integrity
guards, backend selection, the `/reduce` UMAP path, and the
Server-Timing helpers. The 11 benchmark tests are deselected by
default (see [Benchmarks](#benchmarks)); the real-CLAP backend test
skips unless a checkpoint is on disk.

Tests use FastAPI's `dependency_overrides` to inject a stub embedder
in place of whatever `EMBEDDER_BACKEND` would otherwise pick. This
lets the test suite avoid PyTorch entirely — the `dev` extra
doesn't pull `torch`, only `pytest`, `pytest-benchmark`, and `httpx`.

The stub backend (which the tests inject) generates deterministic
hash-based vectors:

```python
seed = int.from_bytes(hashlib.sha256(audio_bytes).digest()[:8], "big")
rng = np.random.default_rng(seed)
v = rng.standard_normal(self.dim).astype(np.float32)
v /= np.linalg.norm(v)  # L2 normalize
```

So given the same input bytes, you get the same vector. This makes
deduplication tests possible without needing a real model.

## Benchmarks

Benchmarks are *regression detectors*, not load tests — "did this PR
make X slower?", not "what's our peak QPS?". They don't run as part
of `cargo test` / `pytest`.

**Rust — Criterion.** Four suites covering the hot paths surfaced by
the diagnostics work:

| Bench | Crate | Measures |
|---|---|---|
| `server_timing` | `music-recommend` | `parse_server_timing` per-call cost |
| `ann` | `music-recommend` | `AnnIndex::upsert` / `query` at N=100/1000/5000 |
| `embedder_client` | `music-recommend` | HTTP roundtrip + JSON parse against a wiremock fake (not inference cost) |
| `trace_store` | `music-gateway` | `insert_batch` + `trim_to_capacity` |

```bash
cargo bench --workspace                          # everything
cargo bench --bench ann -p music-recommend       # one suite
cargo bench --bench ann -p music-recommend -- --quick  # ≈10× faster, iterating only
```

Criterion remembers the previous run and prints a
`change: [+X% -Y%] (p = …)` line — that's the regression signal. HTML
reports land at `target/criterion/<group>/report/index.html`.

**Python — pytest-benchmark.** Two suites in
`services/embedder/tests/`: `test_benchmarks.py` (stub backend, always
runs) and `test_benchmarks_clap.py` (real CLAP inference, skipped
unless the `clap` extra is installed and `CLAP_CHECKPOINT` is set).

```bash
cd services/embedder
uv run --extra dev pytest -m benchmark
uv run --extra dev pytest -m benchmark --benchmark-autosave   # save baseline
uv run --extra dev pytest -m benchmark --benchmark-compare    # diff against it
```

Approximate baselines for sanity-checking live in the
[CLAUDE.md](../CLAUDE.md) microbenchmark section; don't commit
hardware-specific numbers as gates.

## Other suites

| Suite | Run with | Covers |
|---|---|---|
| Docker config templating | `cd docker/gateway && pytest` | `gen_config.py` (env → `gateway.toml` rendering, every `RECOMMEND_*` branch) and the container entrypoint. Pure-Python, no Docker needed. |
| Backup roundtrip | `./scripts/tests/backup-roundtrip.sh` | `backup.sh` → `restore.sh` → sha256-manifest verification, end to end on temp dirs. Run it after touching anything in the backup path. |

## What's not tested

- **Actual audio playback** — the test suite doesn't open audio
  devices. CI doesn't have one, and even on a dev machine it's
  flaky. The decoder + cache-read path is exercised; rodio's sink is
  not.
- **Real CLAP / CLaMP 3 inference** — too heavy for CI. The Rust
  embedder client is tested against `wiremock`; the Python backends
  get guard/unit tests without torch, and real inference is exercised
  manually by running the sidecar (`EMBEDDER_BACKEND=clap|clamp3`) or
  via `pytest -m benchmark` with a checkpoint on disk.
- **TLS** — gateway integration tests skip TLS by going through
  `oneshot`. End-to-end TLS works in production; nothing exercises
  it in CI.
- **Web UI components** — Vitest covers pure-logic helpers; React
  component rendering and Playwright end-to-end flows are not yet in.
  The build type-checks via `tsc -b`, which catches a lot, but
  doesn't catch runtime bugs.
- **Cross-device sync** — the sync state machine has unit tests
  exercising every op. The end-to-end "two clients, both connected
  to the gateway, observe consistent state" is not yet a test.

## CI

There is no CI config in the repo yet (no `.github/workflows/`).
Locally before pushing, the convention is:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

This catches almost everything that would fail in a future CI setup.
The full pre-push checklist (including web and embedder checks) lives
in [CONTRIBUTING.md](./CONTRIBUTING.md).

## Adding a new test

1. **Pick the right level.** A new function in `music-cache`? Unit
   test in the same file. A new endpoint on the gateway? New
   integration test in `crates/music-gateway/tests/`.
2. **Use existing fixtures.** `common::build_state` in the gateway,
   in-memory constructors in the recommend crates.
3. **Run the single test before committing.**
   `cargo test -p <crate> --test <file> <test_name>`.
4. **Don't introduce flakiness.** No sleeps, no real network, no
   real disk. If your test needs time to pass, mock the clock; if it
   needs network, use `wiremock`.
