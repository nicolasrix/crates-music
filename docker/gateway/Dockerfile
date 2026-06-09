# syntax=docker/dockerfile:1.7
#
# Gateway container.
#
# Three build stages: web SPA, Rust binary, slim runtime.
# Build from the repo root:
#
#   docker build -f docker/gateway/Dockerfile -t crates-music/gateway .
#
# The runtime image only carries the compiled artifacts plus the
# entrypoint glue — no Rust toolchain, no node, no build deps.

ARG DEBIAN_VERSION=bookworm

# ─── Stage 1: build the React SPA ─────────────────────────────────────
FROM node:22-alpine AS web-builder
WORKDIR /web

# Dependency manifests first so the npm-install layer caches independent
# of source changes.
COPY apps/web/package.json apps/web/package-lock.json ./
RUN npm ci

COPY apps/web/ ./
# Inject the git SHA for the Settings → About build stamp. The build
# context carries no .git, so vite.config.ts reads it from GIT_SHA instead
# of calling `git`. Pass with: --build-arg GIT_SHA=$(git rev-parse --short HEAD)
ARG GIT_SHA=unknown
ENV GIT_SHA=$GIT_SHA
RUN npm run build

# ─── Stage 2: build the Rust gateway binary ───────────────────────────
FROM rust:1-slim-${DEBIAN_VERSION} AS rust-builder

RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        ca-certificates \
        g++ \
        cmake \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Copy the whole workspace. cargo's path deps would force this for any
# crate touched in the dep graph, so there's nothing to save by being
# clever about partial copies — buildkit's layer cache handles it.
COPY Cargo.toml Cargo.lock rust-toolchain.toml rustfmt.toml ./
COPY crates ./crates

RUN cargo build --release -p music-gateway

# ─── Stage 3: slim runtime ────────────────────────────────────────────
FROM debian:${DEBIAN_VERSION}-slim AS runtime

# Runtime deps:
#   - ca-certificates: TLS to Navidrome
#   - libssl3: linked by the rust binary
#   - libsqlite3-0: linked at runtime by sqlx
#   - openssl: entrypoint mints a self-signed cert if none is present
#   - python3: gen_config.py templater
#   - curl: docker HEALTHCHECK
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl3 \
        libsqlite3-0 \
        openssl \
        python3 \
        curl \
    && rm -rf /var/lib/apt/lists/*

# Non-root execution. The gateway never needs root capabilities; running
# unprivileged limits blast radius if a future bug lets a request reach
# the shell.
#
# uid/gid pinned to 10001 to match the embedder container — both write
# to the same `gw-data` volume (the embedder opens
# `gateway-state.recommend.sqlite` directly to write UMAP projections),
# so the numeric ids must agree across containers or one side fails on
# permission checks. 10001 is well above the system range and safe from
# colliding with future base-image users.
RUN groupadd --system --gid 10001 gateway \
 && useradd --system --uid 10001 --gid gateway \
        --home-dir /var/empty --shell /usr/sbin/nologin gateway

COPY --from=rust-builder /src/target/release/music-gateway /usr/local/bin/music-gateway
COPY --from=web-builder /web/dist /etc/gateway/web/dist
COPY docker/gateway/entrypoint.sh /opt/gateway/entrypoint.sh
COPY docker/gateway/gen_config.py /opt/gateway/gen_config.py
RUN chmod +x /opt/gateway/entrypoint.sh

# /data is the persistent volume mount point. The entrypoint expects to
# be able to write under it (state, certs, audio cache, config).
RUN mkdir -p /data && chown gateway:gateway /data
VOLUME ["/data"]

ENV DATA_DIR=/data \
    GATEWAY_BIN=/usr/local/bin/music-gateway \
    GEN_CONFIG_PATH=/opt/gateway/gen_config.py \
    WEB_STATIC_DIR=/etc/gateway/web/dist \
    RUST_LOG=info,music_gateway=info,sqlx=warn

USER gateway
EXPOSE 8443

# Liveness — `/healthz` returns 200 whenever the HTTP layer is up.
# `/readyz` is *too strict* for the container health signal: a
# degraded (embedder-unreachable) gateway is still serving Subsonic,
# OAuth, sync, and audio cache traffic, and we don't want docker /
# NAS app platforms to recycle it. Operators see degradation via
# /v1/diagnostics and the explicit /readyz endpoint instead.
# Self-signed certs at first boot mean curl needs -k. The 30 s
# start_period covers TLS bind + SQLite migrations.
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD curl --silent --fail --insecure https://localhost:8443/healthz \
        || exit 1

ENTRYPOINT ["/opt/gateway/entrypoint.sh"]
