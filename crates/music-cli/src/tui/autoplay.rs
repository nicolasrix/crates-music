//! Pure tethered-drift seed weighting — a direct port of the web client's
//! `autoplaySeeds.ts`. Given the current queue, the now-playing cursor, and
//! which items autoplay itself added, it produces the weighted seed list +
//! leash anchor set for a `from-seeds` refill. No I/O, no app state: the
//! reducer gathers the inputs, the effect layer feeds them here, and the
//! result becomes the request body.
//!
//! Weighting (highest first), deduped to the max weight per track:
//! - **anchor** (3.0): the session-anchor track, if any.
//! - **user-picked** (2.0): queue items at/after the cursor the user chose
//!   (not autoplay-added).
//! - **scrobble** (1.0): items before the cursor the user chose.
//! - **frontier** (β·decayᵃᵍᵉ): the last `window` played items, any
//!   provenance — the only place autoplay-added tracks re-enter the pool,
//!   giving the drift its direction of travel.
//!
//! The anchor set (session + user + scrobble seeds, *not* the frontier) is
//! the leash: candidates that stray past τ from all of these are demoted
//! server-side. Provenance is keyed by track id (the terminal client doesn't
//! carry the web's per-item ids), a negligible fidelity loss versus item-id
//! keying.

use std::collections::{HashMap, HashSet};

use crate::config::AutoplayConfig;

/// The travel-frontier parameters (from `[tui.autoplay]`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Frontier {
    pub weight: f32,
    pub decay: f32,
    pub window: usize,
}

impl From<&AutoplayConfig> for Frontier {
    fn from(c: &AutoplayConfig) -> Self {
        Self {
            weight: c.frontier_weight,
            decay: c.frontier_decay,
            window: c.frontier_window,
        }
    }
}

/// The weighted seed list plus the leash anchor set.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WeightedSeeds {
    /// `(track_id, weight)`, highest weight first, ties broken by track id.
    pub seeds: Vec<(String, f32)>,
    /// Leash anchors (session + user + scrobble seeds; no frontier), in
    /// first-seen order.
    pub anchor_ids: Vec<String>,
}

const WEIGHT_ANCHOR: f32 = 3.0;
const WEIGHT_USER_PICKED: f32 = 2.0;
const WEIGHT_SCROBBLE: f32 = 1.0;

/// Build the weighted seeds for a refill. `now_playing_index` must be a valid
/// cursor (the caller only refills when one exists).
pub(crate) fn build_seeds(
    queue_track_ids: &[String],
    now_playing_index: usize,
    recommended: &HashSet<String>,
    anchor_track_id: Option<&str>,
    frontier: Frontier,
) -> WeightedSeeds {
    let mut weights: HashMap<String, f32> = HashMap::new();
    let mut anchor_ids: Vec<String> = Vec::new();
    let mut anchor_seen: HashSet<String> = HashSet::new();

    // Keep the *highest* weight seen for a track id (a track that is both an
    // anchor and a frontier member stays at the anchor weight).
    let mut bump = |id: &str, w: f32| {
        let e = weights.entry(id.to_owned()).or_insert(w);
        if w > *e {
            *e = w;
        }
    };
    let mut add_anchor = |id: &str| {
        if anchor_seen.insert(id.to_owned()) {
            anchor_ids.push(id.to_owned());
        }
    };

    // Anchor: the session's seed track.
    if let Some(a) = anchor_track_id {
        bump(a, WEIGHT_ANCHOR);
        add_anchor(a);
    }

    // User-picked / scrobble: every non-autoplay queue item, weighted by
    // whether it sits before (scrobble) or at/after (user-picked) the cursor.
    for (i, id) in queue_track_ids.iter().enumerate() {
        if recommended.contains(id) {
            continue; // autoplay-added — not a boundary/anchor seed
        }
        let w = if i < now_playing_index {
            WEIGHT_SCROBBLE
        } else {
            WEIGHT_USER_PICKED
        };
        bump(id, w);
        add_anchor(id);
    }

    // Frontier: the last `window` played items (including the current one),
    // decaying with age. Any provenance; never an anchor.
    if frontier.weight > 0.0 && frontier.window > 0 {
        let start = now_playing_index.saturating_sub(frontier.window - 1);
        for i in start..=now_playing_index {
            if let Some(id) = queue_track_ids.get(i) {
                let age = now_playing_index - i;
                let w = frontier.weight * frontier.decay.powi(i32::try_from(age).unwrap_or(0));
                bump(id, w);
            }
        }
    }

    let mut seeds: Vec<(String, f32)> = weights.into_iter().collect();
    // Highest weight first; ties broken by track id for a stable order.
    seeds.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    WeightedSeeds { seeds, anchor_ids }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    fn no_frontier() -> Frontier {
        Frontier {
            weight: 0.0,
            decay: 0.55,
            window: 0,
        }
    }

    fn weight_of(s: &WeightedSeeds, id: &str) -> Option<f32> {
        s.seeds.iter().find(|(k, _)| k == id).map(|(_, w)| *w)
    }

    #[test]
    fn anchor_beats_user_picked_beats_scrobble() {
        // queue: [past0, past1, CUR, next0]; cursor at index 2.
        let q = ids(&["past0", "past1", "cur", "next0"]);
        let recommended = HashSet::new();
        let s = build_seeds(&q, 2, &recommended, Some("anchor"), no_frontier());

        assert_eq!(weight_of(&s, "anchor"), Some(3.0));
        assert_eq!(weight_of(&s, "cur"), Some(2.0)); // at cursor = user-picked
        assert_eq!(weight_of(&s, "next0"), Some(2.0)); // after cursor
        assert_eq!(weight_of(&s, "past0"), Some(1.0)); // before cursor = scrobble
        assert_eq!(weight_of(&s, "past1"), Some(1.0));
        // Sorted highest-first: anchor leads.
        assert_eq!(s.seeds[0].0, "anchor");
    }

    #[test]
    fn autoplay_added_tracks_are_excluded_from_boundary_and_anchors() {
        // next0 was pushed by autoplay — it is neither a seed nor an anchor.
        let q = ids(&["cur", "next0", "next1"]);
        let recommended: HashSet<String> = ["next0".to_owned()].into_iter().collect();
        let s = build_seeds(&q, 0, &recommended, None, no_frontier());

        assert_eq!(weight_of(&s, "next0"), None);
        assert!(!s.anchor_ids.contains(&"next0".to_owned()));
        // cur is a user-picked anchor; next1 too.
        assert!(s.anchor_ids.contains(&"cur".to_owned()));
        assert!(s.anchor_ids.contains(&"next1".to_owned()));
    }

    #[test]
    fn frontier_adds_decaying_weight_but_never_an_anchor() {
        // All-autoplay queue so only the frontier contributes seeds.
        let q = ids(&["a", "b", "c"]);
        let recommended: HashSet<String> =
            ["a".to_owned(), "b".to_owned(), "c".to_owned()].into_iter().collect();
        let frontier = Frontier {
            weight: 0.15,
            decay: 0.5,
            window: 3,
        };
        let s = build_seeds(&q, 2, &recommended, None, frontier);

        // c age0 → 0.15; b age1 → 0.075; a age2 → 0.0375.
        assert!((weight_of(&s, "c").unwrap() - 0.15).abs() < 1e-6);
        assert!((weight_of(&s, "b").unwrap() - 0.075).abs() < 1e-6);
        assert!((weight_of(&s, "a").unwrap() - 0.0375).abs() < 1e-6);
        // Frontier members are not leash anchors.
        assert!(s.anchor_ids.is_empty());
    }

    #[test]
    fn user_weight_wins_over_frontier_for_the_same_track() {
        // cur is both user-picked (2.0) and frontier age0 (0.15) — max wins.
        let q = ids(&["cur"]);
        let recommended = HashSet::new();
        let frontier = Frontier {
            weight: 0.15,
            decay: 0.5,
            window: 3,
        };
        let s = build_seeds(&q, 0, &recommended, None, frontier);
        assert_eq!(weight_of(&s, "cur"), Some(2.0));
    }

    #[test]
    fn empty_when_every_seed_source_is_empty() {
        // Single autoplay-added track, no anchor, no frontier → no seeds
        // (drives the caller's from-any fallback).
        let q = ids(&["only"]);
        let recommended: HashSet<String> = ["only".to_owned()].into_iter().collect();
        let s = build_seeds(&q, 0, &recommended, None, no_frontier());
        assert!(s.seeds.is_empty());
        assert!(s.anchor_ids.is_empty());
    }
}
