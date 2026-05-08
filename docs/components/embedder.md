# embedder (Python sidecar)

**Path:** `services/embedder/`
**Type:** Python service, FastAPI + uvicorn
**Test count:** ~18

CLAP audio + text embeddings. Out-of-process from the gateway so the
Rust binary stays small and so PyTorch's model loading doesn't block
gateway startup.

The gateway calls this service over plain HTTP (it's expected to live
on `localhost`, or at least on the same trusted network as the
gateway).

## Endpoints

### `GET /healthz`

Returns 200 if the model is loaded, 503 otherwise.

```json
{
  "status": "ok",
  "model_loaded": true,
  "model_version": "clap-music_audioset_epoch_15_esc_90.14",
  "dim": 512
}
```

The Rust client treats both 200 and 503 as "embedder is reachable" —
it differentiates `ModelNotLoaded` separately so the gateway can log
"sidecar is up but warming" vs "sidecar is down."

### `POST /embed/audio`

Body: raw audio bytes, `Content-Type: application/octet-stream`.
Accepts whatever `librosa.load` accepts (FLAC, MP3, OGG, WAV).

Response:
```json
{
  "vector": [0.012, -0.084, ...],   // 512 floats
  "dim": 512,
  "model_version": "clap-music_..."
}
```

Vectors are L2-normalized so cosine similarity = dot product.

### `POST /embed/text`

Body: `{"text": "rainy sunday afternoon"}`.

Response: same shape as `/embed/audio`. The same model encodes both
modalities into the same space — that's CLAP's whole point.
Text-query stations (planned) will use this endpoint.

## Two backends

Selected via `EMBEDDER_BACKEND` environment variable.

### `stub` (default)

Deterministic hash-based vectors. No GPU, no PyTorch, no model
download. Used for:
- Local development without ML weight files.
- The test suite (so CI doesn't pull torch).

There is **no automatic fallback** from `clap` to `stub` — if you set
`EMBEDDER_BACKEND=clap` and the import fails or `CLAP_CHECKPOINT`
isn't set, the service refuses to start. Degraded mode lives on the
gateway side (it tolerates an unreachable embedder), not inside the
sidecar.

```python
seed = int.from_bytes(hashlib.sha256(audio_bytes).digest()[:8], "big")
rng = np.random.default_rng(seed)
v = rng.standard_normal(self.dim).astype(np.float32)
v /= np.linalg.norm(v)
```

Same input bytes → same vector. Different input bytes → independent
vectors. Useful property for tests that want to check
"deduplication" without a real model.

### `clap` (production)

Wraps [LAION CLAP](https://github.com/LAION-AI/CLAP). Loads a `.pt`
checkpoint from `CLAP_CHECKPOINT` at construction time. Device
selection is delegated to `laion_clap.CLAP_Module`, which picks up
`CUDA_VISIBLE_DEVICES` / `HIP_VISIBLE_DEVICES` from the environment.

For audio: `soundfile` + `librosa.resample` decode and resample to
48 kHz mono (CLAP's expected sample rate), then the audio encoder
produces a 512-dim embedding.

For text: the CLAP text encoder produces a 512-dim embedding in the
same space.

GPU is strongly recommended. Single-track inference on CPU is ~10 s;
on a recent AMD card, ~50 ms.

## Layout

```
services/embedder/
├── pyproject.toml
├── README.md
├── embedder/
│   ├── __init__.py
│   ├── app.py          # FastAPI app + route handlers
│   ├── protocol.py     # Pydantic request/response models
│   ├── stub.py         # StubEmbedder
│   └── clap_backend.py # ClapEmbedder (only imported when EMBEDDER_BACKEND=clap)
└── tests/
    └── test_endpoints.py
```

## Dependency injection for tests

Tests inject a `StubEmbedder` into the FastAPI app via
`app.dependency_overrides`:

```python
def make_test_app():
    app = create_app()
    app.dependency_overrides[get_embedder] = lambda: StubEmbedder(dim=512)
    return app
```

This lets tests run without `torch` installed:

```bash
uv sync --extra dev   # NOT --extra clap
uv run pytest
```

`pyproject.toml` declares `clap` as an optional extra so `pip install -e .[dev]`
doesn't pull torch.

## Run modes

```bash
# Stub (default), port 9000
uvicorn embedder.app:app --port 9000

# CLAP, port 9000 — laion_clap picks up the GPU automatically if
# torch was built with CUDA / ROCm support.
EMBEDDER_BACKEND=clap \
CLAP_CHECKPOINT=/path/to/music_audioset_epoch_15_esc_90.14.pt \
  uvicorn embedder.app:app --port 9000
```

The server is single-threaded by default. For concurrent requests,
add `--workers N` — but be aware that each worker loads the model
independently. At single-user scale, one worker is enough.

## Tests

`tests/test_endpoints.py` covers:
- `/healthz` shape (both `model_loaded: true` and `false` cases).
- `/embed/audio` happy path, returns 512-dim L2-normalized vector.
- `/embed/audio` with empty body → 400.
- `/embed/audio` with invalid audio → 400 (or 500, depending on
  whether librosa can be tricked into raising an unexpected
  exception).
- `/embed/text` happy path.
- `/embed/text` with empty text → 400.
- `/embed/text` with too-long text → 400 (cap at 4 KB).
- Model version is included in every response.
- Pydantic validation on the response (vector length matches `dim`).

All 18-ish tests run in milliseconds because the stub embedder has
no model load time.

## Known gaps

- **No batching.** Each `/embed/audio` is one request → one
  inference. CLAP supports batched inference; we'd see a 5-10× speedup
  on GPU if we batched. The Rust ingest worker is single-track today,
  so this isn't on the critical path yet.
- **No model warm-up endpoint.** The first `/embed/audio` after
  startup is slower because torch lazily compiles kernels. A
  `/warmup` endpoint could amortize this.
- **No metrics.** No `/metrics` Prometheus endpoint. Logs go to
  stdout in plain text.
- **No streaming inference.** Audio uploads are buffered fully into
  memory before inference. Probably fine — clips are 120s ≈ 5 MB —
  but could chunk for longer inputs.
