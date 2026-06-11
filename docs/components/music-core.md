# music-core

**Path:** `crates/music-core/`
**Type:** library, no I/O
**Test count:** 24

Pure domain types. Other crates depend on these and convert their
wire formats (Subsonic JSON, gateway responses, sync ops) into them.
This crate has no `tokio`, no `reqwest`, no `sqlx` — just `serde`
and `thiserror`.

## What lives here

| Module | Exports |
|---|---|
| `ids` | Newtype IDs: `TrackId`, `AlbumId`, `ArtistId`, `QueueItemId`. All wrap `String`. |
| `track` | `Track` — `id`, `title`, `artist`, `album`, `duration`, plus optional upstream metadata: `year`, `genre`, `play_count`, `played_at` (all `Option`, `skip_serializing_if` — older servers omit them). |
| `album` | `Album` — `id`, `name`, `artist`, `year`, `track_count`, plus optional `play_count` / `played_at` mirroring `Track`'s caveats. |
| `artist` | `Artist` — `id`, `name`, `album_count`. |
| `queue` | `Queue`, `QueueItem`. The queue carries `current_index` so seeking is a single op. |
| `playback` | `PlaybackState` — `track_id`, `position_ms`, `playing`. |

Everything implements `Clone`, `Debug`, `serde::Serialize`,
`serde::Deserialize`, and `PartialEq`. Equality is value-based, which
matters for sync conflict detection.

## Why newtype IDs

`TrackId`, `AlbumId`, `ArtistId` are all `String` underneath, but
they're distinct types. The compiler will reject:

```rust
fn fetch_track(id: TrackId) -> Track { ... }

let album_id: AlbumId = ...;
fetch_track(album_id);  // compile error: expected TrackId, got AlbumId
```

This is worth the small ergonomic cost. The Subsonic API mixes IDs
freely in JSON (every field is just `"id": "..."`), so without
newtypes we'd have plenty of `fetch_album(track.id)` bugs.

## Conversion patterns

Each downstream crate owns its conversion. Subsonic → core lives in
`music-subsonic::wire`:

```rust
// music-subsonic/src/wire.rs
impl From<wire::Album> for music_core::Album { ... }
```

Sync ops → core domain mutations live in `music-sync::ops`. Cache
serialization is just `bincode`/`serde_json` of the core types — no
custom format.

This means `music-core` can stay free of dependencies on storage and
wire concerns. The cost is some boilerplate in the conversion sites,
which has been tolerable.

## When to add a type here vs. a downstream crate

Add to `music-core` if:
- Two or more crates need the same type
- It's a stable concept independent of where it came from (a track,
  an album, a queue position)

Keep it in the downstream crate if:
- It's specific to one wire format (Subsonic-flavoured pagination
  metadata, OAuth token shapes)
- It's transient state that no other crate cares about (the cache's
  internal row representation)

When in doubt, start downstream and lift up only when a second crate
needs it.

## Notable design

> "Pure data with no I/O. Other crates depend on these types and
> convert their wire formats into them."
> *— `crates/music-core/src/lib.rs`*

The "no I/O" rule is enforced by absence: this crate has no async
runtime in its dependency closure. If it did, every downstream crate
would inherit it. As-is, you can use these types in WASM, in build
scripts, anywhere.

## Tests

`crates/music-core/tests/` covers:
- Serde round-trips for every public type.
- Equality semantics (especially around `Queue::current_index`).
- ID newtype `From<&str>` / `as_str` / `into_inner` conversions.
- The optional metadata fields (`year`, `genre`, `play_count`,
  `played_at`) deserializing as `None` when upstream omits them.

24 tests, all unit-level, all run in milliseconds.
