//! Interactive player: a handle to a dedicated audio thread.
//!
//! Why a thread and not async: rodio's `OutputStream` is `!Send` and must
//! stay alive for as long as anything should be audible, so it is created
//! *inside* the audio thread and lives in a local there. The handle talks to
//! the thread over a command channel and observes it through a shared
//! [`PlaybackSnapshot`] the thread republishes every loop iteration (the
//! 100 ms `recv_timeout` on the command channel doubles as the publish
//! cadence — no busy loop, and sub-100 ms command latency is imperceptible
//! next to a keypress).
//!
//! The player deliberately holds **one source at a time** (load → play →
//! natural drain emits [`PlayerEvent::TrackEnded`]). Queue orchestration and
//! byte-fetching stay with the caller, which owns a
//! [`crate::PlayQueue`] and prefetches the next track's bytes for
//! near-gapless handoff — decoding from RAM starts in milliseconds. The
//! sample-accurate gapless path for non-interactive use remains
//! [`crate::play_queue_blocking`].

use std::io::Cursor;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use bytes::Bytes;
use rodio::Source as _;
use tokio::sync::mpsc::UnboundedSender;

use crate::PlayError;

/// Observable playback state, republished by the audio thread ~10×/s.
/// Cheap to clone; the TUI polls one per frame.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackSnapshot {
    /// Id of the loaded track; `None` when idle (nothing loaded, decode
    /// failure, or the last track drained).
    pub track_id: Option<String>,
    pub position: Duration,
    /// From track metadata, passed in on load — rodio can't always know it.
    pub duration: Option<Duration>,
    /// True iff a source is loaded and the sink is not paused.
    pub playing: bool,
    pub volume: f32,
}

impl Default for PlaybackSnapshot {
    fn default() -> Self {
        Self {
            track_id: None,
            position: Duration::ZERO,
            duration: None,
            playing: false,
            volume: 1.0,
        }
    }
}

/// Out-of-band notifications from the audio thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayerEvent {
    /// The loaded track drained naturally (never emitted for an explicit
    /// `load`/`stop` — those clear the sink deliberately).
    TrackEnded,
    /// Non-fatal problem (decode failure, unsupported seek). The player
    /// stays alive; the caller decides whether to skip or stop.
    Error(String),
}

enum Cmd {
    Load {
        bytes: Bytes,
        track_id: String,
        duration: Option<Duration>,
    },
    Play,
    Pause,
    Toggle,
    Seek(Duration),
    SetVolume(f32),
    Stop,
    Shutdown,
}

/// Handle to the audio thread. Dropping it shuts the thread down cleanly.
#[derive(Debug)]
pub struct Player {
    cmd_tx: mpsc::Sender<Cmd>,
    shared: Arc<Mutex<PlaybackSnapshot>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Player {
    /// Spawn the audio thread. Fails fast (with the device error) when no
    /// audio output is available — the error travels back over a bootstrap
    /// channel because the `OutputStream` can only be created on the audio
    /// thread itself.
    pub fn spawn(events: UnboundedSender<PlayerEvent>) -> Result<Self, PlayError> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let shared = Arc::new(Mutex::new(PlaybackSnapshot::default()));
        let (boot_tx, boot_rx) = mpsc::channel::<Result<(), PlayError>>();

        let thread_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("audio".to_owned())
            .spawn(move || audio_thread(&cmd_rx, &thread_shared, &events, &boot_tx))
            .map_err(|e| PlayError::Thread(e.to_string()))?;

        match boot_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                cmd_tx,
                shared,
                thread: Some(handle),
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                Err(PlayError::Thread("audio thread died during startup".to_owned()))
            }
        }
    }

    /// Load a fully-buffered track and start playing it, replacing whatever
    /// was loaded before (no `TrackEnded` is emitted for the replaced track).
    pub fn load(&self, bytes: Bytes, track_id: String, duration: Option<Duration>) {
        self.send(Cmd::Load {
            bytes,
            track_id,
            duration,
        });
    }

    pub fn resume(&self) {
        self.send(Cmd::Play);
    }

    pub fn pause(&self) {
        self.send(Cmd::Pause);
    }

    pub fn toggle(&self) {
        self.send(Cmd::Toggle);
    }

    pub fn seek_to(&self, pos: Duration) {
        self.send(Cmd::Seek(pos));
    }

    /// Seek relative to the current position, clamped to `[0, duration]`.
    pub fn seek_by(&self, delta_secs: i64) {
        let snap = self.snapshot();
        self.send(Cmd::Seek(clamp_seek(snap.position, snap.duration, delta_secs)));
    }

    /// Set volume; clamped to `[0.0, 2.0]` (100% = 1.0).
    pub fn set_volume(&self, volume: f32) {
        self.send(Cmd::SetVolume(volume.clamp(0.0, 2.0)));
    }

    /// Stop playback and unload the track (no `TrackEnded` is emitted).
    pub fn stop(&self) {
        self.send(Cmd::Stop);
    }

    /// Current playback state; cheap clone, poll freely.
    #[must_use]
    pub fn snapshot(&self) -> PlaybackSnapshot {
        self.shared
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    fn send(&self, cmd: Cmd) {
        // A send failure means the audio thread is gone; the snapshot stops
        // updating and the UI shows idle — nothing useful to do here.
        let _ = self.cmd_tx.send(cmd);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(Cmd::Shutdown);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// Compute a relative-seek target clamped to `[0, duration]`. Pure — the
/// arithmetic is tested in CI even though seeking itself needs a device.
#[must_use]
pub fn clamp_seek(position: Duration, duration: Option<Duration>, delta_secs: i64) -> Duration {
    let target = if delta_secs.is_negative() {
        position.saturating_sub(Duration::from_secs(delta_secs.unsigned_abs()))
    } else {
        position.saturating_add(Duration::from_secs(delta_secs.unsigned_abs()))
    };
    duration.map_or(target, |d| target.min(d))
}

/// Seek within the current source, returning the new absolute offset of the
/// source's sample zero (the caller's `seek_base`), or `None` to leave it
/// unchanged.
///
/// A native `try_seek` keeps rodio's position counter absolute → base `0`.
/// When the container reports itself unseekable to symphonia (some
/// transcoded/streamed MP3s do, even though the whole file is in RAM), we
/// rebuild the decoder and fast-forward by discarding the leading `pos` — a
/// format-agnostic seek. That fresh source's counter restarts at zero, so the
/// seek target *is* the new absolute base. A decoder-rebuild failure leaves the
/// current source untouched, hence `None`.
fn seek_within(
    sink: &rodio::Sink,
    current_bytes: Option<&Bytes>,
    pos: Duration,
    shared: &Arc<Mutex<PlaybackSnapshot>>,
    events: &UnboundedSender<PlayerEvent>,
) -> Option<Duration> {
    if sink.try_seek(pos).is_ok() {
        return Some(Duration::ZERO);
    }
    let bytes = current_bytes?;
    match rodio::Decoder::new(Cursor::new(bytes.clone())) {
        Ok(source) => {
            let was_paused = sink.is_paused();
            sink.clear(); // no TrackEnded: re-appended immediately below
            sink.append(source.skip_duration(pos));
            if was_paused {
                sink.pause();
            } else {
                sink.play();
            }
            if let Ok(mut s) = shared.lock() {
                s.position = pos;
            }
            Some(pos)
        }
        Err(e) => {
            let _ = events.send(PlayerEvent::Error(format!(
                "seek not supported for this track: {e}"
            )));
            None
        }
    }
}

/// The only rodio-touching code in interactive playback. Kept dumb on
/// purpose: no queue knowledge, no retry policy — just the sink.
fn audio_thread(
    cmd_rx: &mpsc::Receiver<Cmd>,
    shared: &Arc<Mutex<PlaybackSnapshot>>,
    events: &UnboundedSender<PlayerEvent>,
    boot_tx: &mpsc::Sender<Result<(), PlayError>>,
) {
    // OutputStream must live here (it is !Send) for the thread's lifetime.
    let (_stream, handle) = match rodio::OutputStream::try_default() {
        Ok(pair) => pair,
        Err(e) => {
            let _ = boot_tx.send(Err(e.into()));
            return;
        }
    };
    let sink = match rodio::Sink::try_new(&handle) {
        Ok(sink) => sink,
        Err(e) => {
            let _ = boot_tx.send(Err(e.into()));
            return;
        }
    };
    let _ = boot_tx.send(Ok(()));

    // True while a source is loaded that we have NOT deliberately cleared —
    // the guard that makes a natural drain the only source of `TrackEnded`
    // (otherwise load/stop would double-advance the caller's queue).
    let mut track_loaded = false;

    // Kept for the seek-by-reload fallback: `Bytes` is Arc-backed, so holding
    // the current track's whole file costs one refcount, and lets us rebuild
    // the decoder when a container reports itself unseekable to symphonia.
    let mut current_bytes: Option<Bytes> = None;
    // Absolute offset of the loaded source's sample-zero. Zero for normal /
    // natively-seeked playback (rodio's counter is already absolute); set to
    // the seek target after a reload-skip, where the counter restarts at zero.
    let mut seek_base = Duration::ZERO;

    loop {
        match cmd_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Cmd::Load {
                bytes,
                track_id,
                duration,
            }) => {
                sink.clear(); // deliberate: also pauses; no TrackEnded for the old source
                seek_base = Duration::ZERO;
                match rodio::Decoder::new(Cursor::new(bytes.clone())) {
                    Ok(source) => {
                        sink.append(source);
                        sink.play();
                        track_loaded = true;
                        current_bytes = Some(bytes);
                        if let Ok(mut s) = shared.lock() {
                            s.track_id = Some(track_id);
                            s.duration = duration;
                            s.position = Duration::ZERO;
                        }
                    }
                    Err(e) => {
                        track_loaded = false;
                        current_bytes = None;
                        if let Ok(mut s) = shared.lock() {
                            s.track_id = None;
                            s.duration = None;
                        }
                        let _ = events.send(PlayerEvent::Error(format!(
                            "failed to decode {track_id}: {e}"
                        )));
                    }
                }
            }
            Ok(Cmd::Play) => sink.play(),
            Ok(Cmd::Pause) => sink.pause(),
            Ok(Cmd::Toggle) => {
                if sink.is_paused() {
                    sink.play();
                } else {
                    sink.pause();
                }
            }
            Ok(Cmd::Seek(pos)) => {
                if let Some(base) = seek_within(&sink, current_bytes.as_ref(), pos, shared, events)
                {
                    seek_base = base;
                }
            }
            Ok(Cmd::SetVolume(v)) => sink.set_volume(v),
            Ok(Cmd::Stop) => {
                sink.clear(); // deliberate — no TrackEnded
                track_loaded = false;
                current_bytes = None;
                seek_base = Duration::ZERO;
                if let Ok(mut s) = shared.lock() {
                    s.track_id = None;
                    s.duration = None;
                    s.position = Duration::ZERO;
                }
            }
            Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }

        // Publish observable state every iteration (commands and timeouts
        // alike), so position advances while idle-looping during playback.
        let drained = track_loaded && sink.empty();
        if let Ok(mut s) = shared.lock() {
            // `seek_base` is zero unless a reload-skip is active, where rodio's
            // counter restarts at zero — add the target back for an absolute
            // position (see the seek-by-reload fallback above).
            s.position = seek_base + sink.get_pos();
            s.playing = !sink.is_paused() && !sink.empty();
            s.volume = sink.volume();
            if drained {
                s.track_id = None;
            }
        }
        if drained {
            track_loaded = false;
            let _ = events.send(PlayerEvent::TrackEnded);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_seek_forward_within_duration() {
        let pos = Duration::from_secs(30);
        let dur = Some(Duration::from_mins(2));
        assert_eq!(clamp_seek(pos, dur, 10), Duration::from_secs(40));
    }

    #[test]
    fn clamp_seek_forward_clamps_to_duration() {
        let pos = Duration::from_mins(2) - Duration::from_secs(5);
        let dur = Some(Duration::from_mins(2));
        assert_eq!(clamp_seek(pos, dur, 10), Duration::from_mins(2));
    }

    #[test]
    fn clamp_seek_backward_clamps_to_zero() {
        let pos = Duration::from_secs(4);
        assert_eq!(clamp_seek(pos, None, -10), Duration::ZERO);
    }

    #[test]
    fn clamp_seek_backward_within_bounds() {
        let pos = Duration::from_secs(30);
        assert_eq!(clamp_seek(pos, None, -10), Duration::from_secs(20));
    }

    #[test]
    fn clamp_seek_without_duration_is_unbounded_forward() {
        let pos = Duration::from_secs(30);
        assert_eq!(clamp_seek(pos, None, 60), Duration::from_secs(90));
    }
}
