#!/bin/sh
# Embedder failover watchdog.
#
# Polls the *primary* embedder (typically the GPU sidecar on another host)
# and, when it goes unhealthy, starts a local CPU fallback embedder via
# `docker compose`. When the primary recovers it stops the fallback again to
# reclaim RAM. The gateway's own background re-probe (the `[embedder]`
# fallback_urls list) is what actually *switches traffic* — this watchdog
# only manages the fallback container's lifecycle.
#
# Toggle with EMBEDDER_AUTOSTART=true. When false (default) the container
# stays inert so it can sit in the compose file disabled.
#
# Required on the host: the Docker socket mounted, an image with the
# `docker compose` plugin, the project compose files mounted at /project,
# and the fallback embedder image already built/pulled.

set -eu

log() { echo "[embedder-watchdog] $*"; }

if [ "${EMBEDDER_AUTOSTART:-false}" != "true" ]; then
    log "EMBEDDER_AUTOSTART is not 'true' — failover disabled; idling."
    # Sleep forever without spinning; the container stays up so toggling the
    # env and restarting is all it takes to enable.
    exec sleep 2147483647
fi

PRIMARY_URL="${EMBEDDER_PRIMARY_URL:?set EMBEDDER_PRIMARY_URL (the primary embedder, e.g. the GPU sidecar)}"
HEALTH_PATH="${EMBEDDER_HEALTH_PATH:-/healthz}"
INTERVAL="${EMBEDDER_CHECK_INTERVAL:-30}"
FAIL_THRESHOLD="${EMBEDDER_FAIL_THRESHOLD:-2}"   # consecutive misses before we start the fallback
OK_THRESHOLD="${EMBEDDER_OK_THRESHOLD:-3}"       # consecutive hits before we stop it again
SERVICE="${EMBEDDER_FALLBACK_SERVICE:-embedder-fallback}"
PROBE_TIMEOUT="${EMBEDDER_PROBE_TIMEOUT:-5}"
BEARER="${EMBEDDER_BEARER_TOKEN:-}"

# Strip a trailing slash so URL + path don't double up.
HEALTH_URL="${PRIMARY_URL%/}${HEALTH_PATH}"

if ! docker compose version >/dev/null 2>&1; then
    log "FATAL: 'docker compose' is unavailable in this image."
    log "Set WATCHDOG_IMAGE to an image that bundles the compose v2 plugin (e.g. docker:cli)."
    exit 1
fi

# Healthy = HTTP 2xx AND the body reports the model loaded. A reachable but
# still-loading sidecar counts as down so we don't tear the fallback away
# before the primary can actually serve.
primary_healthy() {
    if [ -n "$BEARER" ]; then
        body=$(curl -fsS --max-time "$PROBE_TIMEOUT" \
            -H "Authorization: Bearer $BEARER" "$HEALTH_URL" 2>/dev/null) || return 1
    else
        body=$(curl -fsS --max-time "$PROBE_TIMEOUT" "$HEALTH_URL" 2>/dev/null) || return 1
    fi
    echo "$body" | grep -q '"model_loaded"[[:space:]]*:[[:space:]]*true' || return 1
    return 0
}

fallback_running() {
    docker compose ps --services --filter status=running 2>/dev/null \
        | grep -qx "$SERVICE"
}

start_fallback() {
    log "starting CPU fallback '$SERVICE' (primary unhealthy ${1}x)"
    if docker compose up -d --no-build "$SERVICE"; then
        log "fallback '$SERVICE' started"
    else
        log "ERROR: failed to start '$SERVICE' — is its image built and the compose file mounted?"
    fi
}

stop_fallback() {
    log "stopping CPU fallback '$SERVICE' (primary healthy ${1}x) — reclaiming RAM"
    docker compose stop "$SERVICE" || log "ERROR: failed to stop '$SERVICE'"
}

log "watching $HEALTH_URL every ${INTERVAL}s (start after ${FAIL_THRESHOLD} misses, stop after ${OK_THRESHOLD} hits)"

fails=0
oks=0
while true; do
    if primary_healthy; then
        oks=$((oks + 1))
        fails=0
        if [ "$oks" -ge "$OK_THRESHOLD" ] && fallback_running; then
            stop_fallback "$oks"
        fi
    else
        fails=$((fails + 1))
        oks=0
        if [ "$fails" -ge "$FAIL_THRESHOLD" ] && ! fallback_running; then
            start_fallback "$fails"
        fi
    fi
    sleep "$INTERVAL"
done
