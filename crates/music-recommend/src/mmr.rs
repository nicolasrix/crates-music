//! Maximal Marginal Relevance (MMR) re-ranker.
//!
//! Given a list of candidates with `(sim_to_seed, embedding_vector,
//! artist_key)`, greedily picks `top_n` of them maximising
//!
//! ```text
//! relevance(c)        = sim(c, seed) + relevance_bonus(c)
//! score(c | admitted) = λ * relevance(c)
//!                     - (1 - λ) * max_{a ∈ admitted} sim(c, a)
//!                     - μ * artist_count(c.artist)
//! ```
//!
//! `relevance_bonus` is an additive, caller-supplied adjustment to the
//! relevance term — today it carries the user-preference signal
//! (`preference_weight · affinity(c)`, see [`crate::preference`]): a
//! candidate the listener has liked / replayed is nudged up, one they've
//! disliked / skipped is nudged down. It defaults to `0.0` (no
//! adjustment), so the formula degrades to plain MMR for callers that
//! don't compute it. The bonus only touches relevance — the diversity
//! term still uses raw `sim(c, a)` over the embedding vectors, so
//! preference reorders near-ties without distorting the novelty geometry.
//!
//! - `λ = 1.0`: pure similarity (preserves the ANN's relevance order).
//! - `λ = 0.0`: pure novelty (after an initial relevance-driven pick, each
//!   subsequent slot maximises distance from what's already admitted).
//! - `μ` (artist penalty weight): demotes candidates whose artist is
//!   already represented in the queue or earlier admits. Stacks linearly
//!   — a 2nd admit by the same artist pays `2μ`, a 3rd pays `3μ`. With
//!   `μ = 0` the algorithm degrades to plain MMR; that's the regression
//!   path the bench's λ-sweep was tuned against.
//! - The **first slot is relevance-and-penalty-driven** (no novelty
//!   term). This avoids the λ=0 degenerate case where every score is 0
//!   and the selection collapses to "first candidate in input order".
//!   The artist penalty still applies — if the queue already has the
//!   most-relevant artist, the first MMR pick prefers a fresh artist.
//!
//! ## Design notes
//!
//! - **Pure function over slices.** Returns indices into the input so
//!   the caller can map back to its own data type (`AnnQueryResult` vs
//!   `AggregatedResult` — both flow through the gateway).
//! - **No I/O.** The caller is responsible for fetching candidate
//!   vectors (via [`AnnIndex::get_vector`](crate::ann::AnnIndex::get_vector) or otherwise)
//!   before invoking. This keeps the module trivially testable.
//! - **Missing vectors are tolerated.** A candidate without a vector is
//!   scored on relevance alone — its diversity penalty is treated as
//!   zero. Better than starving the slate; the cost is that a pathological
//!   "all candidates missing vectors" call degrades to pure-relevance
//!   ordering, which is what the system did before MMR existed anyway.
//! - **Missing artist keys are tolerated.** A candidate with
//!   `artist_key = None` pays no artist penalty (we have no signal to
//!   gate on). Same fail-open posture as missing vectors.
//! - **Ties break by input order.** A strict `>` in the max-find ensures
//!   the earliest-seen candidate at any given score wins. Combined with
//!   the relevance-first slot, this makes the output deterministic for a
//!   given input.

use std::collections::HashMap;

use music_core::TrackId;

/// One candidate fed into [`mmr_rerank`]. The caller fills these from
/// whatever its source is (ANN top-K, aggregated multi-seed pool).
#[derive(Clone, Debug)]
pub struct Candidate {
    pub track_id: TrackId,
    /// Similarity to the seed (cosine, in `[-1, 1]`). Used as the
    /// relevance term.
    pub sim_to_seed: f32,
    /// Embedding vector for the diversity term. `None` is acceptable —
    /// see module doc.
    pub vector: Option<Vec<f32>>,
    /// Stable artist identifier for the soft artist-diversity penalty.
    /// Conventionally the same `artist_key` the queue filter uses
    /// (`"id:<artist_id>"` when available, `"name:<lowercased>"`
    /// otherwise). `None` opts the candidate out of the penalty.
    pub artist_key: Option<String>,
    /// Additive adjustment to the relevance term — the user-preference
    /// affinity bonus (`preference_weight · affinity`). `0.0` = no
    /// adjustment (plain MMR). See the module doc's score formula.
    pub relevance_bonus: f32,
}

/// Greedy MMR selection. Returns indices into `candidates` in
/// admission order (best first). Length is `min(top_n, candidates.len())`.
///
/// `lambda` is clamped to `[0.0, 1.0]`; out-of-range values from a
/// misconfigured client don't panic. `artist_penalty_weight` is clamped
/// to `>= 0.0` for the same reason — negative weights would invert the
/// signal and are never what the caller meant.
///
/// `initial_artist_counts` seeds the per-artist tally with the queue's
/// existing contents; pass `&HashMap::new()` to score from a clean
/// slate (also the right call when `artist_penalty_weight == 0.0`).
#[allow(
    clippy::implicit_hasher, // single internal caller (the gateway)
    clippy::cast_precision_loss, // counts are bounded by queue size + top_n
)]
pub fn mmr_rerank(
    candidates: &[Candidate],
    lambda: f32,
    top_n: usize,
    artist_penalty_weight: f32,
    initial_artist_counts: &HashMap<String, u32>,
) -> Vec<usize> {
    if top_n == 0 || candidates.is_empty() {
        return Vec::new();
    }
    let lambda = lambda.clamp(0.0, 1.0);
    let mu = artist_penalty_weight.max(0.0);
    let n = candidates.len();
    let want = top_n.min(n);

    let mut selected: Vec<usize> = Vec::with_capacity(want);
    let mut remaining: Vec<usize> = (0..n).collect();
    // For each candidate index, the running max sim to any admitted
    // candidate so far. Initialised to 0; updated when each admit lands.
    // Indexed by the *original* candidate index, not by position in
    // `remaining` — `remaining` shrinks via swap_remove.
    let mut max_sim_to_admitted: Vec<f32> = vec![0.0; n];
    // Running per-artist tally, seeded from the queue. Mutated as we
    // admit; subsequent same-artist candidates see the bumped count
    // and pay a heavier penalty.
    let mut artist_counts: HashMap<String, u32> = initial_artist_counts.clone();

    // Relevance = raw seed-similarity plus the caller's additive bonus
    // (preference affinity). The diversity term deliberately does NOT use
    // this — it operates on the embedding vectors directly.
    let relevance = |cand_idx: usize| -> f32 {
        candidates[cand_idx].sim_to_seed + candidates[cand_idx].relevance_bonus
    };

    let artist_penalty = |cand_idx: usize, counts: &HashMap<String, u32>| -> f32 {
        if mu == 0.0 {
            return 0.0;
        }
        let Some(ak) = candidates[cand_idx].artist_key.as_deref() else {
            return 0.0;
        };
        let count = counts.get(ak).copied().unwrap_or(0);
        mu * (count as f32)
    };

    while selected.len() < want && !remaining.is_empty() {
        let pick_pos = if selected.is_empty() {
            // First slot: relevance + artist penalty, no novelty term
            // (max_sim_to_admitted is uniformly 0 here anyway).
            argmax_by(&remaining, |&cand_idx| {
                relevance(cand_idx) - artist_penalty(cand_idx, &artist_counts)
            })
        } else {
            argmax_by(&remaining, |&cand_idx| {
                let novelty_penalty = max_sim_to_admitted[cand_idx];
                lambda * relevance(cand_idx)
                    - (1.0 - lambda) * novelty_penalty
                    - artist_penalty(cand_idx, &artist_counts)
            })
        };
        // `remove` (O(n)) instead of `swap_remove` (O(1)) — order
        // matters because the tie-break in `argmax_by` is "first
        // occurrence wins". swap_remove would silently reorder the
        // tail and break determinism on equal scores. At our scale
        // (top_n ≤ 20, candidates ≤ 80) the cost is invisible.
        let picked_idx = remaining.remove(pick_pos);

        // Update the running max-sim cache for everyone still in play.
        if let Some(picked_vec) = candidates[picked_idx].vector.as_deref() {
            for &r_idx in &remaining {
                if let Some(r_vec) = candidates[r_idx].vector.as_deref() {
                    let s = cosine_similarity(picked_vec, r_vec);
                    if s > max_sim_to_admitted[r_idx] {
                        max_sim_to_admitted[r_idx] = s;
                    }
                }
            }
        }

        // Bump the running artist tally so subsequent picks by the same
        // artist pay a heavier penalty. No-op when the candidate carries
        // no artist key.
        if let Some(ak) = candidates[picked_idx].artist_key.as_deref() {
            *artist_counts.entry(ak.to_string()).or_insert(0) += 1;
        }

        selected.push(picked_idx);
    }

    selected
}

/// Find the position in `slice` whose mapped score is maximal. First
/// occurrence wins on ties (strict `>` comparison). Caller guarantees
/// `slice` is non-empty.
fn argmax_by<T, F>(slice: &[T], mut score: F) -> usize
where
    F: FnMut(&T) -> f32,
{
    let mut best_pos = 0;
    let mut best_score = score(&slice[0]);
    for (i, item) in slice.iter().enumerate().skip(1) {
        let s = score(item);
        if s > best_score {
            best_score = s;
            best_pos = i;
        }
    }
    best_pos
}

/// Cosine similarity in `[-1, 1]`. Returns 0.0 for length mismatches,
/// empty inputs, or zero vectors — keeps the caller's loop simple
/// without forcing a Result. CLAP outputs are unit-norm so the divide
/// is purely defensive.
pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut na = 0.0_f32;
    let mut nb = 0.0_f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = (na * nb).sqrt();
    if denom == 0.0 { 0.0 } else { dot / denom }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact-equality is intentional: the helper
// returns literal 0.0 / 1.0 in the cases we
// assert on.
mod tests {
    use super::*;

    fn cand(id: &str, sim: f32, vec: Vec<f32>) -> Candidate {
        Candidate {
            track_id: TrackId::from(id),
            sim_to_seed: sim,
            vector: Some(vec),
            artist_key: None,
            relevance_bonus: 0.0,
        }
    }

    fn cand_no_vec(id: &str, sim: f32) -> Candidate {
        Candidate {
            track_id: TrackId::from(id),
            sim_to_seed: sim,
            vector: None,
            artist_key: None,
            relevance_bonus: 0.0,
        }
    }

    /// Helper for penalty tests: builds a candidate with both a vector
    /// (so the diversity term is well-defined) and an artist key (so
    /// the penalty term applies).
    fn cand_with_artist(id: &str, sim: f32, vec: Vec<f32>, artist: &str) -> Candidate {
        Candidate {
            track_id: TrackId::from(id),
            sim_to_seed: sim,
            vector: Some(vec),
            artist_key: Some(artist.to_string()),
            relevance_bonus: 0.0,
        }
    }

    /// Candidate with an explicit preference bonus on the relevance term.
    fn cand_with_bonus(id: &str, sim: f32, vec: Vec<f32>, bonus: f32) -> Candidate {
        Candidate {
            track_id: TrackId::from(id),
            sim_to_seed: sim,
            vector: Some(vec),
            artist_key: None,
            relevance_bonus: bonus,
        }
    }

    /// Common "no penalty" call shape so individual tests don't have to
    /// repeat `0.0, &HashMap::new()`.
    fn no_penalty(candidates: &[Candidate], lambda: f32, top_n: usize) -> Vec<usize> {
        mmr_rerank(candidates, lambda, top_n, 0.0, &HashMap::new())
    }

    // --- empty / boundary ---

    #[test]
    fn empty_candidates_returns_empty() {
        assert!(no_penalty(&[], 0.7, 5).is_empty());
    }

    #[test]
    fn top_n_zero_returns_empty() {
        let cs = vec![cand("a", 0.9, vec![1.0, 0.0])];
        assert!(no_penalty(&cs, 0.7, 0).is_empty());
    }

    #[test]
    fn single_candidate_returned_regardless_of_lambda() {
        let cs = vec![cand("a", 0.9, vec![1.0, 0.0])];
        for &lam in &[0.0_f32, 0.3, 0.5, 0.7, 1.0] {
            assert_eq!(no_penalty(&cs, lam, 5), vec![0], "λ={lam}");
        }
    }

    #[test]
    fn top_n_caps_output_size() {
        let cs = vec![
            cand("a", 0.9, vec![1.0, 0.0]),
            cand("b", 0.8, vec![0.0, 1.0]),
            cand("c", 0.7, vec![0.5, 0.5]),
        ];
        assert_eq!(no_penalty(&cs, 0.7, 2).len(), 2);
    }

    #[test]
    fn top_n_greater_than_candidates_returns_all() {
        let cs = vec![
            cand("a", 0.9, vec![1.0, 0.0]),
            cand("b", 0.8, vec![0.0, 1.0]),
        ];
        assert_eq!(no_penalty(&cs, 0.7, 99).len(), 2);
    }

    // --- λ extremes ---

    #[test]
    fn lambda_one_orders_by_relevance_descending() {
        let cs = vec![
            cand("a", 0.5, vec![1.0, 0.0]),
            cand("b", 0.9, vec![1.0, 0.0]),
            cand("c", 0.7, vec![1.0, 0.0]),
        ];
        // λ=1: score = sim_to_seed. Order: b (0.9), c (0.7), a (0.5).
        assert_eq!(no_penalty(&cs, 1.0, 3), vec![1, 2, 0]);
    }

    #[test]
    fn lambda_zero_first_pick_is_most_relevant() {
        // Module doc: first slot is relevance-driven regardless of λ.
        let cs = vec![
            cand("a", 0.5, vec![1.0, 0.0]),
            cand("b", 0.9, vec![0.0, 1.0]),
            cand("c", 0.7, vec![1.0, 0.0]),
        ];
        let out = no_penalty(&cs, 0.0, 1);
        assert_eq!(out, vec![1]);
    }

    #[test]
    fn lambda_zero_subsequent_picks_minimise_similarity_to_admitted() {
        // After picking b (idx 1, vec [0,1]):
        //   a: sim(a, b) = 0   → score = 0
        //   c: sim(c, b) = -1  → score = -(-1) * 1.0 = 1
        // Wait: λ=0 → score = 0*rel - 1*max_sim = -max_sim.
        //   a: -max_sim = 0
        //   c: -max_sim = -(-1) = 1
        // c wins (more dissimilar to b → bigger negative sim → smaller
        // penalty when negated).
        let cs = vec![
            cand("a", 0.5, vec![1.0, 0.0]),
            cand("b", 0.9, vec![0.0, 1.0]),
            cand("c", 0.7, vec![-1.0, 0.0]),
        ];
        let out = no_penalty(&cs, 0.0, 3);
        assert_eq!(out[0], 1);
        // Wait actually max_sim_to_admitted is updated unconditionally
        // with `if s > current`. c's sim to b is 0 + 0 = 0 (dot product
        // of [-1, 0] and [0, 1]). Hmm, vectors are orthogonal in this
        // case. Let me re-think with concrete vectors.
        //
        // Pick 1: b (idx 1).
        //   sim(a, b) = (1*0 + 0*1) / (1*1) = 0
        //   sim(c, b) = (-1*0 + 0*1) / (1*1) = 0
        // Both 0 → max_sim updates to 0 only if 0 > 0 (false). So
        // max_sim_to_admitted stays at initial 0 for both.
        //
        // Pick 2 with λ=0: score = -max_sim_to_admitted = 0 for both.
        // Tie-break by input order → a (idx 0) wins.
        assert_eq!(out[1], 0);
        assert_eq!(out[2], 2);
    }

    #[test]
    fn lambda_zero_picks_orthogonal_then_opposite() {
        // Construct so the diversity ordering is unambiguous:
        // a: vec [1,0,0], identical to seed-vec direction
        // b: orthogonal to a → sim 0
        // c: similar to a → sim ≈ 0.99
        // After pick a, between b and c: b has lower sim_to_admitted.
        // λ=0 → b wins regardless of lower relevance.
        let cs = vec![
            cand("a", 0.9, vec![1.0, 0.0, 0.0]),
            cand("b", 0.5, vec![0.0, 1.0, 0.0]), // orthogonal to a
            cand("c", 0.85, vec![0.99, 0.14, 0.0]), // close to a
        ];
        let out = no_penalty(&cs, 0.0, 3);
        assert_eq!(out[0], 0); // a — highest relevance, first pick
        assert_eq!(out[1], 1); // b — most novel
        assert_eq!(out[2], 2); // c — last
    }

    // --- canonical MMR balance ---

    #[test]
    fn lambda_balances_relevance_against_diversity() {
        // Hand-tuned to make the MMR tradeoff visible:
        // a: vec [1,0,0], rel=0.9   (top relevance)
        // b: very close to a (rel=0.85, vec ≈ a)
        // c: orthogonal to a (rel=0.6)
        // First pick: a (highest rel).
        // Second pick at λ=0.5:
        //   b: 0.5*0.85 - 0.5*sim(b,a) ≈ 0.5*0.85 - 0.5*0.95 = -0.05
        //   c: 0.5*0.6  - 0.5*sim(c,a) = 0.5*0.6 - 0.5*0 = 0.3
        // → c wins.
        let cs = vec![
            cand("a", 0.9, vec![1.0, 0.0, 0.0]),
            cand("b", 0.85, vec![0.95, 0.31225, 0.0]), // sim(b,a)≈0.95
            cand("c", 0.6, vec![0.0, 1.0, 0.0]),       // orthogonal
        ];
        let out = no_penalty(&cs, 0.5, 3);
        assert_eq!(out[0], 0); // a
        assert_eq!(out[1], 2); // c — diverse beats similar
        assert_eq!(out[2], 1); // b — last
    }

    #[test]
    fn high_lambda_keeps_relevance_order_when_close_neighbour_wins() {
        // Same setup as above, but λ=0.95 → relevance dominates.
        // Second pick:
        //   b: 0.95*0.85 - 0.05*0.95 = 0.8075 - 0.0475 = 0.76
        //   c: 0.95*0.6  - 0.05*0   = 0.57
        // → b wins despite being similar to a.
        let cs = vec![
            cand("a", 0.9, vec![1.0, 0.0, 0.0]),
            cand("b", 0.85, vec![0.95, 0.31225, 0.0]),
            cand("c", 0.6, vec![0.0, 1.0, 0.0]),
        ];
        let out = no_penalty(&cs, 0.95, 3);
        assert_eq!(out, vec![0, 1, 2]);
    }

    // --- missing-vector fallback ---

    #[test]
    fn missing_vector_uses_relevance_only() {
        // b has no vector → diversity penalty stays 0 throughout.
        // First pick (relevance) → b (0.9).
        // Second pick at λ=0.5:
        //   a: 0.5*0.5 - 0.5*0 (b has no vector) = 0.25
        //   No other candidates.
        // → a wins.
        let cs = vec![cand("a", 0.5, vec![1.0, 0.0]), cand_no_vec("b", 0.9)];
        let out = no_penalty(&cs, 0.5, 2);
        assert_eq!(out, vec![1, 0]);
    }

    #[test]
    fn all_missing_vectors_collapses_to_relevance_order() {
        let cs = vec![
            cand_no_vec("a", 0.5),
            cand_no_vec("b", 0.9),
            cand_no_vec("c", 0.7),
        ];
        // Every score reduces to lambda * relevance after the first pick
        // (no diversity term), so order is by relevance descending for
        // any λ > 0.
        assert_eq!(no_penalty(&cs, 0.7, 3), vec![1, 2, 0]);
    }

    // --- determinism ---

    #[test]
    fn ties_break_by_input_order() {
        // All identical scores → first occurrence wins on each pick.
        let cs = vec![
            cand("a", 0.5, vec![1.0, 0.0]),
            cand("b", 0.5, vec![1.0, 0.0]),
            cand("c", 0.5, vec![1.0, 0.0]),
        ];
        assert_eq!(no_penalty(&cs, 0.7, 3), vec![0, 1, 2]);
    }

    #[test]
    fn lambda_clamps_to_unit_range() {
        let cs = vec![
            cand("a", 0.5, vec![1.0, 0.0]),
            cand("b", 0.9, vec![1.0, 0.0]),
        ];
        // Out-of-range λ should be clamped, not panic. Negative → 0.
        let out_low = no_penalty(&cs, -1.0, 2);
        let out_zero = no_penalty(&cs, 0.0, 2);
        assert_eq!(out_low, out_zero);
        // > 1 → 1.
        let out_high = no_penalty(&cs, 5.0, 2);
        let out_one = no_penalty(&cs, 1.0, 2);
        assert_eq!(out_high, out_one);
    }

    // --- cosine_similarity helper ---

    #[test]
    fn cosine_similarity_basic() {
        // Identical unit vectors → 1.
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        // Orthogonal → 0.
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        // Opposite → -1.
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_handles_zero_vector() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.0, 0.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[0.0, 0.0]), 0.0);
    }

    #[test]
    fn cosine_similarity_handles_length_mismatch() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[1.0, 0.0]), 0.0);
    }

    #[test]
    fn cosine_similarity_normalises_non_unit_vectors() {
        // [2,0] and [3,0] should be sim=1 (same direction).
        assert!((cosine_similarity(&[2.0, 0.0], &[3.0, 0.0]) - 1.0).abs() < 1e-6);
    }

    // --- artist penalty ---

    #[test]
    fn zero_penalty_weight_is_a_no_op_against_the_unpenalised_path() {
        let cs = vec![
            cand_with_artist("a", 0.9, vec![1.0, 0.0], "ar1"),
            cand_with_artist("b", 0.85, vec![0.95, 0.31225], "ar1"),
            cand_with_artist("c", 0.6, vec![0.0, 1.0], "ar2"),
        ];
        let with_penalty = mmr_rerank(&cs, 0.7, 3, 0.0, &HashMap::new());
        let without_penalty = no_penalty(&cs, 0.7, 3);
        assert_eq!(with_penalty, without_penalty);
    }

    #[test]
    fn first_slot_demotes_already_queued_artist() {
        // Two candidates at identical relevance, one's artist is already
        // in the queue (count=1). At μ=0.15 the fresh artist wins.
        let cs = vec![
            cand_with_artist("a", 0.9, vec![1.0, 0.0], "duke"),
            cand_with_artist("b", 0.9, vec![0.0, 1.0], "fresh"),
        ];
        let mut counts = HashMap::new();
        counts.insert("duke".to_string(), 1);
        let out = mmr_rerank(&cs, 0.7, 1, 0.15, &counts);
        assert_eq!(out, vec![1], "fresh artist should win the first slot");
    }

    #[test]
    fn all_same_artist_still_fills_slate_when_no_alternative_exists() {
        // Smada scenario: every candidate is by the seed artist. The
        // soft penalty must still admit them so the queue doesn't
        // starve — that was the whole point of swapping the hard cap.
        let cs = vec![
            cand_with_artist("a", 0.88, vec![1.0, 0.0, 0.0], "duke"),
            cand_with_artist("b", 0.87, vec![0.0, 1.0, 0.0], "duke"),
            cand_with_artist("c", 0.86, vec![0.0, 0.0, 1.0], "duke"),
        ];
        let mut counts = HashMap::new();
        counts.insert("duke".to_string(), 1);
        let out = mmr_rerank(&cs, 0.8, 3, 0.15, &counts);
        assert_eq!(
            out.len(),
            3,
            "all three candidates admitted despite same-artist penalty"
        );
    }

    #[test]
    fn penalty_stacks_linearly_and_runs_are_deterministic() {
        let cs = vec![
            cand_with_artist("a", 0.9, vec![1.0, 0.0, 0.0], "duke"),
            cand_with_artist("b", 0.8, vec![0.0, 1.0, 0.0], "duke"),
            cand_with_artist("c", 0.7, vec![0.0, 0.0, 1.0], "duke"),
        ];
        let out_run1 = mmr_rerank(&cs, 0.7, 3, 0.15, &HashMap::new());
        let out_run2 = mmr_rerank(&cs, 0.7, 3, 0.15, &HashMap::new());
        assert_eq!(out_run1.len(), 3);
        assert_eq!(out_run1, out_run2, "deterministic across runs");
    }

    #[test]
    fn fresh_artist_beats_more_relevant_same_artist_at_typical_weights() {
        // Trace-grounded: at the production μ=0.15, a 0.88-sim
        // candidate by an already-queued artist loses to a 0.85-sim
        // fresh-artist candidate.
        //   queued: count=1 → 0.88 - 0.15 = 0.73
        //   fresh:  count=0 → 0.85 - 0.0  = 0.85
        let cs = vec![
            cand_with_artist("queued", 0.88, vec![1.0, 0.0], "duke"),
            cand_with_artist("fresh", 0.85, vec![0.0, 1.0], "ar2"),
        ];
        let mut counts = HashMap::new();
        counts.insert("duke".to_string(), 1);
        let out = mmr_rerank(&cs, 0.8, 1, 0.15, &counts);
        assert_eq!(out, vec![1]);
    }

    #[test]
    fn candidate_without_artist_key_pays_no_penalty() {
        let mut a = cand_with_artist("a", 0.5, vec![1.0, 0.0], "duke");
        a.artist_key = None;
        let cs = vec![a, cand_with_artist("b", 0.5, vec![0.0, 1.0], "duke")];
        let mut counts = HashMap::new();
        counts.insert("duke".to_string(), 1);
        let out = mmr_rerank(&cs, 0.7, 1, 0.15, &counts);
        assert_eq!(out, vec![0]);
    }

    // --- relevance bonus (user-preference affinity) ---

    #[test]
    fn zero_bonus_matches_the_unadjusted_path() {
        // A bonus of 0 everywhere must produce identical output to plain
        // candidates — the cold-start / preference-disabled guarantee.
        let plain = vec![
            cand("a", 0.9, vec![1.0, 0.0]),
            cand("b", 0.7, vec![0.0, 1.0]),
            cand("c", 0.5, vec![0.5, 0.5]),
        ];
        let zeroed = vec![
            cand_with_bonus("a", 0.9, vec![1.0, 0.0], 0.0),
            cand_with_bonus("b", 0.7, vec![0.0, 1.0], 0.0),
            cand_with_bonus("c", 0.5, vec![0.5, 0.5], 0.0),
        ];
        assert_eq!(no_penalty(&plain, 0.7, 3), no_penalty(&zeroed, 0.7, 3));
    }

    #[test]
    fn positive_bonus_promotes_a_liked_candidate() {
        // λ=1 (pure relevance). b is less similar (0.7 vs 0.9) but a
        // strong preference bonus lifts its relevance above a's.
        //   a: 0.9 + 0.0  = 0.9
        //   b: 0.7 + 0.25 = 0.95  → b wins the first slot
        let cs = vec![
            cand_with_bonus("a", 0.9, vec![1.0, 0.0], 0.0),
            cand_with_bonus("b", 0.7, vec![0.0, 1.0], 0.25),
        ];
        let out = mmr_rerank(&cs, 1.0, 1, 0.0, &HashMap::new());
        assert_eq!(out, vec![1], "liked-but-less-similar b should win");
    }

    #[test]
    fn negative_bonus_demotes_a_disliked_candidate() {
        // λ=1. a is most similar but a disliked penalty sinks it below b.
        //   a: 0.9 - 0.3 = 0.6
        //   b: 0.7 + 0.0 = 0.7  → b wins
        let cs = vec![
            cand_with_bonus("a", 0.9, vec![1.0, 0.0], -0.3),
            cand_with_bonus("b", 0.7, vec![0.0, 1.0], 0.0),
        ];
        let out = mmr_rerank(&cs, 1.0, 2, 0.0, &HashMap::new());
        assert_eq!(out, vec![1, 0], "disliked a demoted below b");
    }

    #[test]
    fn small_bonus_does_not_override_a_large_relevance_gap() {
        // The bonus is a tilt, not an override: a modest bonus can't drag
        // a far-less-relevant track to the top.
        //   a: 0.95 + 0.0  = 0.95
        //   b: 0.40 + 0.15 = 0.55  → a still wins
        let cs = vec![
            cand_with_bonus("a", 0.95, vec![1.0, 0.0], 0.0),
            cand_with_bonus("b", 0.40, vec![0.0, 1.0], 0.15),
        ];
        let out = mmr_rerank(&cs, 1.0, 1, 0.0, &HashMap::new());
        assert_eq!(out, vec![0]);
    }

    #[test]
    fn bonus_feeds_relevance_not_the_diversity_term() {
        // Two identical-direction vectors so the diversity term is the
        // same for both; the bonus only shifts relevance. At λ=0.5 the
        // first pick is relevance+penalty-driven → the bonus decides it.
        let cs = vec![
            cand_with_bonus("a", 0.8, vec![1.0, 0.0], 0.0),
            cand_with_bonus("b", 0.8, vec![1.0, 0.0], 0.2),
        ];
        let out = mmr_rerank(&cs, 0.5, 1, 0.0, &HashMap::new());
        assert_eq!(out, vec![1], "bonus breaks the relevance tie");
    }

    #[test]
    fn negative_penalty_weight_is_clamped_to_zero() {
        // A μ=-1 from a misconfigured client must not invert the
        // signal into a "prefer repeated artists" preference.
        let cs = vec![
            cand_with_artist("a", 0.9, vec![1.0, 0.0], "duke"),
            cand_with_artist("b", 0.85, vec![0.0, 1.0], "ar2"),
        ];
        let mut counts = HashMap::new();
        counts.insert("duke".to_string(), 5);
        let out_neg = mmr_rerank(&cs, 1.0, 1, -1.0, &counts);
        let out_zero = mmr_rerank(&cs, 1.0, 1, 0.0, &counts);
        assert_eq!(out_neg, out_zero);
    }
}
