#!/usr/bin/env bash
# Snapshot the gateway's irreplaceable state into a timestamped tarball.
#
# Usage:  backup.sh <state-dir> <output-dir>
#
# Backs up (in order of how badly you'd miss it):
#   - gateway-state.sqlite             OAuth tokens, master password,
#                                      sync ops, play counts. Required.
#   - gateway-state.recommend.sqlite   CLAP embeddings + ingest queue.
#                                      Optional — skipped if absent.
#   - certs/cert.pem + certs/key.pem   TLS keypair. Required (losing it
#                                      means re-trusting on every device).
#
# Deliberately *not* backed up — all derivable:
#   - gateway-cache.sqlite             L2 metadata cache (re-fetched
#                                      from Navidrome on demand).
#   - gateway-state.traces.sqlite      Diagnostics ring (regenerates
#                                      organically as the gateway runs).
#   - gateway-state.ann + .ann.keys    HNSW mmap (rebuilt at boot from
#                                      embeddings in .recommend.sqlite).
#
# SQLite databases are snapshotted with `.backup` (sqlite3's online
# backup API) — safe to run against a live gateway. A plain `cp` of an
# in-use SQLite file may catch it mid-transaction and produce an
# archive that refuses to open.

set -euo pipefail

usage() {
  cat >&2 <<EOF
usage: $(basename "$0") <state-dir> <output-dir>

  state-dir    directory containing gateway-state.sqlite + certs/
  output-dir   directory to write the timestamped tarball into

example:
  $(basename "$0") /var/lib/crates-music /var/backups/crates-music
EOF
  exit 64
}

[[ $# -eq 2 ]] || usage

STATE_DIR="$1"
OUT_DIR="$2"

[[ -d "$STATE_DIR" ]]               || { echo "state-dir not found: $STATE_DIR" >&2; exit 1; }
[[ -d "$OUT_DIR"   ]]               || { echo "output-dir not found: $OUT_DIR" >&2; exit 1; }
command -v sqlite3 >/dev/null 2>&1  || { echo "sqlite3 CLI not in PATH" >&2; exit 1; }
command -v tar     >/dev/null 2>&1  || { echo "tar not in PATH" >&2; exit 1; }
command -v sha256sum >/dev/null 2>&1 || { echo "sha256sum not in PATH" >&2; exit 1; }

STATE_DB="$STATE_DIR/gateway-state.sqlite"
RECOMMEND_DB="$STATE_DIR/gateway-state.recommend.sqlite"
CERT="$STATE_DIR/certs/cert.pem"
KEY="$STATE_DIR/certs/key.pem"

[[ -f "$STATE_DB" ]] || { echo "missing required: $STATE_DB" >&2; exit 1; }
[[ -f "$CERT"     ]] || { echo "missing required: $CERT" >&2; exit 1; }
[[ -f "$KEY"      ]] || { echo "missing required: $KEY" >&2; exit 1; }

# Staging dir lives next to the output so the final atomic rename
# (mv archive into place) doesn't cross filesystems.
TS="$(date -u +%Y%m%dT%H%M%SZ)"
STAGE="$(mktemp -d -p "$OUT_DIR" ".crates-music-backup-stage.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT

# Inside the staging dir we mirror the *layout* the gateway expects on
# restore — gateway-state.sqlite at root, certs/ as a subdir — so the
# tarball is "extract it and you're done."
mkdir -p "$STAGE/certs"

echo "snapshotting gateway-state.sqlite..."
sqlite3 "$STATE_DB" ".backup '$STAGE/gateway-state.sqlite'"

if [[ -f "$RECOMMEND_DB" ]]; then
  echo "snapshotting gateway-state.recommend.sqlite..."
  sqlite3 "$RECOMMEND_DB" ".backup '$STAGE/gateway-state.recommend.sqlite'"
else
  echo "(skipping .recommend.sqlite — not present)"
fi

echo "copying certs..."
cp -p "$CERT" "$STAGE/certs/cert.pem"
cp -p "$KEY"  "$STAGE/certs/key.pem"

# Manifest: lets restore verify the archive wasn't truncated or
# corrupted in transit, and records the snapshot timestamp + tool
# version for forensic value if a future restore goes sideways.
echo "writing manifest..."
{
  echo "{"
  echo "  \"version\": 1,"
  echo "  \"created_utc\": \"$TS\","
  echo "  \"tool\": \"crates-music backup.sh\","
  echo "  \"files\": ["
  FIRST=1
  while IFS= read -r f; do
    REL="${f#"$STAGE/"}"
    [[ "$REL" == "manifest.json" ]] && continue
    SUM="$(sha256sum "$f" | awk '{print $1}')"
    SIZE="$(stat -c %s "$f")"
    [[ $FIRST -eq 1 ]] || echo ","
    FIRST=0
    printf '    {"path": "%s", "size": %s, "sha256": "%s"}' "$REL" "$SIZE" "$SUM"
  done < <(find "$STAGE" -type f | sort)
  echo ""
  echo "  ]"
  echo "}"
} > "$STAGE/manifest.json"

ARCHIVE="$OUT_DIR/crates-music-backup-$TS.tar.gz"
echo "writing $ARCHIVE..."
tar -czf "$ARCHIVE" -C "$STAGE" .

# Permissions: this archive holds private TLS key + password hashes +
# OAuth refresh tokens. Anyone who can read it owns the deployment.
chmod 0600 "$ARCHIVE"

SIZE_HUMAN="$(du -h "$ARCHIVE" | awk '{print $1}')"
echo "done: $ARCHIVE ($SIZE_HUMAN)"
