//! Optional MPRIS D-Bus bridge (Linux only).
//!
//! Exposes `org.mpris.MediaPlayer2` + `.Player` so desktop environments and
//! media-key daemons can drive and observe the TUI player — the Linux analog
//! of the PWA's Media Session. Two directions:
//!
//! * **In:** D-Bus method calls (PlayPause/Next/Previous/Seek/…) become
//!   [`Msg`]s on the same channel keys and background tasks already feed.
//! * **Out:** each frame the event loop [`publish`](Bridge::publish)es a
//!   snapshot; a background task diffs it and emits `PropertiesChanged`.
//!
//! The `mpris-server` [`Player`] is `Rc`-backed (`!Send`), so it lives on a
//! dedicated thread with a current-thread runtime + `LocalSet`. The bridge
//! talks to it over a `watch` channel (state out) and clones of `msg_tx`
//! (commands in). Building the server fails cleanly when there's no session
//! bus (headless / SSH / CI) — the TUI just runs without MPRIS.

use tokio::sync::mpsc::UnboundedSender;

use super::msg::Msg;
use super::state::App;

/// Handle owned by the event loop. Dropping it (on quit) drops the `watch`
/// sender, which ends the bridge thread's watcher loop and tears down the
/// D-Bus name. A no-op zero-sized value on non-Linux platforms.
pub(super) struct Bridge {
    #[cfg(target_os = "linux")]
    tx: tokio::sync::watch::Sender<imp::State>,
}

impl Bridge {
    /// Spawn the bridge. Never fails the TUI: on non-Linux, or if the D-Bus
    /// server can't be created, this returns a handle whose `publish` is a
    /// no-op.
    pub(super) fn spawn(msg_tx: UnboundedSender<Msg>) -> Self {
        #[cfg(target_os = "linux")]
        {
            let (tx, rx) = tokio::sync::watch::channel(imp::State::default());
            imp::spawn_thread(msg_tx, rx);
            Self { tx }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = msg_tx;
            Self {}
        }
    }

    /// Push the current player state to the D-Bus side. Cheap and called every
    /// frame; the `watch` only wakes the bridge when a field actually changed.
    pub(super) fn publish(&self, app: &App) {
        #[cfg(target_os = "linux")]
        {
            let next = imp::State::from_app(app);
            self.tx.send_if_modified(|cur| {
                if *cur == next {
                    false
                } else {
                    *cur = next;
                    true
                }
            });
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = app;
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::rc::Rc;

    use mpris_server::{Metadata, PlaybackStatus, Player, Time, TrackId};
    use tokio::sync::mpsc::UnboundedSender;
    use tokio::sync::watch;

    use super::super::msg::Msg;
    use super::super::state::App;

    /// `org.mpris.MediaPlayer2.<suffix>`. Alphanumeric only — D-Bus name
    /// elements can't contain hyphens.
    const BUS_NAME_SUFFIX: &str = "cratesmusic";

    /// Plain, `Send` snapshot of everything the D-Bus side mirrors. Derived
    /// from [`App`] on the UI thread and shipped over a `watch` channel so the
    /// bridge thread never touches `App`.
    #[derive(Clone, PartialEq)]
    pub(super) struct State {
        status: Status,
        track_id: Option<String>,
        title: String,
        artist: Option<String>,
        album: Option<String>,
        /// Track length in microseconds; 0 when unknown.
        length_us: i64,
        position_us: i64,
        can_next: bool,
        can_prev: bool,
        volume: f64,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Status {
        Playing,
        Paused,
        Stopped,
    }

    impl Default for State {
        fn default() -> Self {
            Self {
                status: Status::Stopped,
                track_id: None,
                title: String::new(),
                artist: None,
                album: None,
                length_us: 0,
                position_us: 0,
                can_next: false,
                can_prev: false,
                volume: 1.0,
            }
        }
    }

    impl State {
        pub(super) fn from_app(app: &App) -> Self {
            let current = app.queue.current();
            let status = if current.is_none() {
                Status::Stopped
            } else if app.playback.playing {
                Status::Playing
            } else {
                Status::Paused
            };
            let idx = app.queue.current_index();
            let can_next = current.is_some()
                && (app.autoplay.enabled || idx.is_some_and(|i| i + 1 < app.queue.len()));
            let can_prev = idx.is_some_and(|i| i > 0);
            // µs of any real track fits i64; saturate rather than wrap on the
            // pathological case.
            let length_us = current
                .and_then(|t| t.duration)
                .and_then(|d| i64::try_from(d.as_micros()).ok())
                .unwrap_or(0);
            let position_us = i64::try_from(app.playback.position.as_micros()).unwrap_or(i64::MAX);
            Self {
                status,
                track_id: current.map(|t| t.id.clone()),
                title: current.map_or_else(String::new, |t| t.title.clone()),
                artist: current.and_then(|t| t.artist.clone()),
                album: current.and_then(|t| t.album.clone()),
                length_us,
                position_us,
                can_next,
                can_prev,
                volume: f64::from(app.playback.volume),
            }
        }

        fn playback_status(&self) -> PlaybackStatus {
            match self.status {
                Status::Playing => PlaybackStatus::Playing,
                Status::Paused => PlaybackStatus::Paused,
                Status::Stopped => PlaybackStatus::Stopped,
            }
        }

        fn metadata(&self) -> Metadata {
            let mut b = Metadata::builder().trackid(track_path(self.track_id.as_deref()));
            if !self.title.is_empty() {
                b = b.title(self.title.clone());
            }
            if let Some(artist) = &self.artist {
                b = b.artist([artist.clone()]);
            }
            if let Some(album) = &self.album {
                b = b.album(album.clone());
            }
            if self.length_us > 0 {
                b = b.length(Time::from_micros(self.length_us));
            }
            b.build()
        }
    }

    /// Map a Navidrome track id to a valid D-Bus object path. Non-path
    /// characters are replaced so `TrackId::try_from` can't fail; falls back to
    /// the spec's "no track" sentinel when there's nothing playing.
    fn track_path(id: Option<&str>) -> TrackId {
        let Some(id) = id else {
            return TrackId::NO_TRACK;
        };
        let sanitized: String = id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        TrackId::try_from(format!("/io/cratesmusic/track/{sanitized}"))
            .unwrap_or(TrackId::NO_TRACK)
    }

    pub(super) fn spawn_thread(msg_tx: UnboundedSender<Msg>, rx: watch::Receiver<State>) {
        // A detached thread: on quit the UI drops the watch sender, the
        // watcher loop below ends, and the runtime + D-Bus name tear down.
        let spawned = std::thread::Builder::new()
            .name("mpris".to_owned())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::debug!(error = %e, "mpris: no runtime; media keys disabled");
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, serve(msg_tx, rx));
            });
        if let Err(e) = spawned {
            tracing::debug!(error = %e, "mpris: could not spawn thread");
        }
    }

    async fn serve(msg_tx: UnboundedSender<Msg>, mut rx: watch::Receiver<State>) {
        let player = match Player::builder(BUS_NAME_SUFFIX)
            .identity("crates-music")
            .can_play(true)
            .can_pause(true)
            .can_go_next(true)
            .can_go_previous(true)
            .can_seek(true)
            .can_control(true)
            .build()
            .await
        {
            Ok(p) => Rc::new(p),
            Err(e) => {
                // No session bus (headless/SSH), or the name is taken by another
                // instance. Non-fatal — the TUI runs fine without MPRIS.
                tracing::info!(error = %e, "mpris: D-Bus unavailable; media keys disabled");
                return;
            }
        };

        connect_controls(&player, &msg_tx);
        tokio::task::spawn_local(player.run());

        // Prime the D-Bus properties from the first snapshot, then mirror every
        // subsequent change until the UI drops the sender.
        let mut prev = State::default();
        loop {
            let next = rx.borrow_and_update().clone();
            apply(&player, &prev, &next).await;
            prev = next;
            if rx.changed().await.is_err() {
                break; // UI gone → shut the bridge down
            }
        }
    }

    /// Wire D-Bus method calls to reducer messages. Each closure is a cheap,
    /// non-blocking `send` on the unbounded channel.
    fn connect_controls(player: &Rc<Player>, msg_tx: &UnboundedSender<Msg>) {
        macro_rules! forward {
            ($connect:ident, $msg:expr) => {{
                let tx = msg_tx.clone();
                player.$connect(move |_| {
                    let _ = tx.send($msg);
                });
            }};
        }
        forward!(connect_play_pause, Msg::TransportToggle);
        forward!(connect_play, Msg::TransportPlay);
        forward!(connect_pause, Msg::TransportPause);
        // Stop maps to Pause, not a queue-clearing stop: a stray media-key
        // Stop shouldn't wipe the user's queue.
        forward!(connect_stop, Msg::TransportPause);
        forward!(connect_next, Msg::TransportNext);
        forward!(connect_previous, Msg::TransportPrev);

        // Relative seek (µs offset, may be negative) → whole-second SeekBy.
        let tx = msg_tx.clone();
        player.connect_seek(move |_, offset| {
            let _ = tx.send(Msg::SeekBy(offset.as_secs()));
        });
    }

    /// Emit `PropertiesChanged` only for fields that actually changed. Position
    /// updates via the sync setter (no signal — MPRIS clients poll `Position`).
    async fn apply(player: &Player, prev: &State, next: &State) {
        if prev.status != next.status {
            let _ = player.set_playback_status(next.playback_status()).await;
        }
        if prev.track_id != next.track_id
            || prev.title != next.title
            || prev.artist != next.artist
            || prev.album != next.album
            || prev.length_us != next.length_us
        {
            let _ = player.set_metadata(next.metadata()).await;
        }
        if prev.can_next != next.can_next {
            let _ = player.set_can_go_next(next.can_next).await;
        }
        if prev.can_prev != next.can_prev {
            let _ = player.set_can_go_previous(next.can_prev).await;
        }
        if (prev.volume - next.volume).abs() > f64::EPSILON {
            let _ = player.set_volume(next.volume).await;
        }
        player.set_position(Time::from_micros(next.position_us));
    }
}
