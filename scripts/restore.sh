#!/usr/bin/env bash
# Restore a backup archive produced by scripts/backup.sh into a state
# directory. Verifies every file's sha256 against the manifest before
# placing it — a corrupted archive fails loudly, not quietly.
#
# Usage:  restore.sh [--force] <archive.tar.gz> <dest-dir>
#
# Refuses to clobber a non-empty <dest-dir> unless --force is given.
# Always shut the gateway down first; restoring while the DBs are open
# corrupts both the live and restored state.

set -euo pipefail

usage() {
  cat >&2 <<EOF
usage: $(basename "$0") [--force] <archive.tar.gz> <dest-dir>

  --force      overwrite an existing non-empty <dest-dir>
  archive      backup tarball produced by scripts/backup.sh
  dest-dir     directory to place restored state into (created if absent)

stop the gateway before running this. restoring over live DBs corrupts both.
EOF
  exit 64
}

FORCE=0
ARGS=()
for arg in "$@"; do
  case "$arg" in
    --force) FORCE=1 ;;
    -h|--help) usage ;;
    *) ARGS+=("$arg") ;;
  esac
done

[[ ${#ARGS[@]} -eq 2 ]] || usage

ARCHIVE="${ARGS[0]}"
DEST="${ARGS[1]}"

[[ -f "$ARCHIVE" ]] || { echo "archive not found: $ARCHIVE" >&2; exit 1; }
command -v tar >/dev/null 2>&1       || { echo "tar not in PATH" >&2; exit 1; }
command -v sha256sum >/dev/null 2>&1 || { echo "sha256sum not in PATH" >&2; exit 1; }

mkdir -p "$DEST"
# Non-empty == any file or subdir, hidden or otherwise. The `-mindepth 1`
# clause keeps `$DEST` itself from counting as content.
if [[ $FORCE -eq 0 ]] && find "$DEST" -mindepth 1 -print -quit | grep -q .; then
  echo "dest-dir is not empty (pass --force to overwrite): $DEST" >&2
  exit 1
fi

STAGE="$(mktemp -d -t crates-music-restore.XXXXXX)"
trap 'rm -rf "$STAGE"' EXIT

echo "extracting archive..."
tar -xzf "$ARCHIVE" -C "$STAGE"

[[ -f "$STAGE/manifest.json" ]] || { echo "archive missing manifest.json" >&2; exit 1; }

echo "verifying checksums..."
# Parse {"path":"...","size":N,"sha256":"..."} entries without a JSON
# parser dep. The manifest format is producer-controlled (we write it
# in backup.sh), so a regex match is safe here — we're not parsing
# arbitrary user JSON.
while IFS=$'\t' read -r path want_sum; do
  [[ -z "$path" ]] && continue
  full="$STAGE/$path"
  [[ -f "$full" ]] || { echo "manifest references missing file: $path" >&2; exit 1; }
  have_sum="$(sha256sum "$full" | awk '{print $1}')"
  if [[ "$have_sum" != "$want_sum" ]]; then
    echo "checksum mismatch for $path" >&2
    echo "  manifest: $want_sum" >&2
    echo "  actual:   $have_sum" >&2
    exit 1
  fi
done < <(
  grep -oE '"path":[[:space:]]*"[^"]+",[[:space:]]*"size":[[:space:]]*[0-9]+,[[:space:]]*"sha256":[[:space:]]*"[a-f0-9]+"' "$STAGE/manifest.json" \
    | sed -E 's/.*"path":[[:space:]]*"([^"]+)".*"sha256":[[:space:]]*"([a-f0-9]+)".*/\1\t\2/'
)

echo "placing files into $DEST..."
# Copy everything *except* the manifest itself — it's a verification
# artifact, not gateway state.
mkdir -p "$DEST/certs"
cp -p "$STAGE/gateway-state.sqlite" "$DEST/gateway-state.sqlite"
if [[ -f "$STAGE/gateway-state.recommend.sqlite" ]]; then
  cp -p "$STAGE/gateway-state.recommend.sqlite" "$DEST/gateway-state.recommend.sqlite"
fi
# Restore whichever cert files the archive carries — naming varies by
# toolchain (mkcert vs entrypoint self-sign), so we don't hard-code.
cp -rp "$STAGE/certs/." "$DEST/certs/"

# Lock down anything that looks like a private key. mkcert names them
# *-key.pem; the entrypoint uses key.pem. cp -p preserves perms from
# the archive, but defense-in-depth: if a wonky umask leaked a 0644
# private key into the source dir, this catches it on the way back out.
shopt -s nullglob
for f in "$DEST/certs/"*key*.pem; do chmod 0600 "$f"; done
shopt -u nullglob

echo "done. start the gateway against $DEST and it should come up restored."
