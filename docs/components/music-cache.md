# music-cache

**Path:** `crates/music-cache/`
**Type:** library
**Test count:** 37

Two caches in one crate:

1. **L2 metadata cache** — SQLite-backed, ETag-keyed. Stores the
   opaque bodies of cacheable browse responses (`getAlbumList2`,
   `getAlbum`, `getArtists`, `getArtist`, `search3`) **and** cover-art
   bytes (keyed under a `getCoverArt|` prefix).
2. **L3 audio cache** — content-addressed file cache. Stores encoded
   audio files keyed by `(track_id, bitrate, codec)`.

They're in the same crate because they share the SQLite pool, but
their concerns are separate. The L2 cache is a write-through HTTP
cache; the L3 cache is a pinning-aware file LRU.

## L2 metadata cache

```rust
use music_cache::Cache;

let cache = Cache::open(Path::new("gateway-cache.sqlite")).await?;

// Try fresh cache first
if let Some(entry) = cache.get("getAlbum|id=123").await? {
    if entry.is_fresh(SystemTime::now()) {
        return Ok(entry.body);
    }
    // Stale: revalidate upstream with If-None-Match: entry.etag
}

// On miss or 200 from upstream — the etag is computed internally
// (sha256(body) truncated, see `etag_for`):
cache.put("getAlbum|id=123", body, ttl).await?;
```

The cache stores opaque bytes. Whatever wraps it (in the gateway
proxy handler) decides what JSON shape goes in.

`get` and `put` are `#[tracing::instrument]`ed as `cache.lookup`
(records `hit`) and `cache.write` (records `bytes`), feeding the
diagnostics trace store. The gateway runs `put` **off the request hot
path** — the response is returned to the client before the cache write
is awaited — so a slow disk write never adds to user-perceived
latency.

### ETags

ETags are the first 16 hex chars of `sha256(body)`:

```rust
pub fn etag_for(body: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(body);
    format!("{:x}", h.finalize())[..16].to_string()
}
```

This means:
- Same body → same ETag, regardless of which gateway process or
  machine generated it.
- An ETag mismatch genuinely means the body changed.

Contrast with random or timestamp-based ETags, which would make
`If-None-Match` useless across restarts.

> "ETags are the first 16 hex chars of SHA-256(body) so they are
> stable across processes and across restarts."
> *— `crates/music-cache/src/lib.rs`*

### TTL

Each entry has a `ttl_seconds` configured per call site (typically
`browse_ttl_seconds` from gateway config, default 24 h). `is_fresh`
checks `now < created_at + ttl`.

After TTL expiry, the entry is **stale, not gone**. The proxy handler
sends `If-None-Match: <etag>` to upstream; if upstream returns 304,
we mark the entry fresh again without re-downloading.

This is the cheapest possible cache strategy: TTL bounds the *check*
rate, ETags bound the *transfer* rate.

### Bulk invalidation & cover-art self-healing

Beyond per-key `delete`, the L2 cache exposes etag- and prefix-scoped
sweeps:

```rust
cache.keys_with_etag(etag).await?;        // every key whose body hashes to etag
cache.delete_keys_with_etag(etag).await?; // flush all copies of one body
cache.clear_browse().await?;              // drop everything except getCoverArt|…
cache.clear_covers().await?;              // drop only getCoverArt|… entries
```

`clear_browse` is what `POST /v1/admin/cache/invalidate` calls (gateway
side) to force a refetch of browse responses without waiting out
`browse_ttl_seconds`; cover art is left alone because its keys are
already content-addressed by Navidrome's `coverArt` ids.

The etag pair powers **cover-art placeholder self-healing**. Navidrome
serves the same default "no artwork" image for any track without
embedded art, so that body shows up in the cache under many distinct
`getCoverArt|…` keys with one shared etag. When the gateway classifier
identifies a hash as the placeholder, `delete_keys_with_etag` flushes
every cached copy so subsequent requests re-run through detection and
get the SVG substitute. (The classifier itself and the SVG substitution
live gateway-side, not in this crate — this crate only provides the
etag-scoped storage primitives.)

### Tables

```sql
CREATE TABLE cache_entries (
    key          TEXT PRIMARY KEY NOT NULL,
    etag         TEXT NOT NULL,
    body         BLOB NOT NULL,
    fetched_at   INTEGER NOT NULL,  -- unix epoch seconds
    ttl_seconds  INTEGER NOT NULL CHECK (ttl_seconds >= 0)
);
CREATE INDEX cache_entries_fetched_at_idx ON cache_entries(fetched_at);
```

The same `cache_entries` table backs both browse JSON and cover-art
bytes; the two are distinguished only by key prefix (`getCoverArt|…`
for art, otherwise a browse key).

TTL freshness is decided per-entry from `fetched_at + ttl_seconds`
(see `Entry::is_fresh(now)`), not from a `created_at`/`accessed_at`
pair. There's no LRU eviction yet — it's not needed at single-user
scale where the metadata DB is in the dozens of MB. `expire_before`
trims entries past their deadline; the `fetched_at` index keeps that
sweep cheap.

## L3 audio cache

```rust
use music_cache::audio::{AudioCache, AudioKey};

let audio = AudioCache::open(&cache_dir).await?;
let key = AudioKey {
    track_id: TrackId::from("track_a"),
    bitrate: 320,
    codec: "mp3".into(),
};

// Read
let entry = audio.get(&key).await?;
if let Some(e) = entry {
    return Ok(std::fs::read(&e.path)?);
}

// Miss — download, then put
let bytes = download(...);
audio.put(&key, &bytes).await?;
```

### Content addressing

> "Audio cache is content-addressed by `(trackId, bitrate, codec)`,
> never by URL."
> *— `CLAUDE.md`*

URLs change when the gateway address changes. The track itself
doesn't. Indexing by `(track_id, bitrate, codec)` makes the cache
portable: if you switch gateway hosts, your local audio cache stays
useful.

### Pinning

Pinned tracks live in a separate budget that's never LRU-evicted.
This is the offline-mode story: a user pins a playlist, the audio
files stay on disk no matter how many other tracks they listen to.

```rust
audio.pin(&key).await?;
let outcome = audio.unpin(&key).await?;  // returns whether it was pinned
```

Two budgets in `[cache]`:
- `regular_budget_bytes` — LRU-evicted to fit
- `pinned_budget_bytes` — pinned tracks; eviction returns an error
  ("you're out of space, drop something") instead of silently
  evicting

The CLI exposes this:

```bash
music pin <track_id>     # pin (auto-fetch if not cached)
music unpin <track_id>
music pinned             # list
music cache stats
music cache evict        # force fit-to-budget
```

## In-memory mode

`Cache::open_in_memory()` and `AudioCache::open_in_memory()` both
spin up single-connection in-memory pools for tests. In-memory SQLite
isn't shared across connections, so the pool is forced to one
connection. Production never uses this — it's strictly a test seam.

## Tests

37 tests (24 in `tests/audio.rs`, 13 in `tests/cache.rs`). Each test
gets a `tempfile::tempdir()` so file paths don't collide. Coverage:

- L2: put/get/expire round-trips, ETag computation, TTL semantics,
  stale-but-revalidatable entries, and `clear_browse` keeping
  `getCoverArt|` keys while dropping browse keys.
- L3: put/get/delete, pinning, eviction, budget arithmetic, pinned
  budget overflow returns an error rather than evicting silently.

## Known gaps

- **No L4 transcoded-audio cache yet** — that lives in the gateway,
  not here, and isn't implemented. The `[cache]` config block has the
  hooks; the gateway proxy handler will populate it when transcoding
  lands.
- **No FTS5 search index** — when the metadata cache grows search
  responsibilities (P5+ "search-while-offline"), we'll add a search
  index here, probably as a virtual FTS5 table.
- **Eviction for L2** — not currently implemented because metadata
  fits comfortably in tens of MB at single-user scale. If a future
  user has a 100k-track library, this becomes a real concern.
