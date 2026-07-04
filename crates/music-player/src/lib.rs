//! Audio playback for native clients (CLI today; mobile uses Media3 directly).
//!
//! Three layers:
//!   - [`resolve_source`] — pure I/O glue between [`AudioCache`] and a
//!     caller-supplied fetcher closure. Cache hit returns the on-disk blob
//!     and bumps `last_accessed_at`; cache miss invokes the fetcher and
//!     stores the result. Fully testable without hardware.
//!   - [`play_blocking`] — rodio + symphonia decode + playback.
//!     Must be invoked from a `spawn_blocking` context, not the async
//!     runtime. Untested in CI (no audio device).
//!   - [`Player`] + [`PlayQueue`] — interactive playback for the TUI: a
//!     handle to a dedicated audio thread (pause/seek/volume/position) plus
//!     a pure, device-free transport-queue state machine.

#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

mod player;
mod queue;
mod source;

pub use player::{PlaybackSnapshot, Player, PlayerEvent, clamp_seek};
pub use queue::{PlayQueue, QueuedTrack};
pub use source::{ResolveError, read_cached, resolve_source};

use std::io::Cursor;

use bytes::Bytes;

#[derive(Debug, thiserror::Error)]
pub enum PlayError {
    #[error("audio output unavailable: {0}")]
    Stream(#[from] rodio::StreamError),
    #[error("failed to construct sink: {0}")]
    Sink(#[from] rodio::PlayError),
    #[error("decoder rejected stream: {0}")]
    Decode(#[from] rodio::decoder::DecoderError),
    #[error("audio thread failed: {0}")]
    Thread(String),
}

/// Decode `bytes` and play to the default audio output, blocking until the
/// stream ends. Call from inside `tokio::task::spawn_blocking`.
pub fn play_blocking(bytes: Bytes) -> Result<(), PlayError> {
    play_queue_blocking(vec![bytes])
}

/// Play a queue of audio buffers gaplessly to the default audio output,
/// blocking until the last source ends.
///
/// **How gapless works here:** every buffer's decoder is constructed up-front
/// and appended to the same [`rodio::Sink`]. rodio's queue source drains
/// decoder N to completion, then immediately starts pulling samples from
/// decoder N+1. Because all decoders are ready before playback starts, there
/// is no decoder-startup delay at the boundary — the audio output thread
/// never sees a gap.
///
/// Sample-rate mismatches across queued tracks are handled by rodio's
/// per-source resampler; the boundary is sample-accurate.
///
/// Empty queues are a no-op and do not touch the audio device.
/// Call from inside `tokio::task::spawn_blocking`.
pub fn play_queue_blocking(queue: Vec<Bytes>) -> Result<(), PlayError> {
    if queue.is_empty() {
        return Ok(());
    }
    let (_stream, handle) = rodio::OutputStream::try_default()?;
    let sink = rodio::Sink::try_new(&handle)?;
    for bytes in queue {
        let source = rodio::Decoder::new(Cursor::new(bytes))?;
        sink.append(source);
    }
    sink.sleep_until_end();
    Ok(())
}
