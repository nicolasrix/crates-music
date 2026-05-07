#!/usr/bin/env bash
# Generate a local TLS cert for `music-gateway` via mkcert.
#
# - Idempotent: re-running just regenerates the gateway.local cert.
# - `mkcert -install` is run only if no local CA is present yet.
# - Output: certs/gateway.local.pem (cert), certs/gateway.local-key.pem (key).
#
# Each *client device* (your phone, another laptop, etc.) also needs to trust
# the local CA. After running this once on the gateway machine, copy the
# rootCA file from `mkcert -CAROOT` to each client and install it. On
# Android, install via Settings → Security → Encryption & Credentials →
# Install from storage. The OAuth-via-Custom-Tabs flow won't work without
# the CA being trusted by the system browser.

set -euo pipefail

if ! command -v mkcert >/dev/null 2>&1; then
    echo "mkcert is not installed. See https://github.com/FiloSottile/mkcert" >&2
    exit 1
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CERT_DIR="${REPO_ROOT}/certs"
HOSTNAME="${MUSIC_GATEWAY_HOSTNAME:-gateway.local}"

mkdir -p "${CERT_DIR}"

# Install the local CA into the system trust store if it's not there yet.
# `mkcert -install` is itself idempotent but prompts on first run.
CAROOT="$(mkcert -CAROOT)"
if [ ! -f "${CAROOT}/rootCA.pem" ]; then
    echo "Installing mkcert local CA into system trust store..."
    mkcert -install
fi

CERT_FILE="${CERT_DIR}/${HOSTNAME}.pem"
KEY_FILE="${CERT_DIR}/${HOSTNAME}-key.pem"

mkcert \
    -cert-file "${CERT_FILE}" \
    -key-file "${KEY_FILE}" \
    "${HOSTNAME}" "localhost" "127.0.0.1" "::1"

echo
echo "Wrote:"
echo "  cert: ${CERT_FILE}"
echo "  key:  ${KEY_FILE}"
echo
echo "If clients reach the gateway over the LAN, add an /etc/hosts entry on"
echo "each client (or run mDNS/avahi):"
echo "  <gateway-ip>  ${HOSTNAME}"
