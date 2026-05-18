# syntax=docker/dockerfile:1.7
#
# Caddy reverse proxy with the Cloudflare DNS-01 provider baked in.
#
# Why a custom build at all: Caddy's `tls { dns cloudflare … }` directive
# requires the github.com/caddy-dns/cloudflare module to be linked into
# the binary. The stock `caddy:2-alpine` image doesn't include it (the
# upstream image stays minimal and DNS providers are opt-in). `xcaddy`
# is Caddy's official build-with-plugins tool — fetches the module
# source, compiles a static binary, ships in a slim runtime image.
#
# Build from the repo root:
#
#   docker build -f docker/caddy/Dockerfile -t crates-music/caddy:dev .
#
# Resulting image is ~50 MB (Caddy is one Go static binary plus an
# alpine base; the Cloudflare provider adds <1 MB).

FROM caddy:2-builder-alpine AS builder
RUN xcaddy build \
    --with github.com/caddy-dns/cloudflare

FROM caddy:2-alpine
COPY --from=builder /usr/bin/caddy /usr/bin/caddy

# Caddy's default healthcheck endpoint is its admin API on :2019,
# loopback-only by default. `caddy` user already exists in the base
# image; no extra user-management needed.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget --quiet --tries=1 --spider http://localhost:2019/config/ || exit 1
