# Getting started

This walks through bringing up the full local dev stack: gateway,
web app, and (optionally) the embedder sidecar. Allow ~30 minutes
end-to-end on the first run, mostly cert generation and the initial
`cargo build`.

## Prerequisites

| Tool | Version | What for |
|---|---|---|
| Rust | 1.95+ (stable) | All Rust crates. Pinned in `rust-toolchain.toml`. |
| Node.js | 20+ | Web app (Vite + React). |
| Python | 3.11+ | Embedder sidecar (optional unless you want recommendations). |
| `mkcert` | any | Local TLS cert. The gateway refuses to start without one. |
| Navidrome | any recent | The music catalog. Run separately; the gateway proxies to it. |

Optional but recommended:
- `uv` — Python dependency manager for the embedder. Faster than pip.
- `cargo-watch` — auto-rebuild on save (`cargo watch -x 'run -p music-gateway -- --config gateway.toml'`).

## 1. Clone and build

```bash
git clone <repo-url>
cd crates-music
cargo build --workspace
```

The first build pulls a lot — `usearch` ships C++ that `cc-rs` has to
compile, `axum-server` pulls rustls, etc. Plan on 5-10 minutes
depending on your machine. Subsequent builds are seconds.

## 2. Generate TLS certs

```bash
./scripts/dev-certs.sh
```

This:
1. Runs `mkcert -install` (one-time, installs the local CA into your
   system trust store; you may be prompted for sudo).
2. Generates `certs/gateway.local.pem` and `certs/gateway.local-key.pem`.

Add `gateway.local` to your `/etc/hosts` (or wherever your OS keeps
hosts):

```
127.0.0.1   gateway.local
```

If you want the gateway reachable from your phone or other LAN
devices, point `gateway.local` at the gateway machine's LAN IP on
each device, and run `mkcert -install` on each device too. The CA is
per-device.

## 3. Configure the gateway

```bash
cp gateway.example.toml gateway.toml
$EDITOR gateway.toml
```

Edit at minimum:

- `[server].bearer_token` — replace with `openssl rand -hex 32`
- `[upstream].navidrome_url` — your Navidrome URL
- `[upstream].username` / `password` — your Navidrome creds

Full reference: [CONFIGURATION.md](./CONFIGURATION.md).

## 4. Start the gateway

```bash
cargo run -p music-gateway -- --config gateway.toml
```

On first run you'll see a warning like:

```
WARN  gateway is unconfigured — visit https://gateway.local:8443/oauth/setup with token: <hex>
```

Open that URL in your browser. Set a master password. The setup token
is single-use; after this run, the warning won't appear again.

The gateway also creates four SQLite files next to the config:
- `gateway-state.sqlite` — OAuth state (irreplaceable: holds your
  master password hash + per-device refresh tokens; back this up).
- `gateway-cache.sqlite` — L2 metadata + cover-art cache
  (throwaway: re-derived from upstream on demand).
- `gateway-state.recommend.sqlite` — embeddings, ingest queue, event
  log, track metadata, play history, feedback, 2D projections.
  Embeddings are reproducible (re-ingest a track to regenerate); the
  event log + feedback are append-only signal worth backing up.
- `gateway-state.traces.sqlite` — diagnostics ring buffer (closed
  `tracing` spans + browser RUM events). Throwaway.

Plus one file outside SQLite:
- `gateway-state.ann` (+ `gateway-state.ann.keys` sidecar) — the
  HNSW index. Derived cache; rebuildable from the embedding store at
  boot, so safe to delete.

## 5. Smoke-test with the CLI

```bash
mkdir -p ~/.config/crates-music
cat > ~/.config/crates-music/config.toml <<EOF
[server]
url = "http://nav.lan:4533"
username = "alice"
password = "wonderland"

[gateway]
url = "https://gateway.local:8443"
bearer_token = "<the same token you put in gateway.toml>"
EOF

cargo run -p music-cli -- albums list
```

You should see your Navidrome's albums. If you get a TLS error,
either `mkcert -install` didn't take effect, or `gateway.local` isn't
in your hosts file.

## 6. Start the web app

```bash
cd apps/web
npm install
npm run dev
```

Open http://localhost:5173. Click "Sign in." You'll be redirected to
the gateway, prompted for your master password, then bounced back
with an OAuth code.

The Vite dev server proxies `/oauth`, `/rest`, and `/v1` to the
gateway over HTTPS. It skips cert verification on this proxy because
the local mkcert CA isn't in Node's trust store by default — see
`apps/web/vite.config.ts`.

## 7. (Optional) Start the embedder

The recommender requires the Python sidecar. For most onboarding work
you don't need it — the gateway runs in degraded mode without it:
track-seeded recommend endpoints (`/v1/recommend/next` etc.) return
404 for every seed, and the text-query station endpoint
(`/v1/recommend/station`) returns 503. Everything else still works.

If you want it:

```bash
cd services/embedder

# Stub backend: deterministic hash-based vectors. No GPU. Fast to set up.
uv sync                                    # or: pip install -e .[dev]
uv run uvicorn embedder.app:app --port 9000

# Or the real CLaMP 3 backend (production) — 768-dim, music-specific.
# Requires PyTorch + ROCm/CUDA, a CLaMP 3 checkpoint, and MERT-v1-95M:
uv sync --extra clamp3
EMBEDDER_BACKEND=clamp3 \
  CLAMP3_CHECKPOINT=/path/to/weights_clamp3_saas_*.pth \
  MERT_FOLDER=/path/to/MERT-v1-95M \
  uv run uvicorn embedder.app:app --port 9000
# MERT_FOLDER can be a local copy or the hub id `m-a-p/MERT-v1-95M`.
# The clamp3 extra includes `sentencepiece`, required by the
# xlm-roberta-base text tokenizer that powers text-query stations.

# Or the legacy CLAP backend — 512-dim, requires PyTorch + ROCm/CUDA + a checkpoint:
uv sync --extra clap
CLAP_CHECKPOINT=/path/to/clap.pt EMBEDDER_BACKEND=clap \
  uv run uvicorn embedder.app:app --port 9000
```

Then add to `gateway.toml`:

```toml
[embedder]
url = "http://localhost:9000"
timeout_seconds = 30
```

Restart the gateway. You'll see (CLaMP 3, the production backend):

```
INFO  embedder: probe ok model=clamp3-saas dim=768 device=cuda
```

For the legacy CLAP backend the probe reports `dim=512` instead; the
stub backend reports `model=stub-v1` (and `dim=512` by default,
overridable via `EMBEDDER_STUB_DIM`). The gateway's
`[recommend].embedding_dim` must match the probed dim — see
[CONFIGURATION.md](./CONFIGURATION.md).

## Verifying everything works

| Check | How |
|---|---|
| Gateway up | `curl -k https://gateway.local:8443/healthz` → 200 with `{"status": "ok", ...}` |
| Auth working | `curl -k -H "Authorization: Bearer $TOKEN" https://gateway.local:8443/v1/whoami` → 200 |
| Subsonic proxy | `curl -k -H "Authorization: Bearer $TOKEN" 'https://gateway.local:8443/rest/ping?v=1.16.1&c=test&f=json'` → Subsonic ping OK |
| Embedder reachable | `curl http://localhost:9000/healthz` → 200 with `model_loaded: true` |
| Web app | http://localhost:5173 → albums list loads |
| Diagnostics | http://localhost:5173/diagnostics after sign-in → live recommend / RUM / trace dashboards |

## Common first-run snags

**"loading TLS cert + key"** — gateway can't find `certs/gateway.local.pem`.
Run `./scripts/dev-certs.sh` from the repo root (paths in the example
config are relative).

**Browser says "your connection is not private"** — `mkcert -install`
hasn't run yet, or the browser caches certs. Restart the browser.
Firefox uses its own trust store; run `mkcert -install` while Firefox
is closed.

**Gateway crashes with `bind: Address already in use`** — something
is on `8443`. Either change `listen` in `gateway.toml` or kill the
other process.

**Web app shows blank page** — check the browser console. Most
likely the OAuth redirect didn't match `[[oauth.clients]].redirect_uris`
in `gateway.toml`. The example config has both `localhost:5173` and
`gateway.local:8443` redirects pre-configured.

**`cargo build` fails on `usearch`** — needs a working C++ compiler.
On Linux: `apt install build-essential` or `pacman -S base-devel`.
On macOS: `xcode-select --install`.

## What's next

- [components/music-gateway.md](./components/music-gateway.md) — where most code changes land.
- [components/music-recommend.md](./components/music-recommend.md) — the recommender stack (embeddings, ANN, MMR, feedback, projection).
- [API.md](./API.md) — endpoint reference for client work.
- [TESTING.md](./TESTING.md) — running the test suite.
- [CONFIGURATION.md](./CONFIGURATION.md) — the rest of the gateway config knobs.
- The `/diagnostics` page in the web app is the fastest way to see
  what the gateway is actually doing in real time.
