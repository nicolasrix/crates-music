#!/usr/bin/env bash
#
# Compose stack smoke test.
#
# Drives the full docker-compose deploy with the `smoke` profile, which
# brings up a minimal Navidrome stand-in so the gateway's /readyz
# healthcheck can reach `healthy`. Steps:
#
#   1. `docker compose --profile smoke build`
#   2. `docker compose --profile smoke up -d`
#   3. Poll /healthz on the gateway until it's 200 (liveness).
#   4. Poll /readyz on the gateway until it's 200 (readiness — proves
#      both Navidrome and the embedder probes resolved).
#   5. Verify both containers reach the `healthy` state.
#   6. Verify the gateway logged the embedder as reachable.
#   7. `docker compose down -v` — never leave volumes behind, since
#      the bearer.token would otherwise survive across test runs.
#
# Exits 0 on success, non-zero on any failure.

set -euo pipefail

cd "$(dirname "$0")/../.."

PROJECT="cratesmusic-smoke"
COMPOSE=(docker compose -p "$PROJECT" --profile smoke)
PORT="${SMOKE_PORT:-18443}"

cleanup() {
    "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

# Required env vars are interpolated by compose at parse time on EVERY
# subcommand (build, up, logs, ...). Exporting them once is simpler
# than threading them through each command.
#
# NAVIDROME_URL points at the `navidrome-stub` compose service so
# /readyz can succeed without a real Navidrome on the LAN.
export NAVIDROME_URL=http://navidrome-stub:4533
export NAVIDROME_USERNAME=alice
export NAVIDROME_PASSWORD=wonderland
export GATEWAY_PORT="$PORT"

echo "==> building images"
"${COMPOSE[@]}" build >/dev/null

echo "==> starting stack on port $PORT"
"${COMPOSE[@]}" up -d

echo "==> waiting for /healthz (liveness)"
deadline=$(( $(date +%s) + 60 ))
healthy=false
while [ "$(date +%s)" -lt "$deadline" ]; do
    if curl --silent --insecure --fail "https://localhost:$PORT/healthz" >/dev/null; then
        healthy=true
        break
    fi
    sleep 2
done
if [ "$healthy" != true ]; then
    echo "FAIL: gateway never responded to /healthz in 60s" >&2
    "${COMPOSE[@]}" logs --tail=60 >&2
    exit 1
fi

echo "==> verifying /healthz response payload"
body=$(curl --silent --insecure "https://localhost:$PORT/healthz")
echo "    $body"
echo "$body" | grep -q '"status":"ok"' \
    || { echo "FAIL: unexpected /healthz body" >&2; exit 1; }
echo "$body" | grep -q '"service":"music-gateway"' \
    || { echo "FAIL: missing service field" >&2; exit 1; }

echo "==> waiting for /readyz (strict readiness)"
deadline=$(( $(date +%s) + 60 ))
ready=false
while [ "$(date +%s)" -lt "$deadline" ]; do
    if curl --silent --insecure --fail "https://localhost:$PORT/readyz" >/dev/null; then
        ready=true
        break
    fi
    sleep 2
done
if [ "$ready" != true ]; then
    echo "FAIL: gateway /readyz never returned 200 in 60s" >&2
    curl --silent --insecure "https://localhost:$PORT/readyz" >&2 || true
    "${COMPOSE[@]}" logs --tail=60 >&2
    exit 1
fi

echo "==> verifying /readyz payload"
body=$(curl --silent --insecure "https://localhost:$PORT/readyz")
echo "    $body"
echo "$body" | grep -q '"status":"ready"' \
    || { echo "FAIL: /readyz did not report ready" >&2; exit 1; }
echo "$body" | grep -q '"navidrome":{"status":"ok"' \
    || { echo "FAIL: /readyz did not confirm navidrome reachable" >&2; exit 1; }

echo "==> verifying embedder can see the recommend SQLite"
# The auto-projection task (gateway-side) passes the absolute path of
# `/data/state/gateway-state.recommend.sqlite` to the embedder's
# /reduce endpoint. Both containers must agree on that path AND the
# embedder must be allowed to write to it — otherwise the reducer
# fails 400 (file missing) or 500 (read-only) and the latent-space
# diagnostic surface stays empty.
REC_DB="/data/state/gateway-state.recommend.sqlite"
"${COMPOSE[@]}" exec -T embedder test -f "$REC_DB" \
    || { echo "FAIL: embedder cannot see $REC_DB — volume not shared?" >&2; exit 1; }
"${COMPOSE[@]}" exec -T embedder test -w "$REC_DB" \
    || { echo "FAIL: embedder cannot write $REC_DB — uid mismatch?" >&2; exit 1; }
echo "    embedder sees $REC_DB (read+write)"

echo "==> verifying embedder boot probe succeeded"
# Gateway logs `embedder: probe ok` or `embedder: ready` after talking
# to the sidecar at startup. A "disabled" or "unreachable" line means
# cross-container DNS broke or the embedder didn't start.
gw_logs=$("${COMPOSE[@]}" logs gateway 2>&1)
if echo "$gw_logs" | grep -qE 'embedder: (disabled|unreachable)'; then
    echo "FAIL: gateway did not connect to embedder" >&2
    echo "$gw_logs" | tail -20 >&2
    exit 1
fi
if ! echo "$gw_logs" | grep -qE 'embedder: probe ok|embedder: ready'; then
    echo "FAIL: gateway log missing embedder-ready signal" >&2
    echo "$gw_logs" | tail -20 >&2
    exit 1
fi

echo "==> verifying container health states"
for svc in gateway embedder; do
    cid=$("${COMPOSE[@]}" ps -q "$svc")
    state=$(docker inspect --format '{{.State.Health.Status}}' "$cid" 2>/dev/null || echo "unknown")
    if [ "$state" != "healthy" ] && [ "$state" != "starting" ]; then
        # `starting` is acceptable within the start-period; `healthy`
        # is the steady-state goal. Anything else (`unhealthy`,
        # `none`, `unknown`) is a regression — the new /readyz-based
        # gateway healthcheck should now reach `healthy` because the
        # navidrome-stub satisfies the readiness probe.
        echo "FAIL: $svc state is $state" >&2
        exit 1
    fi
    echo "    $svc → $state"
done

echo "==> PASS"
