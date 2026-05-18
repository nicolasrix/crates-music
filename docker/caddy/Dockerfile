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

# Bake the Caddyfile into the image. The file uses {$ENV_VAR}
# substitution for everything host-specific (public hostname, CF
# token, ACME directory), so the same image works for dev and prod —
# behaviour is determined by the env at container start, not the
# Caddyfile content. This makes the image portable to NAS app platforms,
# which doesn't have a clean way to bind-mount files from a repo
# checkout into the Apps UI.
COPY docker/caddy/Caddyfile /etc/caddy/Caddyfile

# Caddy's admin API on :2019 binds to 127.0.0.1 only. We probe via
# the literal IPv4 — `localhost` in Alpine resolves to ::1 first,
# which the admin endpoint doesn't listen on, and busybox wget
# doesn't fall back to v4 → "Connection refused".
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget --quiet --tries=1 --spider http://127.0.0.1:2019/config/ || exit 1
