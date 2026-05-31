//! Rules-based user-preference affinity (pure math).
//!
//! The recommender is **seed-driven**: every station or `/next` request
//! starts from a track (or text) the user actively chose, so the
//! acoustic-similarity term already captures "what *kind* of song do I
//! want right now". This module answers the orthogonal question — "does
//! this listener actually like *this specific* song?" — from that
//! track's own interaction history, and nothing else. There is no global
//! taste centroid; a candidate the user has never engaged with carries
//! zero affinity and rides on pure acoustic relevance (the discovery
//! case).
//!
//! ## The decayed counter
//!
//! Taste drifts: a skip from a year ago should not count like one from
//! yesterday. We avoid a nightly batch recompute by storing, per track,
//! a single exponentially-**decayed counter** `(score, updated_ms)` and
//! decaying it to "now" only when it's read:
//!
//! ```text
//! decay(s, Δt) = s · exp(-ln2 · Δt / half_life)
//! ```
//!
//! Exponential decay is *composable* — decaying to time `t₁` then to
//! `t₂` equals decaying straight to `t₂` — so folding a new event is
//! O(1): decay the stored score to the event's time, add the event
//! weight, done. Reading is one more decay to the current clock. No
//! per-event history, no scheduled job. See [`fold_event`] and
//! [`affinity_at`].
//!
//! ## Weights
//!
//! Implicit + explicit signal collapses to a scalar weight per event
//! ([`event_weight`]). Explicit thumbs dominate; a completed play is a
//! mild positive; a skip is negative, scaled by *how early* it was
//! abandoned (a skip at 95 % is barely a skip). All constants are v1
//! rules-of-thumb, deliberately conservative, and live here so the one
//! place to retune is obvious.
//!
//! ## Bounding
//!
//! The raw decayed score is unbounded (a track played daily for years
//! would dominate). [`affinity_at`] squashes it through `tanh` into
//! `(-1, 1)` so a handful of strong signals saturates rather than
//! runs away, and the downstream MMR bonus (`weight · affinity`) stays
//! predictable regardless of corpus age.

/// Explicit upvote ("I like this recommendation"). Strong positive.
pub const LIKE_WEIGHT: f32 = 1.0;
/// Explicit downvote. Strong negative — mirror of [`LIKE_WEIGHT`].
pub const DISLIKE_WEIGHT: f32 = -1.0;
/// A completed play (a submitted scrobble). Mild positive: the user let
/// it run, but didn't reach for the thumb. Scaled by completion so a
/// barely-started "play" can't masquerade as endorsement.
pub const PLAY_WEIGHT: f32 = 0.25;
/// A skip, at completion 0. Negative; scaled by `(1 - completion)` so an
/// early bail is a clear "no" while a near-end skip is almost neutral.
pub const SKIP_WEIGHT: f32 = -0.5;

/// Squash scale for [`affinity_at`]. Larger = gentler ramp (one event
/// moves the needle less). At `2.0`, a single like → `tanh(0.5) ≈ 0.46`;
/// two likes plus a play saturate toward `~0.8` without ever hitting 1.
pub const AFFINITY_SCALE: f32 = 2.0;

/// One interaction, reduced to the signal the affinity model cares
/// about. The caller decides which variant to emit (e.g. a Subsonic
/// submission scrobble → [`Self::Play`]; a `/v1/events` skip carrying a
/// `played_ms` → [`Self::Skip`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AffinityEvent {
    /// Explicit thumbs-up.
    Like,
    /// Explicit thumbs-down.
    Dislike,
    /// A play that counted. `completion` ∈ `[0, 1]` is the fraction of
    /// the track heard (1.0 when unknown — a submitted scrobble means
    /// the listen counted).
    Play { completion: f32 },
    /// The user skipped. `completion` ∈ `[0, 1]` is where they bailed.
    Skip { completion: f32 },
}

/// Map an event to its scalar weight. Completion is clamped to `[0, 1]`
/// defensively — a client reporting `played_ms > duration` (clock skew,
/// looping) must not invert the sign.
#[must_use]
pub fn event_weight(event: AffinityEvent) -> f32 {
    match event {
        AffinityEvent::Like => LIKE_WEIGHT,
        AffinityEvent::Dislike => DISLIKE_WEIGHT,
        AffinityEvent::Play { completion } => PLAY_WEIGHT * completion.clamp(0.0, 1.0),
        AffinityEvent::Skip { completion } => SKIP_WEIGHT * (1.0 - completion.clamp(0.0, 1.0)),
    }
}

/// Exponentially decay `score` over `dt_ms` milliseconds. A non-positive
/// `dt_ms` (equal or out-of-order timestamps) is a no-op — decay only
/// ever moves a value *toward* zero, never away. A non-positive
/// `half_life_ms` disables decay (returns `score` unchanged) rather than
/// dividing by zero.
#[must_use]
#[allow(
    clippy::cast_precision_loss, // ms timestamps; f64 mantissa covers ~285k years of ms
    clippy::cast_possible_truncation // decay only shrinks |score|, stays in f32 range
)]
pub fn decay(score: f32, dt_ms: i64, half_life_ms: i64) -> f32 {
    if dt_ms <= 0 || half_life_ms <= 0 {
        return score;
    }
    // λ = ln2 / half_life. exp(-λ·Δt) in f64 for headroom, back to f32.
    let lambda = std::f64::consts::LN_2 / half_life_ms as f64;
    let factor = (-lambda * dt_ms as f64).exp();
    (f64::from(score) * factor) as f32
}

/// Fold a new event of `weight` (occurring at `event_ms`) into the
/// decayed counter `(prev_score, prev_updated_ms)`, returning the new
/// `(score, updated_ms)`.
///
/// Order-tolerant and symmetric: both the stored score and the incoming
/// weight are decayed to a common anchor (the later of the two
/// timestamps), so an out-of-order replay from an offline batch
/// contributes a correctly-aged weight without dragging the clock
/// backwards. With in-order events (`event_ms >= prev_updated_ms`) the
/// weight is undecayed and the stored score is aged forward — the common
/// case.
#[must_use]
pub fn fold_event(
    prev_score: f32,
    prev_updated_ms: i64,
    event_ms: i64,
    weight: f32,
    half_life_ms: i64,
) -> (f32, i64) {
    let anchor = prev_updated_ms.max(event_ms);
    let score_at_anchor = decay(prev_score, anchor - prev_updated_ms, half_life_ms);
    let weight_at_anchor = decay(weight, anchor - event_ms, half_life_ms);
    (score_at_anchor + weight_at_anchor, anchor)
}

/// Read the bounded affinity for a stored counter, decayed to `now_ms`
/// and squashed through `tanh` into `(-1, 1)`. `None`-equivalent (a
/// track with no row) is the caller's job; this is the "we have a row"
/// path.
#[must_use]
pub fn affinity_at(score: f32, updated_ms: i64, now_ms: i64, half_life_ms: i64) -> f32 {
    let decayed = decay(score, now_ms - updated_ms, half_life_ms);
    (decayed / AFFINITY_SCALE).tanh()
}

/// The additive relevance adjustment fed into the MMR score for a
/// candidate: `weight · affinity`, clamped to `±weight` (the `tanh`
/// bound already guarantees this, but a negative `weight` from a
/// misconfigured client is clamped to 0 so the signal can never invert).
#[must_use]
pub fn preference_bonus(affinity: f32, weight: f32) -> f32 {
    weight.max(0.0) * affinity.clamp(-1.0, 1.0)
}

/// Convert a half-life expressed in days to milliseconds, saturating.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation // bounds-checked against i64::MAX above
)]
pub fn half_life_days_to_ms(days: f32) -> i64 {
    let ms = f64::from(days) * 24.0 * 60.0 * 60.0 * 1000.0;
    if ms <= 0.0 {
        0
    } else if ms >= i64::MAX as f64 {
        i64::MAX
    } else {
        ms as i64
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    // --- event_weight ---

    #[test]
    fn like_and_dislike_are_mirror_strong_signals() {
        assert_eq!(event_weight(AffinityEvent::Like), LIKE_WEIGHT);
        assert_eq!(event_weight(AffinityEvent::Dislike), DISLIKE_WEIGHT);
        assert_eq!(event_weight(AffinityEvent::Like), -event_weight(AffinityEvent::Dislike));
    }

    #[test]
    fn full_play_is_mild_positive() {
        assert_eq!(event_weight(AffinityEvent::Play { completion: 1.0 }), PLAY_WEIGHT);
    }

    #[test]
    fn partial_play_scales_down() {
        let w = event_weight(AffinityEvent::Play { completion: 0.5 });
        assert!((w - PLAY_WEIGHT * 0.5).abs() < 1e-6);
    }

    #[test]
    fn early_skip_is_strong_negative() {
        // Bailed immediately → full skip penalty.
        assert_eq!(event_weight(AffinityEvent::Skip { completion: 0.0 }), SKIP_WEIGHT);
    }

    #[test]
    fn late_skip_is_almost_neutral() {
        // Skipped the last 5 % → barely a skip.
        let w = event_weight(AffinityEvent::Skip { completion: 0.95 });
        assert!(w < 0.0 && w.abs() < 0.05, "got {w}");
    }

    #[test]
    fn completion_is_clamped_so_sign_cannot_invert() {
        // A client over-reporting completion must not turn a skip
        // positive or a play negative.
        assert!(event_weight(AffinityEvent::Skip { completion: 2.0 }) <= 0.0);
        assert!(event_weight(AffinityEvent::Play { completion: -1.0 }) >= 0.0);
    }

    // --- decay ---

    #[test]
    fn decay_is_identity_at_zero_dt() {
        assert_eq!(decay(1.0, 0, DAY_MS), 1.0);
    }

    #[test]
    fn decay_halves_at_one_half_life() {
        let out = decay(1.0, 30 * DAY_MS, 30 * DAY_MS);
        assert!((out - 0.5).abs() < 1e-5, "got {out}");
    }

    #[test]
    fn decay_quarters_at_two_half_lives() {
        let out = decay(1.0, 60 * DAY_MS, 30 * DAY_MS);
        assert!((out - 0.25).abs() < 1e-5, "got {out}");
    }

    #[test]
    fn negative_dt_is_a_noop() {
        // Out-of-order read: never amplify.
        assert_eq!(decay(0.7, -DAY_MS, DAY_MS), 0.7);
    }

    #[test]
    fn nonpositive_half_life_disables_decay() {
        assert_eq!(decay(0.7, 5 * DAY_MS, 0), 0.7);
        assert_eq!(decay(0.7, 5 * DAY_MS, -1), 0.7);
    }

    #[test]
    fn decay_preserves_sign() {
        assert!(decay(-1.0, 10 * DAY_MS, 30 * DAY_MS) < 0.0);
    }

    // --- fold_event ---

    #[test]
    fn fold_into_empty_counter_is_just_the_weight() {
        let (score, updated) = fold_event(0.0, 0, 1_000, LIKE_WEIGHT, 30 * DAY_MS);
        assert_eq!(score, LIKE_WEIGHT);
        assert_eq!(updated, 1_000);
    }

    #[test]
    fn fold_in_order_ages_prior_score_then_adds_weight() {
        // Prior like at t=0; another like one half-life later.
        // Prior decays to 0.5, plus the fresh 1.0 → 1.5.
        let hl = 30 * DAY_MS;
        let (score, updated) = fold_event(1.0, 0, hl, LIKE_WEIGHT, hl);
        assert!((score - 1.5).abs() < 1e-4, "got {score}");
        assert_eq!(updated, hl);
    }

    #[test]
    fn fold_out_of_order_keeps_clock_and_ages_the_late_weight() {
        // Stored score anchored at t=2·hl. A replayed event from t=hl
        // arrives late: its weight is aged forward by one half-life
        // (→0.5) and the clock stays at 2·hl.
        let hl = 30 * DAY_MS;
        let (score, updated) = fold_event(1.0, 2 * hl, hl, LIKE_WEIGHT, hl);
        assert_eq!(updated, 2 * hl, "clock must not move backwards");
        assert!((score - 1.5).abs() < 1e-4, "got {score}");
    }

    #[test]
    fn fold_is_associative_across_two_in_order_events() {
        // Folding A then B should match decaying-and-summing by hand.
        let hl = 30 * DAY_MS;
        let (s1, u1) = fold_event(0.0, 0, 0, LIKE_WEIGHT, hl); // like at t=0
        let (s2, _u2) = fold_event(s1, u1, hl, PLAY_WEIGHT, hl); // play at t=hl
        // Expected: like decays to 0.5 by t=hl, plus play weight.
        assert!((s2 - (0.5 + PLAY_WEIGHT)).abs() < 1e-4, "got {s2}");
    }

    // --- affinity_at ---

    #[test]
    fn affinity_of_zero_score_is_zero() {
        assert_eq!(affinity_at(0.0, 0, 10_000, 30 * DAY_MS), 0.0);
    }

    #[test]
    fn affinity_is_bounded_in_open_unit_interval() {
        // A strongly positive score saturates near 1 without reaching it
        // (for realistic magnitudes; tanh of a huge input rounds to 1.0
        // in f32, which is still within bounds — never exceeds it).
        let a = affinity_at(10.0, 0, 0, 30 * DAY_MS);
        assert!(a > 0.99 && a < 1.0, "got {a}");
        let b = affinity_at(-10.0, 0, 0, 30 * DAY_MS);
        assert!(b < -0.99 && b > -1.0, "got {b}");
        // And the absolute bound holds even for extreme scores.
        assert!(affinity_at(1e6, 0, 0, 30 * DAY_MS) <= 1.0);
        assert!(affinity_at(-1e6, 0, 0, 30 * DAY_MS) >= -1.0);
    }

    #[test]
    fn positive_score_gives_positive_affinity_negative_gives_negative() {
        assert!(affinity_at(1.0, 0, 0, 30 * DAY_MS) > 0.0);
        assert!(affinity_at(-1.0, 0, 0, 30 * DAY_MS) < 0.0);
    }

    #[test]
    fn affinity_decays_toward_zero_over_time() {
        let hl = 30 * DAY_MS;
        let fresh = affinity_at(1.0, 0, 0, hl);
        let aged = affinity_at(1.0, 0, 4 * hl, hl); // 4 half-lives later
        assert!(aged.abs() < fresh.abs(), "fresh {fresh}, aged {aged}");
        assert!(aged > 0.0 && aged < 0.1, "got {aged}");
    }

    // --- preference_bonus ---

    #[test]
    fn bonus_scales_affinity_by_weight() {
        assert!((preference_bonus(0.5, 0.2) - 0.1).abs() < 1e-6);
        assert!((preference_bonus(-1.0, 0.15) + 0.15).abs() < 1e-6);
    }

    #[test]
    fn bonus_clamps_negative_weight_to_zero() {
        // A misconfigured negative weight must not flip the signal into
        // "prefer disliked tracks".
        assert_eq!(preference_bonus(0.8, -0.5), 0.0);
    }

    #[test]
    fn bonus_clamps_out_of_range_affinity() {
        assert!((preference_bonus(5.0, 0.2) - 0.2).abs() < 1e-6);
    }

    // --- half_life_days_to_ms ---

    #[test]
    fn half_life_days_round_trips() {
        assert_eq!(half_life_days_to_ms(30.0), 30 * DAY_MS);
        assert_eq!(half_life_days_to_ms(1.0), DAY_MS);
    }

    #[test]
    fn nonpositive_half_life_days_is_zero() {
        assert_eq!(half_life_days_to_ms(0.0), 0);
        assert_eq!(half_life_days_to_ms(-5.0), 0);
    }
}
