#!/usr/bin/env bash
# Ship a locally-built Docker image to a remote host over SSH — no registry.
#
# Usage:  ship-image.sh <image[:tag]> [ssh-host]
#   image     local image ref, e.g. crates-music/embedder-clamp3:dev
#   ssh-host  ssh destination (default: nas-host)
#
# Streams `docker save | gzip` over ssh straight into `docker load` on the
# remote — no intermediate tarball on either disk. This is the no-registry
# path for a single-host homelab: slower than a `docker pull` but needs no
# registry, no auth, no public exposure.
#
# Why this and not `docker pull` on the remote:
#   - We don't run a registry. Pushing to Docker Hub would mean publishing
#     a ~5 GB image (with baked HF weights) to a public namespace.
#   - The image is amd64 and the target host is amd64, so a `docker save`'d
#     tarball loads as-is. (Cross-arch would need buildx --platform.)
#
# Prerequisites on the remote (the ssh user must satisfy these):
#   - docker CLI on PATH and permission to `docker load`. On the NAS host
#     the `admin` user is NOT in the docker group but has passwordless
#     sudo, so set REMOTE_DOCKER="sudo docker" (see below).
#   - gunzip on PATH (coreutils — effectively always present).
#
# REMOTE_DOCKER env overrides the remote docker invocation (default
# "docker"). For the NAS host:
#   REMOTE_DOCKER="sudo docker" ./scripts/ship-image.sh <image> nas-host
#
# After shipping, deploy on the remote with the compose files (which must
# also be present there, e.g. via `git pull` or rsync):
#   docker compose -f docker-compose.yml -f docker-compose.clamp3.yml up -d
#
# NOTE: this ships *code*, not the CLaMP 3 saas checkpoint. That .pth is
# bind-mounted at runtime from $CRATES_CONFIG_DIR/models on the remote and
# must be staged there separately (e.g. rsync) — it is deliberately not
# baked into the image.
set -euo pipefail

IMAGE="${1:?usage: ship-image.sh <image[:tag]> [ssh-host]}"
HOST="${2:-nas-host}"
# Remote docker invocation. some hosts require "sudo docker".
REMOTE_DOCKER="${REMOTE_DOCKER:-docker}"

# Fail early if the local image isn't built yet — a clearer message than a
# mid-stream `docker save` error.
if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "error: no such local image: $IMAGE" >&2
    echo "       build it first, e.g.:" >&2
    echo "       docker build -f docker/embedder/Dockerfile.clamp3 -t $IMAGE ." >&2
    exit 1
fi

# Prefer pigz (parallel gzip) — meaningfully faster on a multi-GB image.
if command -v pigz >/dev/null 2>&1; then
    COMPRESS=(pigz)
else
    COMPRESS=(gzip)
fi

SIZE_BYTES=$(docker image inspect "$IMAGE" --format '{{.Size}}')
SIZE_MB=$((SIZE_BYTES / 1024 / 1024))
echo "shipping $IMAGE (~${SIZE_MB} MB uncompressed) to ${HOST} via ${COMPRESS[0]}..."

# Optional progress meter if `pv` is installed locally; otherwise stream
# straight through. The remote always receives a plain gzip stream.
if command -v pv >/dev/null 2>&1; then
    docker save "$IMAGE" \
        | "${COMPRESS[@]}" \
        | pv -s "$SIZE_BYTES" -N "save+compress" \
        | ssh "$HOST" "gunzip | $REMOTE_DOCKER load"
else
    docker save "$IMAGE" \
        | "${COMPRESS[@]}" \
        | ssh "$HOST" "gunzip | $REMOTE_DOCKER load"
fi

echo "done. verify on ${HOST}:  ${REMOTE_DOCKER} images | grep '${IMAGE%%:*}'"
