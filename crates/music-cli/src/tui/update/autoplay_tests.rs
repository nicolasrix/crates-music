//! Autoplay reducer tests — refill triggering (threshold / cooldown /
//! in-flight / toggle-off generation guard), provenance recording, and the
//! feedback-thumb gate. All drive the offline/local queue so no sync echo is
//! needed; the online path shares the same `maybe_refill` / `on_refilled`
//! logic (only the push target differs).

use music_core::{Track, TrackId};

use super::super::msg::{Effect, Msg, StationError};
use super::super::state::{App, FeedbackVote, to_queued};
use super::{autoplay, update};

/// gateway = true → sync starts Offline: `maybe_refill` runs (it only needs
/// the HTTP recommender, not the WS), pushing to the local queue.
fn app() -> App {
    App::new(None, false, true)
}

fn track(id: &str) -> Track {
    Track {
        id: TrackId::from(id.to_owned()),
        title: id.to_owned(),
        album_id: None,
        album_name: None,
        artist_id: None,
        artist_name: None,
        track_number: None,
        disc_number: None,
        duration_seconds: Some(180),
        bit_rate_kbps: None,
        content_type: None,
        suffix: None,
        year: None,
        genre: None,
        play_count: None,
        played_at: None,
    }
}

/// Populate the local queue with `ids`, cursor at `start`.
fn load_queue(a: &mut App, ids: &[&str], start: usize) {
    let queued: Vec<_> = ids.iter().map(|id| to_queued(&track(id))).collect();
    a.queue.replace(queued, start);
}

fn find_refill(effects: &[Effect]) -> Option<&Effect> {
    effects
        .iter()
        .find(|e| matches!(e, Effect::AutoplayRefill { .. }))
}

// ── refill triggering ────────────────────────────────────────────────────

#[test]
fn refill_fires_when_upcoming_below_threshold() {
    let mut a = app();
    a.autoplay.enabled = true;
    load_queue(&mut a, &["a", "b", "c"], 0); // upcoming = 2 < 5

    let effects = autoplay::maybe_refill(&mut a);
    let refill = find_refill(&effects).expect("refill fires");
    let Effect::AutoplayRefill {
        need,
        now_playing_index,
        queue_track_ids,
        ..
    } = refill
    else {
        unreachable!()
    };
    assert_eq!(*need, 3); // 5 - 2 upcoming
    assert_eq!(*now_playing_index, 0);
    assert_eq!(queue_track_ids.len(), 3);
    assert!(a.autoplay.inflight, "the in-flight lock is set");
}

#[test]
fn no_refill_when_upcoming_at_threshold() {
    let mut a = app();
    a.autoplay.enabled = true;
    load_queue(&mut a, &["a", "b", "c", "d", "e", "f", "g"], 0); // upcoming = 6 ≥ 5
    assert!(find_refill(&autoplay::maybe_refill(&mut a)).is_none());
    assert!(!a.autoplay.inflight);
}

#[test]
fn no_refill_when_disabled() {
    let mut a = app();
    a.autoplay.enabled = false;
    load_queue(&mut a, &["a", "b"], 0);
    assert!(find_refill(&autoplay::maybe_refill(&mut a)).is_none());
}

#[test]
fn no_refill_while_a_refill_is_in_flight() {
    let mut a = app();
    a.autoplay.enabled = true;
    a.autoplay.inflight = true;
    load_queue(&mut a, &["a", "b"], 0);
    assert!(find_refill(&autoplay::maybe_refill(&mut a)).is_none());
}

#[test]
fn cooldown_blocks_then_releases() {
    let mut a = app();
    a.autoplay.enabled = true;
    a.tick = 10;
    a.autoplay.cooldown_until = 20;
    load_queue(&mut a, &["a", "b"], 0);
    // Still cooling down.
    assert!(find_refill(&autoplay::maybe_refill(&mut a)).is_none());
    // Past the cooldown → fires.
    a.tick = 20;
    assert!(find_refill(&autoplay::maybe_refill(&mut a)).is_some());
}

#[test]
fn direct_mode_never_refills() {
    // gateway = false → SyncPhase::Disabled: no recommender to call.
    let mut a = App::new(None, false, false);
    a.autoplay.enabled = true;
    load_queue(&mut a, &["a", "b"], 0);
    assert!(find_refill(&autoplay::maybe_refill(&mut a)).is_none());
}

#[test]
fn no_refill_without_a_cursor() {
    let mut a = app();
    a.autoplay.enabled = true;
    // Empty queue → no current index.
    assert!(find_refill(&autoplay::maybe_refill(&mut a)).is_none());
}

// ── completion: push, provenance, cooldown ───────────────────────────────

#[test]
fn on_refilled_pushes_records_provenance_and_short_cooldown() {
    let mut a = app();
    a.autoplay.enabled = true;
    a.tick = 100;
    load_queue(&mut a, &["a"], 0);
    a.autoplay.inflight = true;
    let gen0 = a.autoplay.generation;

    let tracks = vec![track("r1"), track("r2"), track("r3")];
    autoplay::on_refilled(&mut a, gen0, 3, Ok(tracks));

    // Full delivery (added == need) → short cooldown, lock released.
    assert!(!a.autoplay.inflight);
    assert_eq!(a.autoplay.cooldown_until, 100 + 6);
    // Provenance recorded for all three.
    for id in ["r1", "r2", "r3"] {
        assert!(a.autoplay.recommended.contains(id));
    }
    // Pushed onto the local queue.
    assert_eq!(a.queue.len(), 4);
}

#[test]
fn under_delivery_arms_the_long_cooldown() {
    let mut a = app();
    a.autoplay.enabled = true;
    a.tick = 100;
    load_queue(&mut a, &["a"], 0);
    a.autoplay.inflight = true;
    let gen0 = a.autoplay.generation;

    // Asked for 3, got 1.
    autoplay::on_refilled(&mut a, gen0, 3, Ok(vec![track("r1")]));
    assert_eq!(a.autoplay.cooldown_until, 100 + 120);
}

#[test]
fn empty_and_unavailable_results_arm_the_long_cooldown() {
    for result in [
        Ok(Vec::new()),
        Err(StationError::Unavailable),
    ] {
        let mut a = app();
        a.autoplay.enabled = true;
        a.tick = 5;
        load_queue(&mut a, &["a"], 0);
        a.autoplay.inflight = true;
        let gen0 = a.autoplay.generation;
        autoplay::on_refilled(&mut a, gen0, 4, result);
        assert_eq!(a.autoplay.cooldown_until, 5 + 120);
        assert_eq!(a.queue.len(), 1, "nothing pushed");
    }
}

#[test]
fn toggle_off_mid_flight_discards_the_result() {
    let mut a = app();
    a.autoplay.enabled = true;
    load_queue(&mut a, &["a", "b"], 0);

    // A refill goes out (generation G, lock held).
    autoplay::maybe_refill(&mut a);
    let stale_gen = a.autoplay.generation;
    assert!(a.autoplay.inflight);

    // User toggles autoplay off before it lands → generation bumps, lock clears.
    autoplay::toggle(&mut a);
    assert!(!a.autoplay.enabled);
    assert_ne!(a.autoplay.generation, stale_gen);

    // The in-flight result arrives with the stale generation → dropped.
    autoplay::on_refilled(&mut a, stale_gen, 3, Ok(vec![track("r1")]));
    assert!(a.autoplay.recommended.is_empty(), "no provenance recorded");
    assert_eq!(a.queue.len(), 2, "nothing pushed");
}

#[test]
fn toggle_on_kicks_an_immediate_refill() {
    let mut a = app();
    load_queue(&mut a, &["a", "b"], 0); // short queue
    let effects = autoplay::toggle(&mut a); // turn on
    assert!(a.autoplay.enabled);
    assert!(find_refill(&effects).is_some(), "toggling on refills at once");
}

// ── feedback thumbs ──────────────────────────────────────────────────────

#[test]
fn feedback_is_a_no_op_on_a_user_picked_track() {
    let mut a = app();
    load_queue(&mut a, &["a", "b"], 0); // "a" was NOT autoplay-added
    let effects = autoplay::feedback(&mut a, FeedbackVote::Up);
    assert!(effects.is_empty());
    assert!(a.autoplay.votes.is_empty());
}

#[test]
fn feedback_on_a_pick_votes_optimistically_and_submits() {
    let mut a = app();
    load_queue(&mut a, &["a", "b"], 0);
    a.autoplay.recommended.insert("a".to_owned());

    let effects = autoplay::feedback(&mut a, FeedbackVote::Up);
    assert_eq!(a.autoplay.votes.get("a"), Some(&FeedbackVote::Up));
    let submit = effects
        .iter()
        .find(|e| matches!(e, Effect::SubmitFeedback { .. }))
        .expect("submits");
    let Effect::SubmitFeedback { track_id, vote, .. } = submit else {
        unreachable!()
    };
    assert_eq!(track_id, "a");
    assert_eq!(*vote, Some(FeedbackVote::Up));
}

#[test]
fn pressing_the_active_thumb_again_clears_the_vote() {
    let mut a = app();
    load_queue(&mut a, &["a"], 0);
    a.autoplay.recommended.insert("a".to_owned());
    a.autoplay.votes.insert("a".to_owned(), FeedbackVote::Up);

    let effects = autoplay::feedback(&mut a, FeedbackVote::Up); // same thumb
    assert!(!a.autoplay.votes.contains_key("a"), "vote cleared");
    let Effect::SubmitFeedback { vote, previous, .. } = effects
        .iter()
        .find(|e| matches!(e, Effect::SubmitFeedback { .. }))
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(*vote, None); // clear
    assert_eq!(*previous, Some(FeedbackVote::Up));
}

#[test]
fn feedback_failure_rolls_the_vote_back() {
    let mut a = app();
    load_queue(&mut a, &["a"], 0);
    a.autoplay.recommended.insert("a".to_owned());
    // Optimistically voted up; the previous state was neutral (None).
    a.autoplay.votes.insert("a".to_owned(), FeedbackVote::Up);

    autoplay::on_feedback_done(&mut a, "a".to_owned(), None, Err("boom".to_owned()));
    assert!(!a.autoplay.votes.contains_key("a"), "rolled back to neutral");
}

// ── dispatch smoke test ──────────────────────────────────────────────────

#[test]
fn toggle_autoplay_message_flips_the_flag() {
    let mut a = app();
    assert!(!a.autoplay.enabled);
    update(&mut a, Msg::ToggleAutoplay);
    assert!(a.autoplay.enabled);
    update(&mut a, Msg::ToggleAutoplay);
    assert!(!a.autoplay.enabled);
}
