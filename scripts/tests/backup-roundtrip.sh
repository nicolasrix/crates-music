#!/usr/bin/env bash
# Roundtrip test for scripts/backup.sh + scripts/restore.sh.
#
# Populates a fake state directory with two SQLite databases and a
# certs/ pair, runs a backup, wipes the state, runs a restore, and
# verifies that every byte of irreplaceable state survives the trip.
#
# Exit 0 on success; non-zero on any failure (set -e). Designed to run
# from CI or as a manual smoke (`bash scripts/tests/backup-roundtrip.sh`).

set -euo pipefail

# Resolve repo root from this script's location so the test works no
# matter the caller's cwd. dirname → scripts/tests → scripts → repo.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
BACKUP="$REPO_ROOT/scripts/backup.sh"
RESTORE="$REPO_ROOT/scripts/restore.sh"

WORKDIR="$(mktemp -d -t crates-music-backup-test.XXXXXX)"
trap 'rm -rf "$WORKDIR"' EXIT

STATE="$WORKDIR/state"
OUT="$WORKDIR/out"
DEST="$WORKDIR/restored"
mkdir -p "$STATE/certs" "$OUT" "$DEST"

# --- fixtures -----------------------------------------------------------

# Two SQLite databases with one row each — enough to verify byte-identical
# restore (we'll checksum the dbs end to end) without coupling to the
# real schema.
sqlite3 "$STATE/gateway-state.sqlite" \
  "CREATE TABLE t (k TEXT PRIMARY KEY, v TEXT); INSERT INTO t VALUES ('marker', 'state-db-payload');"
sqlite3 "$STATE/gateway-state.recommend.sqlite" \
  "CREATE TABLE t (k TEXT PRIMARY KEY, v TEXT); INSERT INTO t VALUES ('marker', 'recommend-db-payload');"

# Cert pair — just placeholder bytes; backup treats them as opaque blobs.
# Use the mkcert-style naming the real dev gateway uses, so this test
# catches regressions in the "what file names exist in certs/" axis.
printf 'PEM-CERT-FIXTURE\n' > "$STATE/certs/gateway.local.pem"
printf 'PEM-KEY-FIXTURE\n'  > "$STATE/certs/gateway.local-key.pem"

# Capture pre-backup checksums of every file the contract promises to
# restore. The recommend db comparison is post-`.backup` so we hash
# *table contents* (.dump) instead of raw bytes — sqlite's online
# backup may rewrite page layout without changing logical state.
hash_db() { sqlite3 "$1" .dump | sha256sum | awk '{print $1}'; }
hash_file() { sha256sum "$1" | awk '{print $1}'; }

EXPECT_STATE_DB="$(hash_db "$STATE/gateway-state.sqlite")"
EXPECT_RECOMMEND_DB="$(hash_db "$STATE/gateway-state.recommend.sqlite")"
EXPECT_CERT="$(hash_file "$STATE/certs/gateway.local.pem")"
EXPECT_KEY="$(hash_file "$STATE/certs/gateway.local-key.pem")"

# --- act ----------------------------------------------------------------

echo "== running backup =="
"$BACKUP" "$STATE" "$OUT"

# Exactly one archive should land in $OUT, name-stamped.
shopt -s nullglob
ARCHIVES=("$OUT"/crates-music-backup-*.tar.gz)
shopt -u nullglob
if [[ ${#ARCHIVES[@]} -ne 1 ]]; then
  echo "FAIL: expected 1 backup archive in $OUT, found ${#ARCHIVES[@]}" >&2
  exit 1
fi
ARCHIVE="${ARCHIVES[0]}"
echo "produced: $ARCHIVE"

# Wipe original state to prove restore is the source of truth.
rm -rf "$STATE"

echo "== running restore =="
"$RESTORE" "$ARCHIVE" "$DEST"

# --- assert -------------------------------------------------------------

assert_eq() {
  local what="$1" expected="$2" actual="$3"
  if [[ "$expected" != "$actual" ]]; then
    echo "FAIL: $what mismatch" >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    exit 1
  fi
  echo "OK: $what"
}

assert_eq "state.sqlite contents"      "$EXPECT_STATE_DB"     "$(hash_db "$DEST/gateway-state.sqlite")"
assert_eq "recommend.sqlite contents"  "$EXPECT_RECOMMEND_DB" "$(hash_db "$DEST/gateway-state.recommend.sqlite")"
assert_eq "certs/gateway.local.pem"    "$EXPECT_CERT"         "$(hash_file "$DEST/certs/gateway.local.pem")"
assert_eq "certs/gateway.local-key.pem" "$EXPECT_KEY"         "$(hash_file "$DEST/certs/gateway.local-key.pem")"

# Private key must be 0600 after restore (defense-in-depth).
KEY_MODE="$(stat -c %a "$DEST/certs/gateway.local-key.pem")"
[[ "$KEY_MODE" == "600" ]] || { echo "FAIL: key mode = $KEY_MODE, expected 600" >&2; exit 1; }
echo "OK: private key mode locked to 0600"

# --- safety: restore must refuse to clobber a non-empty dest by default --

echo "== restore must refuse to clobber =="
if "$RESTORE" "$ARCHIVE" "$DEST" 2>/dev/null; then
  echo "FAIL: restore did not refuse to clobber a non-empty dest" >&2
  exit 1
fi
echo "OK: restore refused non-empty dest"

# --- and accept --force ------------------------------------------------

echo "== restore --force should overwrite =="
"$RESTORE" --force "$ARCHIVE" "$DEST"
assert_eq "state.sqlite after --force" "$EXPECT_STATE_DB" "$(hash_db "$DEST/gateway-state.sqlite")"

# --- edge: backup without optional recommend.sqlite -------------------

echo "== backup without recommend.sqlite =="
STATE2="$WORKDIR/state2"
OUT2="$WORKDIR/out2"
DEST2="$WORKDIR/restored2"
mkdir -p "$STATE2/certs" "$OUT2" "$DEST2"
sqlite3 "$STATE2/gateway-state.sqlite" \
  "CREATE TABLE t (k TEXT); INSERT INTO t VALUES ('only-state');"
# This branch uses the docker-entrypoint cert naming (cert.pem/key.pem)
# to cover the *other* shape — between this and the main case above,
# both naming conventions are exercised.
printf 'CERT\n' > "$STATE2/certs/cert.pem"
printf 'KEY\n'  > "$STATE2/certs/key.pem"
"$BACKUP" "$STATE2" "$OUT2"
shopt -s nullglob
A2=("$OUT2"/crates-music-backup-*.tar.gz)
shopt -u nullglob
"$RESTORE" "${A2[0]}" "$DEST2"
[[ ! -f "$DEST2/gateway-state.recommend.sqlite" ]] || {
  echo "FAIL: recommend.sqlite should not be in archive when source had none" >&2
  exit 1
}
echo "OK: optional recommend.sqlite handled correctly"

# --- safety: a corrupted archive must be rejected ---------------------

echo "== corrupted archive detection =="
CORRUPT="$WORKDIR/corrupt.tar.gz"
cp "$ARCHIVE" "$CORRUPT"
# Flip a byte deep enough into the gzip stream to defeat header magic
# but still produce a "extractable" archive with bad payload checksums.
# (Restoring should fail at the sha256 verify step, not at tar -xz.)
python3 -c "
import sys
p = sys.argv[1]
with open(p, 'rb') as f: data = bytearray(f.read())
# Find the embedded sqlite header 'SQLite' inside the gzip (will fail
# decompression), or fall back to flipping a payload byte at 60% mark.
i = max(64, len(data) * 6 // 10)
data[i] ^= 0xFF
with open(p, 'wb') as f: f.write(data)
" "$CORRUPT"
if "$RESTORE" --force "$CORRUPT" "$DEST" 2>/dev/null; then
  echo "FAIL: corrupted archive was accepted" >&2
  exit 1
fi
echo "OK: corrupted archive rejected"

echo ""
echo "all checks passed"
