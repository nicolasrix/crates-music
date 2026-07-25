# music-player

**Path:** `crates/music-player/`
**Type:** library
**Test count:** 8 (most playback skipped — no audio device in CI)

Native audio playback. Three layers:

1. **Cache resolution** — pure I/O glue: take a `TrackId`, return
   audio bytes from the L3 cache or download them.
2. **Decoding + playback (blocking)** — `rodio` + `symphonia`. Must run
   inside `tokio::task::spawn_blocking` because `rodio::OutputStream`
   holds a non-Send handle to the OS audio device. Powers
   `crates-cli play` (sample-accurate gapless).
3. **Interactive playback** — [`Player`] (a handle to a dedicated audio
   thread with pause/seek/volume/position) plus [`PlayQueue`] (a pure,
   device-free transport-queue state machine). Powers the CLI's TUI.

Used by the CLI. The web app — which is also the mobile client, as an
installable PWA — uses the browser's `<audio>` element (MSE remains a
deferred option). There is no native mobile app (P4 retired), so this
crate is **CLI-only**.

## Public API

```rust
use bytes::Bytes;
use music_player::{play_blocking, play_queue_blocking};

// Single track
play_blocking(audio_bytes)?;

// Queue of pre-fetched bytes — gapless
play_queue_blocking(vec![track1_bytes, track2_bytes, track3_bytes])?;
```

Both functions block until playback finishes. Callers wrap them in
`spawn_blocking`:

```rust
let bytes = resolve_source(&cache, &track_id).await?;
tokio::task::spawn_blocking(move || play_blocking(bytes))
    .await
    .unwrap()?;
```

## Interactive playback: `Player` + `PlayQueue`

The TUI needs transport controls the blocking API can't offer. Two new
pieces (both in this crate so any future native client can reuse them):

- **`Player::spawn(events) -> Result<Player, PlayError>`** starts a
  dedicated audio thread that owns the `!Send` `OutputStream`/`Sink`.
  The handle exposes fire-and-forget commands — `load(bytes, track_id,
  duration)`, `toggle`/`pause`/`resume`, `seek_to`/`seek_by`,
  `set_volume` (clamped 0–2), `stop` — plus `snapshot() ->
  PlaybackSnapshot { track_id, position, duration, playing, volume }`,
  republished by the thread every ~100 ms for per-frame polling.
  Out-of-band `PlayerEvent::{TrackEnded, Error}` arrive on the tokio
  channel passed to `spawn`. A deliberate-clear flag inside the thread
  guarantees `TrackEnded` only fires on natural drain — `load`/`stop`
  never emit it, so callers can't double-advance. `spawn` fails fast
  (bootstrap channel) when there is no audio device; callers degrade to
  browse-only instead of crashing.
- **`PlayQueue`** holds the queue + cursor (`replace`, `enqueue`,
  `enqueue_next`, `remove` (cursor-preserving), `advance`, `previous`,
  `jump`, `next_up`). It is deliberately audio-free: all transport
  logic is unit-tested in CI, and the audio thread stays dumb. The
  player holds **one track at a time**; the caller drives handoff on
  `TrackEnded` and prefetches `next_up()`'s bytes into RAM for
  near-gapless starts (decode-from-RAM begins in milliseconds).
  Sample-accurate gapless remains the blocking API's domain.

## Why blocking

`rodio::OutputStream::try_default()` returns an
`(OutputStream, OutputStreamHandle)`. The `OutputStream` must stay
alive for the duration of playback, but it's `!Send`. You can't hold
it across an `.await`.

The pragmatic answer is: don't try to. Drop into a blocking task
where the audio handle owns its thread, and let that thread block on
the sink draining.

Async-friendly playback (where `play()` returns a future) would
require either a third-party rodio fork or a custom `cpal`
integration. The cost-benefit doesn't justify it for a CLI player.

## Gapless playback

`play_queue_blocking` pre-constructs decoders for every track in the
queue and feeds them all into a single `Sink`. Rodio handles
sample-rate conversion per source, so a 44.1 kHz track followed by a
48 kHz track plays without glitching at the boundary.

> "Gapless playback. Decoder pipeline keeps the next track pre-rolled.
> Sample-accurate hand-off."
> *— `CLAUDE.md`*

The trade-off: building decoders up front means we hold all queue
audio in memory simultaneously. At single-user scale with an
album-length queue (~10 tracks × ~5 MB each = 50 MB), that's fine.

## Resolution

`resolve_source(&cache, &track_id) -> Bytes` is the canonical "give
me the audio for this track" function. Tries the cache first; if
miss, returns a `ResolveError::NotCached`. The caller (CLI command
handler) decides whether to download.

```rust
match resolve_source(&cache, &track_id).await {
    Ok(bytes) => play_track(bytes),
    Err(ResolveError::NotCached) => {
        let bytes = download_via_gateway(&track_id).await?;
        cache.put(&track_id, &bytes).await?;
        play_track(bytes);
    }
    Err(e) => return Err(e.into()),
}
```

This split keeps the player crate from depending on the network. The
network code lives in `music-cli` where it has access to gateway
config.

## Empty queues

```rust
play_queue_blocking(vec![])?;
```

Returns `Ok(())` immediately without touching the audio device.
Useful so callers don't need to special-case "queue is empty" before
calling.

> "Empty queues are a no-op and do not touch the audio device."
> *— `crates/music-player/src/lib.rs`*

## Errors

`PlayError`:
- `Decode(symphonia::core::errors::Error)` — symphonia failed to
  parse the audio stream.
- `Output(rodio::PlayError)` — couldn't open the audio device.
- `Sink(rodio::PlayError)` — couldn't append to the sink.

`ResolveError`:
- `NotCached` — track isn't in L3.
- `Cache(music_cache::Error)` — SQL error reading the cache.

## Tests

8 tests. The actual `rodio::OutputStream::try_default()` path is
skipped in CI because there's no audio device. What's tested:

- `resolve_source` returns `NotCached` on a miss.
- `resolve_source` reads bytes back identically after a `cache.put`.
- `play_queue_blocking(vec![])` returns immediately.
- Symphonia's decoder errors propagate cleanly.

End-to-end "actually plays audio" is exercised manually during
development.

## Known gaps

- **Interactive playback is near-gapless, not sample-accurate** — the
  `Player` loads one track at a time; the boundary costs a decoder
  start from RAM (milliseconds). Appending decoder N+1 to the sink
  shortly before N drains would close even that; not wired yet.
- **`Player` seek support depends on the container** —
  `Sink::try_seek` works for the symphonia decoders that support it;
  failures surface as a `PlayerEvent::Error` ("seek not supported"),
  never a crash.
- **Sync-aware playback** (driving the local player from the gateway's
  shared queue state) still isn't wired — the TUI queue is local.
