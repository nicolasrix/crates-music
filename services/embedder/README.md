# music-embedder

Audio + text embedding sidecar for the crates-music gateway.

The gateway calls this service via HTTP to embed tracks during ingest
(`POST /embed/audio`) and to handle text-query stations (`POST /embed/text`).
Inference runs out-of-process — Python with PyTorch — so the Rust
gateway stays lightweight and we use the canonical model implementations
rather than a re-export to ONNX.

## Endpoints

| Method | Path | Body | Returns |
|---|---|---|---|
| GET | `/healthz` | — | `{status, model_loaded[, model_version, dim, device]}` (200 if loaded, 503 otherwise) |
| POST | `/embed/audio` | raw bytes (`application/octet-stream`) | `{vector: [f32; dim], dim, model_version}` |
| POST | `/embed/text` | `{"text": "..."}` JSON | `{vector: [f32; dim], dim, model_version}` |

Vectors are L2-normalized so cosine similarity = dot product. The
vector dimension is reported per-backend on `/healthz` rather than
fixed by the service; the gateway pins it via the `[recommend]
embedding_dim` config field at boot time.

When `EMBEDDER_BEARER_TOKEN` is set (split-host deployments), the
descriptive `/healthz` fields (`model_version`, `dim`, `device`) are
returned only to callers that present the bearer — an unauthenticated
LAN peer gets just `{status, model_loaded}`, enough for a liveness
probe but not enough to fingerprint the model or hardware. The gateway
boot probe carries the bearer by default, so it still reads `dim` at
startup.

## Backends

Selected via the `EMBEDDER_BACKEND` env var:

- `stub` (default) — deterministic hash-based vectors. No model load,
  no GPU, no PyTorch. Useful for the gateway's dev loop and for
  degraded mode if the real backend isn't ready. Default dim=512;
  the constructor accepts a `dim` so the stub can stand in for either
  backend's wire shape during tests.
- `clap` — LAION CLAP via `laion_clap`. 512-dim. Requires the `clap`
  optional extra and `CLAP_CHECKPOINT` env var pointing at a `.pt` file.
- `clamp3` — CLaMP 3 via the vendored `embedder._clamp3` package.
  768-dim. Requires the `clamp3` optional extra plus `CLAMP3_CHECKPOINT`
  (path to the CLaMP 3 unified `.pth`) and `MERT_FOLDER` (path to a
  local copy of `m-a-p/MERT-v1-95M`).

## Running

Dev (stub backend, no extras):

```sh
uv venv
uv pip install -e '.[dev]'
uv run uvicorn embedder.app:app --host 127.0.0.1 --port 8001
```

Production (CLAP backend):

```sh
uv pip install -e '.[clap]'
EMBEDDER_BACKEND=clap CLAP_CHECKPOINT=/srv/models/music_audioset_epoch_15_esc_90.14.pt \
    uv run uvicorn embedder.app:app --host 127.0.0.1 --port 8001
```

## Tests

```sh
uv run pytest
```

Tests inject a `StubEmbedder` via FastAPI's `dependency_overrides`,
so the suite runs without PyTorch / CLAP weights.
