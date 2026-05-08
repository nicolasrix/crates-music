# music-embedder

CLAP audio + text embedding sidecar for the crates-music gateway.

The gateway calls this service via HTTP to embed tracks during ingest
(`POST /embed/audio`) and to handle text-query stations (`POST /embed/text`).
Inference runs out-of-process — Python with PyTorch + LAION CLAP — so
the Rust gateway stays lightweight and we use the canonical CLAP
implementation rather than a re-export to ONNX.

## Endpoints

| Method | Path | Body | Returns |
|---|---|---|---|
| GET | `/healthz` | — | `{status, model_loaded, model_version, dim}` (200 if loaded, 503 otherwise) |
| POST | `/embed/audio` | raw bytes (`application/octet-stream`) | `{vector: [f32; 512], dim: 512, model_version}` |
| POST | `/embed/text` | `{"text": "..."}` JSON | `{vector: [f32; 512], dim: 512, model_version}` |

Vectors are L2-normalized so cosine similarity = dot product.

## Backends

Selected via the `EMBEDDER_BACKEND` env var:

- `stub` (default) — deterministic hash-based vectors. No model load,
  no GPU, no PyTorch. Useful for the gateway's dev loop and for
  degraded mode if the real backend isn't ready.
- `clap` — production. Loads a LAION CLAP checkpoint via `laion_clap`.
  Requires the `clap` optional extra and `CLAP_CHECKPOINT` env var
  pointing at a `.pt` file.

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
