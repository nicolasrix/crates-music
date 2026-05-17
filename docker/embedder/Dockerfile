# syntax=docker/dockerfile:1.7
#
# Embedder sidecar (Python FastAPI).
#
# Phase A: stub backend only — deterministic hash vectors, no GPU,
# no torch. The gateway uses this for local dev and the CLI happy
# path; real CLAP inference lives in a follow-up `Dockerfile.clap`
# variant once GPU passthrough is documented.
#
# Build from the repo root:
#
#   docker build -f docker/embedder/Dockerfile -t crates-music/embedder .

ARG PYTHON_VERSION=3.12

# ─── Stage 1: resolve & install deps into a venv ──────────────────────
FROM python:${PYTHON_VERSION}-slim AS deps

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Dependency manifest first — editing embedder/app.py shouldn't bust
# the pip-install layer.
COPY services/embedder/pyproject.toml services/embedder/README.md ./

# Install just the stub-backend deps. The optional `[clap]` extra is
# the heavy one (torch, librosa, laion-clap) and we deliberately skip
# it in Phase A.
RUN pip install --no-cache-dir \
        'fastapi>=0.115' \
        'uvicorn[standard]>=0.32' \
        'pydantic>=2.9' \
        'numpy>=1.26' \
        'python-multipart>=0.0.20'

COPY services/embedder/embedder ./embedder
RUN pip install --no-cache-dir --no-deps .

# ─── Stage 2: slim runtime ────────────────────────────────────────────
FROM python:${PYTHON_VERSION}-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*

# uid/gid pinned to 10001 — the embedder shares the gateway's
# `gw-data` volume so it can write UMAP projections into
# `gateway-state.recommend.sqlite`. Both containers must agree on the
# numeric uid or the kernel rejects the write. Keep this aligned with
# `docker/gateway/Dockerfile`.
RUN groupadd --system --gid 10001 embedder \
 && useradd --system --uid 10001 --gid embedder \
        --home-dir /var/empty --shell /usr/sbin/nologin embedder

# Lift site-packages and the uvicorn launcher from the deps stage.
COPY --from=deps /usr/local/lib/python3.12/site-packages /usr/local/lib/python3.12/site-packages
COPY --from=deps /usr/local/bin/uvicorn /usr/local/bin/uvicorn

USER embedder

ENV EMBEDDER_BACKEND=stub \
    PYTHONUNBUFFERED=1

EXPOSE 9000

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --silent --fail http://localhost:9000/healthz || exit 1

CMD ["uvicorn", "embedder.app:app", "--host", "0.0.0.0", "--port", "9000"]
