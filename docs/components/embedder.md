# embedder (Python sidecar)

**Path:** `services/embedder/`
**Type:** Python service, FastAPI + uvicorn

Audio + text embeddings for the recommender. Out-of-process from the
gateway so the Rust binary stays small and so PyTorch's model loading
doesn't block gateway startup.

The gateway calls this service over HTTP. It's expected to live on
`localhost` or on the same trusted LAN as the gateway — and in the live
deployment it runs on a **separate GPU host** (the GPU host with the AMD
RDNA4 GPU) while the gateway runs CPU-only on the NAS, reaching the
sidecar over the LAN. For that split-host shape an optional bearer token
guards the compute endpoints (see [Authentication](#authentication)).

The **content embedder is pluggable** (see [Backends](#backends)). The
production backend is **CLaMP 3** (768-dim, music-specific); **CLAP**
(512-dim) is the prior backend and still supported; **stub** is the
dev/test default. The vector dimension is read from the active backend
(`emb.dim`), never hardcoded — the gateway's `[recommend].embedding_dim`
must match it.

## Endpoints

### `GET /healthz`

Returns 200 if the model is loaded, 503 otherwise. Not behind the bearer
gate — boot probes shouldn't need the secret, and 200/503 leaks nothing.

```json
{
  "status": "ok",
  "model_loaded": true,
  "model_version": "weights_clamp3_saas_h_size_768_t_model_FacebookAI_xlm-roberta-base_...",
  "dim": 768,
  "device": "cuda"
}
```

`dim` is backend-dependent (768 for CLaMP 3, 512 for CLAP). `model_version`
is the checkpoint identity and likewise varies by backend (a CLAP
deployment reports e.g. `clap-music_audioset_epoch_15_esc_90.14`).

The `device` field reports the compute device. On ROCm-built PyTorch, HIP
devices identify as `"cuda"` — so `"cuda"` with an AMD card means GPU
acceleration is engaged. Older sidecars that pre-date this field still
parse on the gateway side (surfaced as `device=unknown` in the boot log).

The Rust client treats both 200 and 503 as "embedder is reachable" — it
differentiates `ModelNotLoaded` separately so the gateway can log "sidecar
is up but warming" vs "sidecar is down."

### `POST /embed/audio`

Body: raw audio bytes, `Content-Type: application/octet-stream`. Inference
runs in a threadpool (`asyncio.to_thread`) so concurrent ingest workers
overlap their CPU-side decode/resample even though the GPU forward pass
serializes on the device.

Response:
```json
{
  "vector": [0.012, -0.084, ...],   // `dim` floats (768 for CLaMP 3)
  "dim": 768,
  "model_version": "weights_clamp3_saas_..."
}
```

Vectors are L2-normalized so cosine similarity = dot product. A
`Server-Timing` header carries per-stage durations (decode, forward, …)
that the gateway parses into its trace store.

### `POST /embed/text`

Body: `{"text": "rainy sunday afternoon"}`.

Response: same shape as `/embed/audio`. The model encodes both modalities
into the **same joint space** — that's the point. The gateway's
`GET /v1/recommend/station?text=…` endpoint calls this to turn a
natural-language prompt into a query vector for the content ANN.

### `POST /reduce`

Dimensionality reduction (UMAP + PCA) for the latent-space visualisation,
**vectors-over-the-wire** so it works regardless of host. The embedder
does *not* open the gateway's SQLite (that assumed a shared volume, which
breaks on a separate GPU host). Instead the gateway reads its own
embedding rows, packs the `(N, dim)` matrix as base64 row-major
little-endian f32 — the exact byte layout of the stored `vector` blobs —
and ships it here; the embedder returns coordinates and the gateway
persists the projection rows itself.

Request:
```json
{
  "track_ids": ["t1", "t2", ...],
  "dim": 768,
  "vectors_b64": "…base64 of N*dim LE f32, row-major…",
  "n_components": 2,           // 2 or 3
  "n_neighbors": 15, "min_dist": 0.1, "random_state": 42, "metric": "cosine"
}
```

Response: `{"points": [{"track_id", "x", "y", "z?", "pc1?".."pc4?"}, …]}`.

Errors: `400` if `vectors_b64` doesn't decode to exactly `N*dim*4` bytes,
or on bad knobs (`n_components ∉ {2,3}`, unknown `metric`, `n_neighbors ≥
N`); `503` if the optional `reduce` extra (umap-learn + sklearn) isn't
installed. Pure compute — no model load, no `emb.loaded` guard.

## Authentication

`EMBEDDER_BEARER_TOKEN` (optional) guards the compute endpoints for
split-host deployments. When set, `/embed/audio`, `/embed/text`, and
`/reduce` require `Authorization: Bearer <token>` (constant-time compare,
401 otherwise); `/healthz` is exempt. Empty string is treated as unset, so
an accidentally-empty env var doesn't silently accept everyone with
`Bearer `. The same token is configured on the gateway's `EMBEDDER_URL`
client.

## Backends

Selected via the `EMBEDDER_BACKEND` environment variable
(`stub` | `clap` | `clamp3`). Imports are lazy — `stub` never pulls torch.
There is **no automatic fallback** between backends: if a real backend's
import fails or its required env vars are unset, the service refuses to
start. Degraded mode lives on the gateway side (it tolerates an
unreachable embedder), not inside the sidecar.

### `stub` (default)

Deterministic hash-based vectors. No GPU, no PyTorch, no model download.
Used for local dev without ML weights and for the test suite (so CI
doesn't pull torch).

```python
seed = int.from_bytes(hashlib.sha256(audio_bytes).digest()[:8], "big")
rng = np.random.default_rng(seed)
v = rng.standard_normal(self.dim).astype(np.float32)
v /= np.linalg.norm(v)
```

Same input bytes → same vector; different bytes → independent vectors.
Defaults to 512-dim; `EMBEDDER_STUB_DIM` overrides it (set `768` so the
stub mimics CLaMP 3's wire shape during dev).

### `clamp3` (production)

[CLaMP 3](https://github.com/sanderwood/clamp3) — a music-specific joint
audio/text encoder producing **768-dim** vectors. Requires the `clamp3`
extra, `CLAMP3_CHECKPOINT` (path to the unified saas `.pth`), and
`MERT_FOLDER` (path to a local `m-a-p/MERT-v1-95M` copy, or the HF hub id —
discouraged in prod since the container's unprivileged user has no
writable HF cache). Upstream inference code is vendored under
`embedder/_clamp3/` (slim `model.py` / `audio_io.py` / `feature_extractor.py`
/ MusicHuBERT, pinned at upstream `9016d2b`, with `LICENSES/` + `VENDORED.md`).

- **Audio:** MERT-v1-95M frontend (24 kHz mono, sliding 5-sec windows) →
  mean over 13 hidden layers → BOS/EOS markers → CLaMP 3 audio encoder →
  L2-norm → 768-dim.
- **Text:** xlm-roberta-base tokenize → 128-token-windowed CLaMP 3 text
  encoder → token-count-weighted mean → L2-norm, into the same 768-dim
  joint space as audio.

> **Tokenizer guard.** xlm-roberta-base is a SentencePiece model. Without
> the `sentencepiece` package (or its baked vocab) `transformers` silently
> loads a degenerate 5-token vocab where every word maps to `<unk>` — so
> all text embeddings collapse and stations return identical results.
> `Clamp3Embedder.__init__` fails loud on this: it raises if
> `vocab_size < 1000` or if `"death metal"` and `"smooth jazz"` tokenize
> to identical ids. `sentencepiece>=0.2` is a hard `clamp3` dependency.

### `clap`

Wraps [LAION CLAP](https://github.com/LAION-AI/CLAP), **512-dim**. Loads a
`.pt` checkpoint from `CLAP_CHECKPOINT`. The music checkpoints (e.g.
`music_audioset_epoch_15_esc_90.14.pt`) require `amodel="HTSAT-base"` —
hardcoded in `clap_backend.py` because the dimensions don't match
otherwise. `soundfile` + `librosa.resample` decode to 48 kHz mono; the
text encoder produces a same-space 512-dim embedding.

## GPU acceleration (AMD ROCm)

The default PyPI torch wheel is CUDA-only. To use an AMD card, install the
system ROCm SDK and route torch through PyTorch's ROCm wheel index.
Verified working on the RDNA4 (gfx1201) with ROCm 7.2.2 + torch
2.9.x+rocm6.4.

**1. Install ROCm userspace** (Arch / CachyOS):

```bash
sudo pacman -S rocm-hip-sdk    # or rocm-hip-runtime + rocm-hip-libraries
/opt/rocm/bin/rocminfo | grep -E "gfx|Marketing"
```

The card should be listed natively (e.g. `gfx1201`). If it's only listed
under a generic name, set `HSA_OVERRIDE_GFX_VERSION=11.0.0` to fall back to
RDNA3 emulation — works on RDNA4 cards before native support lands.

**2. Pin ROCm torch wheels.** `services/embedder/pyproject.toml` routes
`torch`, `torchaudio`, `torchvision`, and `pytorch-triton-rocm` through
`https://download.pytorch.org/whl/rocm6.4` on Linux (the marker keeps
non-Linux installs on default PyPI). This applies to both real backends:

```bash
cd services/embedder && uv sync --extra clamp3   # or --extra clap
```

This swaps in the `+rocm6.4` torch and removes the ~6 GB of unused
`nvidia-*` libraries the default wheels ship with.

**3. Verify.** Start the embedder and check `/healthz`:

```bash
EMBEDDER_BACKEND=clamp3 \
  CLAMP3_CHECKPOINT=/path/to/weights_clamp3_saas_*.pth \
  MERT_FOLDER=/path/to/MERT-v1-95M \
  HIP_VISIBLE_DEVICES=0 \
  uv run uvicorn embedder.app:app --port 9000
curl -s localhost:9000/healthz | jq '{dim, device}'   # → {"dim":768,"device":"cuda"}
```

The gateway boot log echoes the same fields — look for
`embedder: ready ... dim=768 device="cuda"` — so silent CPU fallback
(e.g. after a torch upgrade clobbers the source override) is visible
without re-benchmarking.

## Layout

```
services/embedder/
├── pyproject.toml          # extras: clap, clamp3, dev, reduce
├── README.md
├── embedder/
│   ├── app.py              # FastAPI app + route handlers + Server-Timing
│   ├── protocol.py         # Embedder protocol + EmbedResult
│   ├── reduce.py           # UMAP/PCA reduction (project_matrix)
│   ├── stub.py             # StubEmbedder
│   ├── clap_backend.py     # ClapEmbedder      (imported only for EMBEDDER_BACKEND=clap)
│   ├── clamp3_backend.py   # Clamp3Embedder    (imported only for EMBEDDER_BACKEND=clamp3)
│   └── _clamp3/            # vendored upstream CLaMP 3 inference code (pinned 9016d2b)
└── tests/
    ├── test_endpoints.py        test_default_embedder.py  test_reduce.py
    ├── test_clamp3_backend.py   test_clap_backend.py      test_server_timing.py
    └── test_benchmarks.py       test_benchmarks_clap.py
```

## Dependency injection for tests

Tests inject a `StubEmbedder` into the FastAPI app via
`app.dependency_overrides`, so they run without `torch` installed:

```python
def make_test_app():
    app = build_app()
    app.dependency_overrides[get_embedder] = lambda: StubEmbedder(dim=768)
    return app
```

```bash
uv sync --extra dev   # NOT --extra clap / --extra clamp3
uv run pytest
```

`pyproject.toml` declares `clap` and `clamp3` as optional extras so
`pip install -e .[dev]` doesn't pull torch.

## Run modes

```bash
# Stub (default), port 9000 — EMBEDDER_STUB_DIM=768 to mimic CLaMP 3's shape
uvicorn embedder.app:app --port 9000

# CLaMP 3 (production)
EMBEDDER_BACKEND=clamp3 \
CLAMP3_CHECKPOINT=/path/to/weights_clamp3_saas_*.pth \
MERT_FOLDER=/path/to/MERT-v1-95M \
  uvicorn embedder.app:app --port 9000

# CLAP (legacy)
EMBEDDER_BACKEND=clap \
CLAP_CHECKPOINT=/path/to/music_audioset_epoch_15_esc_90.14.pt \
  uvicorn embedder.app:app --port 9000
```

Single-threaded by default. `--workers N` enables concurrent requests but
each worker loads the model independently; at single-user scale one worker
is enough.

## Tests

`tests/` covers, via the stub backend (millisecond runtime, no model load):

- `/healthz` shape (`model_loaded` true/false).
- `/embed/audio` happy path → L2-normalized vector of the backend's `dim`;
  empty body → 400; invalid audio → 400/500.
- `/embed/text` happy path; empty text handling.
- `/reduce` byte-length validation, 2D/3D output, bad-knob → 400.
- Model version present in every response; Pydantic validates
  `len(vector) == dim`.
- `test_clamp3_backend.py` / `test_clap_backend.py` exercise the real
  backends (skipped unless the extra + checkpoints are present), including
  the degenerate-tokenizer guard.
- `test_server_timing.py` covers the `Server-Timing` formatter.

Benchmarks (`pytest -m benchmark`) live in `test_benchmarks.py` (stub) and
`test_benchmarks_clap.py` (real CLAP) — see the project README for the
benchmark workflow.

## Known gaps

- **No batching.** Each `/embed/audio` is one request → one inference.
  Both CLAP and CLaMP 3 support batched inference; the Rust ingest worker
  is single-track today, so this isn't on the critical path yet.
- **No model warm-up endpoint.** The first forward pass after startup is
  slow (kernels compile lazily — on ROCm, MIOpen JIT can take several
  seconds; cached thereafter). A `/warmup` endpoint could amortize it.
- **No metrics.** No `/metrics` Prometheus endpoint; per-stage timings ride
  the `Server-Timing` header into the gateway's trace store instead.
- **No streaming inference.** Audio uploads buffer fully into memory before
  inference. Fine for ~120 s clips (a few MB); could chunk for longer.
