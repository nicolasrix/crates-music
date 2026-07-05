//! Listening-signal policy: when to scrobble, when an abandonment counts
//! as a skip, and which queue entries a dislike excludes from playback.
//!
//! Direct ports of the web client's pure modules (`scrobble.ts`,
//! `skip.ts`, `autoSkip.ts`) so both clients report identical signal to
//! Navidrome (play counts) and the gateway (preference affinity). Keep the
//! thresholds in lock-step with those files.

use std::collections::HashMap;
use std::time::Duration;

use music_player::QueuedTrack;

use super::state::Rating;
use crate::api::OutgoingEvent;

/// Tracks under this never scrobble and their abandonment never counts as
/// a skip — stingers/interludes carry no preference signal.
const MIN_SIGNAL_DURATION: Duration = Duration::from_secs(30);
/// Submission threshold cap (the Last.fm convention): half the track, but
/// never more than 4 minutes.
const SUBMISSION_CAP: Duration = Duration::from_mins(4);

/// Per-loaded-track emission state. Reset whenever the player reports a
/// different `track_id`, so the flags follow what is actually audible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackSignal {
    pub track_id: String,
    pub now_playing_sent: bool,
    pub submission_sent: bool,
    /// A skip verdict was already taken for this load (emitted or gated
    /// out) — dedups the "activate + failing resolve + next" pile-ups.
    pub abandoned: bool,
}

impl TrackSignal {
    pub(crate) fn new(track_id: String) -> Self {
        Self {
            track_id,
            now_playing_sent: false,
            submission_sent: false,
            abandoned: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrobbleDecision {
    None,
    NowPlaying,
    Submission,
}

/// What (if anything) to scrobble right now for the loaded track.
/// `duration` comes from track metadata (`None` = never learned it —
/// don't scrobble what we can't threshold).
pub(crate) fn evaluate_scrobble(
    duration: Option<Duration>,
    position: Duration,
    sig: &TrackSignal,
) -> ScrobbleDecision {
    // An abandoned load is one the user already left — the snapshot only
    // still reports it because the player thread lags a tick. Never
    // now-playing-hint a track that was simultaneously reported as a skip.
    if sig.abandoned {
        return ScrobbleDecision::None;
    }
    let Some(duration) = duration else {
        return ScrobbleDecision::None;
    };
    if duration < MIN_SIGNAL_DURATION {
        return ScrobbleDecision::None;
    }
    if !sig.now_playing_sent {
        return ScrobbleDecision::NowPlaying;
    }
    if !sig.submission_sent && position >= (duration / 2).min(SUBMISSION_CAP) {
        return ScrobbleDecision::Submission;
    }
    ScrobbleDecision::None
}

/// Whether a *manual* abandonment of the loaded track should be reported
/// as a skip, and with what position. `None` = don't emit (too short,
/// never actually started, or unknown duration). Natural end-of-track is
/// not a skip — callers simply don't invoke this on that path.
pub(crate) fn evaluate_skip(duration: Option<Duration>, position: Duration) -> Option<u64> {
    let duration = duration?;
    if duration < MIN_SIGNAL_DURATION {
        return None;
    }
    if position.is_zero() {
        return None;
    }
    // Clamp: an honest position keeps the diagnostics readable even if the
    // player briefly reports past the metadata duration near the tail.
    Some(u64::try_from(position.min(duration).as_millis()).unwrap_or(u64::MAX))
}

/// A signal event waiting in the outbox for the next batched
/// `POST /v1/events` flush.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingEvent {
    pub event_type: &'static str,
    pub track_id: String,
    pub occurred_at: i64,
    pub played_ms: Option<u64>,
    /// Failed-flush count; the event is dropped past `MAX_EVENT_ATTEMPTS`.
    pub attempts: u8,
}

/// Flush attempts per event before it is dropped (the signal is advisory —
/// better to lose a skip than to hammer a struggling gateway forever).
pub(crate) const MAX_EVENT_ATTEMPTS: u8 = 3;

impl PendingEvent {
    pub(crate) fn skip(track_id: String, played_ms: u64) -> Self {
        Self {
            event_type: "skip",
            track_id,
            // The one impurity the reducer allows itself — events must
            // carry the moment they happened, not the moment they flushed.
            occurred_at: crate::auth::store::now_ms(),
            played_ms: Some(played_ms),
            attempts: 0,
        }
    }

    pub(crate) fn to_outgoing(&self) -> OutgoingEvent {
        OutgoingEvent {
            event_type: self.event_type.to_owned(),
            track_id: self.track_id.clone(),
            occurred_at: self.occurred_at,
            metadata: self
                .played_ms
                .map(|ms| serde_json::json!({ "played_ms": ms })),
        }
    }
}

/// Whether a queue entry is excluded from play by a dislike at any level —
/// the track itself, its album, or its artist. Mirrors the server-side
/// `disliked_exclusions` union so the player auto-skips exactly what the
/// recommender would never surface.
pub(crate) fn is_disliked(item: &QueuedTrack, ratings: &HashMap<String, Rating>) -> bool {
    let disliked = |id: &str| ratings.get(id) == Some(&Rating::Dislike);
    disliked(&item.id)
        || item.album_id.as_deref().is_some_and(disliked)
        || item.artist_id.as_deref().is_some_and(disliked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    // ── evaluate_scrobble (mirrors scrobble.test.ts) ────────────────────

    #[test]
    fn scrobble_short_track_never_fires() {
        let sig = TrackSignal::new("t".into());
        assert_eq!(
            evaluate_scrobble(Some(secs(20)), secs(19), &sig),
            ScrobbleDecision::None
        );
    }

    #[test]
    fn scrobble_unknown_duration_never_fires() {
        let sig = TrackSignal::new("t".into());
        assert_eq!(evaluate_scrobble(None, secs(100), &sig), ScrobbleDecision::None);
    }

    #[test]
    fn scrobble_now_playing_fires_first() {
        let sig = TrackSignal::new("t".into());
        assert_eq!(
            evaluate_scrobble(Some(secs(180)), secs(0), &sig),
            ScrobbleDecision::NowPlaying
        );
    }

    #[test]
    fn scrobble_submission_at_half_duration() {
        let mut sig = TrackSignal::new("t".into());
        sig.now_playing_sent = true;
        assert_eq!(
            evaluate_scrobble(Some(secs(180)), secs(89), &sig),
            ScrobbleDecision::None
        );
        assert_eq!(
            evaluate_scrobble(Some(secs(180)), secs(90), &sig),
            ScrobbleDecision::Submission
        );
    }

    #[test]
    fn scrobble_submission_caps_at_four_minutes() {
        let mut sig = TrackSignal::new("t".into());
        sig.now_playing_sent = true;
        // A 20-minute track submits at 4:00, not 10:00.
        assert_eq!(
            evaluate_scrobble(Some(secs(1200)), secs(240), &sig),
            ScrobbleDecision::Submission
        );
    }

    #[test]
    fn scrobble_suppressed_after_abandonment() {
        // A skipped load must not now-playing-hint on the lag tick, even
        // though its flags were never set.
        let mut sig = TrackSignal::new("t".into());
        sig.abandoned = true;
        assert_eq!(
            evaluate_scrobble(Some(secs(180)), secs(1), &sig),
            ScrobbleDecision::None
        );
    }

    #[test]
    fn scrobble_each_stage_fires_once() {
        let mut sig = TrackSignal::new("t".into());
        sig.now_playing_sent = true;
        sig.submission_sent = true;
        assert_eq!(
            evaluate_scrobble(Some(secs(180)), secs(170), &sig),
            ScrobbleDecision::None
        );
    }

    // ── evaluate_skip (mirrors skip.test.ts) ────────────────────────────

    #[test]
    fn skip_short_track_not_reported() {
        assert_eq!(evaluate_skip(Some(secs(20)), secs(10)), None);
    }

    #[test]
    fn skip_unknown_duration_not_reported() {
        assert_eq!(evaluate_skip(None, secs(10)), None);
    }

    #[test]
    fn skip_never_started_not_reported() {
        assert_eq!(evaluate_skip(Some(secs(180)), secs(0)), None);
    }

    #[test]
    fn skip_reports_clamped_position_ms() {
        assert_eq!(evaluate_skip(Some(secs(180)), secs(60)), Some(60_000));
        // Position past duration clamps to duration.
        assert_eq!(evaluate_skip(Some(secs(180)), secs(200)), Some(180_000));
    }

    // ── is_disliked (mirrors autoSkip.test.ts) ──────────────────────────

    fn queued(id: &str, album_id: Option<&str>, artist_id: Option<&str>) -> QueuedTrack {
        QueuedTrack {
            id: id.to_owned(),
            title: id.to_owned(),
            artist: None,
            album: None,
            artist_id: artist_id.map(str::to_owned),
            album_id: album_id.map(str::to_owned),
            duration: None,
        }
    }

    #[test]
    fn dislike_matches_track_album_and_artist_levels() {
        let mut ratings = HashMap::new();
        ratings.insert("t-bad".to_owned(), Rating::Dislike);
        ratings.insert("al-bad".to_owned(), Rating::Dislike);
        ratings.insert("ar-bad".to_owned(), Rating::Dislike);
        ratings.insert("t-liked".to_owned(), Rating::Like);

        assert!(is_disliked(&queued("t-bad", None, None), &ratings));
        assert!(is_disliked(&queued("t1", Some("al-bad"), None), &ratings));
        assert!(is_disliked(&queued("t2", None, Some("ar-bad")), &ratings));
        assert!(!is_disliked(&queued("t-liked", Some("al"), Some("ar")), &ratings));
        assert!(!is_disliked(&queued("t3", None, None), &ratings));
    }
}
