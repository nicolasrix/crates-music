# music-cache

**Path:** `crates/music-cache/`
**Type:** library
**Test count:** 35

Two caches in one crate:

1. **L2 metadata cache** — SQLite-backed, ETag-keyed. Stores the JSON
   bodies of cacheable browse responses (`getAlbumList2`, `getAlbum`).
2. **L3 audio cache** — content-addressed file cache. Stores encoded
   audio files keyed by `(track_id, bitrate, codec)`.

They're in the same crate because they share the SQLite pool, but
their concerns are separate. The L2 cache is a write-through HTTP
cache; the L3 cache is a pinning-aware file LRU.

## L2 metadata cache

```rust
use music_cache::{Cache, etag_for};

let cache = Cache::open("gateway-cache.sqlite").await?;

// Try fresh cache first
if let Some(entry) = cache.get("album:abc").await? {
    if cache.is_fresh(&entry, ttl) {
        return Ok(entry.body);
    }
    // Stale: revalidate with If-None-Match: entry.etag
}

// On miss or 200 from upstream:
let etag = etag_for(&body);
cache.put("album:abc", &body, &etag).await?;
```

The cache stores opaque bytes. Whatever wraps it (in the gateway
proxy handler) decides what JSON shape goes in.

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

### Tables

```sql
CREATE TABLE entries (
    key         TEXT PRIMARY KEY,
    body        BLOB NOT NULL,
    etag        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    accessed_at INTEGER NOT NULL
);
```

`accessed_at` is updated on each read, in service of a future LRU
eviction. There's no eviction yet — it's not needed at single-user
scale where the metadata DB is in the dozens of MB.

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

35 tests. Each test gets a `tempfile::tempdir()` so file paths don't
collide. Coverage:

- L2: put/get/expire round-trips, ETag computation, TTL semantics,
  stale-but-revalidatable entries.
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
