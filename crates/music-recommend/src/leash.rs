//! Anchor leash: a growing soft penalty that keeps a "travelling" autoplay
//! station within a similarity radius of a fixed set of *anchor* tracks.
//!
//! ## Why
//!
//! A single-anchor station (one user-picked seed, Σ-similarity over its ANN
//! neighbours) hits a hard ceiling: once the seed's ~`per_seed_n` neighbours
//! are exhausted, refill starves. Letting the station re-seed from its own
//! recent output lets it travel further — but naïvely, it drifts off into
//! unrelated genres because nothing constrains how far each step strays from
//! the user's original intent.
//!
//! The leash separates the two concerns:
//!
//! - **Direction** comes from the Σ-similarity aggregation over the seed pool
//!   (anchors + recency-weighted recent tail) — handled upstream in
//!   [`crate::aggregate`].
//! - **Boundary** comes from this module: a candidate that falls past `tau`
//!   cosine of its *nearest* anchor is demoted by
//!   `lambda · max(0, tau − nearest_sim)²`. Zero inside the radius, growing
//!   quadratically outside it — a soft wall, not a hard cut, so the station
//!   can still move when nothing closer exists.
//!
//! ## Geometry
//!
//! Similarities are cosines in the **whitened** embedding space — the same
//! space the ANN stores vectors in and the same space the Σ-similarity scores
//! live in (the gateway reads candidate + anchor vectors via
//! `AnnIndex::get_vector`, which returns the stored whitened vector). Mixing
//! raw and whitened cosines here would make the penalty incomparable to the
//! score it adjusts, so callers must pass whitened vectors for both.

use music_core::TrackId;

/// Default leash radius `tau` (cosine). Derived from the offline stay-close
/// parameter sweep on the live 768-dim whitened CLaMP 3 corpus: at `tau≈0.28`
/// a travelling station holds end-similarity to the anchor ~0.4 while still
/// reaching 60+ distinct tracks (the single-anchor ceiling was ~20).
pub const DEFAULT_LEASH_TAU: f32 = 0.28;

/// Default leash strength `lambda`. Same sweep — strong enough to keep the
/// frontier from wandering into unrelated genres without freezing travel.
pub const DEFAULT_LEASH_LAMBDA: f32 = 16.0;

/// Leash tuning. `tau` is the cosine radius inside which no penalty applies;
/// `lambda` scales how hard candidates are pushed back once they cross it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LeashParams {
    /// Cosine radius. A candidate whose nearest-anchor similarity is `>= tau`
    /// pays nothing. Typical stay-close value ~0.28 in 768-dim whitened space.
    pub tau: f32,
    /// Penalty scale. Higher = tighter leash. Typical ~16.
    pub lambda: f32,
}

impl LeashParams {
    /// Whether the leash does anything. A non-positive `lambda` (or a `tau`
    /// so low nothing can fall below it) makes [`Self::penalty`] a no-op, so
    /// the gateway can skip the per-candidate vector fetch entirely.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.lambda > 0.0 && self.tau > -1.0
    }

    /// The leash penalty (always `>= 0`) for a candidate whose nearest-anchor
    /// cosine is `nearest_sim`. Quadratic in the shortfall past `tau`.
    #[must_use]
    pub fn penalty(&self, nearest_sim: f32) -> f32 {
        if self.lambda <= 0.0 {
            return 0.0;
        }
        let gap = (self.tau - nearest_sim).max(0.0);
        self.lambda * gap * gap
    }
}

/// Cosine of `candidate` against its *nearest* anchor.
///
/// - Anchors with a vector of the wrong length (or empty) are skipped — they
///   contribute no constraint rather than a spurious 0.0 that would penalize
///   every candidate.
/// - With **no** usable anchors, returns `None`: the caller should treat the
///   leash as inactive for this candidate (fail-open), not as "infinitely
///   far" (which would penalize everything uniformly and just rescale scores).
#[must_use]
pub fn nearest_anchor_sim(candidate: &[f32], anchors: &[Vec<f32>]) -> Option<f32> {
    let mut best: Option<f32> = None;
    for anchor in anchors {
        if anchor.len() != candidate.len() || anchor.is_empty() {
            continue;
        }
        let sim = crate::mmr::cosine_similarity(candidate, anchor);
        best = Some(best.map_or(sim, |b: f32| b.max(sim)));
    }
    best
}

/// One candidate's leash outcome, returned in input order so the caller can
/// zip it back onto the aggregated results without a second lookup.
#[derive(Clone, Debug, PartialEq)]
pub struct LeashAdjustment {
    pub track_id: TrackId,
    /// Cosine to the nearest anchor, or `None` when the candidate had no
    /// vector or no anchor was usable (penalty was treated as 0).
    pub nearest_sim: Option<f32>,
    /// The penalty subtracted from this candidate's relevance score (`>= 0`).
    pub penalty: f32,
}

/// Aggregate stats over a leash pass — folded into the request span for
/// `/diagnostics` so a tightening/loosening leash is observable without a
/// rebuild.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LeashStats {
    /// Candidates that paid a non-zero penalty (fell past `tau`).
    pub demoted: usize,
    /// Candidates measured against at least one anchor (had a vector).
    pub measured: usize,
    /// Min / max nearest-anchor similarity across measured candidates.
    pub min_sim: f32,
    pub max_sim: f32,
}

/// Candidate handed to [`apply`]: just its id + whitened `vector`
/// (`None` ⇒ unmeasurable ⇒ no penalty, fail-open). The caller keeps the
/// relevance score itself and subtracts the returned [`LeashAdjustment::penalty`].
#[derive(Debug)]
pub struct LeashCandidate<'a> {
    pub track_id: &'a TrackId,
    pub vector: Option<&'a [f32]>,
}

/// Compute leash penalties for `candidates` against `anchors`, returning the
/// per-candidate adjustments (input order) and aggregate stats. Pure: the
/// caller applies the penalties to its own scored collection and re-sorts.
///
/// `anchors` are the whitened anchor vectors (already fetched + filtered to
/// the resolvable ones by the caller). An empty `anchors` slice makes every
/// penalty 0 — the leash is inert, which is the correct degrade when no
/// anchor is embedded yet.
#[must_use]
pub fn apply(
    candidates: &[LeashCandidate<'_>],
    anchors: &[Vec<f32>],
    params: LeashParams,
) -> (Vec<LeashAdjustment>, LeashStats) {
    let mut out = Vec::with_capacity(candidates.len());
    let mut stats = LeashStats::default();
    let mut min_sim = f32::INFINITY;
    let mut max_sim = f32::NEG_INFINITY;

    for c in candidates {
        let nearest = c.vector.and_then(|v| nearest_anchor_sim(v, anchors));
        let penalty = match nearest {
            Some(sim) => {
                stats.measured += 1;
                min_sim = min_sim.min(sim);
                max_sim = max_sim.max(sim);
                let p = params.penalty(sim);
                if p > 0.0 {
                    stats.demoted += 1;
                }
                p
            }
            // No vector or no usable anchor: fail-open (no penalty).
            None => 0.0,
        };
        out.push(LeashAdjustment {
            track_id: c.track_id.clone(),
            nearest_sim: nearest,
            penalty,
        });
    }

    if stats.measured > 0 {
        stats.min_sim = min_sim;
        stats.max_sim = max_sim;
    }
    (out, stats)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn p(tau: f32, lambda: f32) -> LeashParams {
        LeashParams { tau, lambda }
    }

    #[test]
    fn no_penalty_inside_radius() {
        // sim >= tau ⇒ zero penalty.
        assert_eq!(p(0.3, 16.0).penalty(0.3), 0.0);
        assert_eq!(p(0.3, 16.0).penalty(0.9), 0.0);
    }

    #[test]
    fn quadratic_penalty_outside_radius() {
        // gap = 0.3 - 0.1 = 0.2 ⇒ 16 * 0.2^2 = 0.64.
        let got = p(0.3, 16.0).penalty(0.1);
        assert!((got - 0.64).abs() < 1e-6, "got {got}");
        // Twice the gap ⇒ four times the penalty (quadratic).
        let near = p(0.3, 16.0).penalty(0.2); // gap 0.1 ⇒ 0.16
        assert!((got / near - 4.0).abs() < 1e-4);
    }

    #[test]
    fn lambda_zero_is_inert() {
        assert_eq!(p(0.9, 0.0).penalty(-1.0), 0.0);
        assert!(!p(0.9, 0.0).is_active());
        assert!(p(0.28, 16.0).is_active());
    }

    #[test]
    fn nearest_anchor_picks_closest() {
        let cand = vec![1.0, 0.0, 0.0];
        let anchors = vec![
            vec![0.0, 1.0, 0.0], // cos 0
            vec![1.0, 1.0, 0.0], // cos ~0.707
        ];
        let got = nearest_anchor_sim(&cand, &anchors).unwrap();
        assert!((got - 0.707).abs() < 1e-3, "got {got}");
    }

    #[test]
    fn nearest_anchor_skips_wrong_dim_and_empties() {
        let cand = vec![1.0, 0.0, 0.0];
        // Only the matching-dim anchor counts; the 2-dim one is skipped.
        let anchors = vec![vec![0.5, 0.5], vec![1.0, 0.0, 0.0]];
        let got = nearest_anchor_sim(&cand, &anchors).unwrap();
        assert!((got - 1.0).abs() < 1e-6, "got {got}");
        // No usable anchors ⇒ None (fail-open), not a bogus 0.0.
        assert_eq!(nearest_anchor_sim(&cand, &[vec![0.5, 0.5]]), None);
        assert_eq!(nearest_anchor_sim(&cand, &[]), None);
    }

    #[test]
    fn apply_fails_open_on_missing_vector_or_anchor() {
        let id_a = TrackId::from("a");
        let id_b = TrackId::from("b");
        let vec_a = vec![1.0, 0.0, 0.0];
        let candidates = vec![
            LeashCandidate {
                track_id: &id_a,
                vector: Some(&vec_a),
            },
            // No vector ⇒ no penalty regardless of how far it "is".
            LeashCandidate {
                track_id: &id_b,
                vector: None,
            },
        ];
        // Far anchor (cos 0 < tau 0.5) ⇒ a is penalized, b is not.
        let anchors = vec![vec![0.0, 1.0, 0.0]];
        let (adj, stats) = apply(&candidates, &anchors, p(0.5, 10.0));
        assert!(adj[0].penalty > 0.0);
        assert_eq!(adj[1].penalty, 0.0);
        assert_eq!(adj[1].nearest_sim, None);
        assert_eq!(stats.measured, 1);
        assert_eq!(stats.demoted, 1);
    }

    #[test]
    fn apply_empty_anchors_is_inert() {
        let id_a = TrackId::from("a");
        let vec_a = vec![1.0, 0.0, 0.0];
        let candidates = vec![LeashCandidate {
            track_id: &id_a,
            vector: Some(&vec_a),
        }];
        let (adj, stats) = apply(&candidates, &[], p(0.5, 10.0));
        assert_eq!(adj[0].penalty, 0.0);
        assert_eq!(adj[0].nearest_sim, None);
        assert_eq!(stats.measured, 0);
        assert_eq!(stats.demoted, 0);
    }
}
