//! Pure aggregation helpers for multi-seed recommendations.
//!
//! The gateway's `from-seeds` endpoint runs N per-seed ANN queries and
//! folds them into a single ranked list with Σ-similarity scoring.
//! That fold is small, pure, and testable in isolation; this module
//! is the home for it. No I/O, no async, no locks — just slices,
//! HashMaps, and a sort.

use std::collections::{HashMap, HashSet};

use music_core::TrackId;
use rand::Rng;

use crate::ann::AnnQueryResult;

/// One candidate after aggregation across seeds.
///
/// `score` is Σ of the per-seed cosine similarity over the seeds that
/// surfaced this track. Tracks that show up under more seeds rank
/// higher than tracks that show up under just one — a cheap proxy for
/// "in the centroid of the seed set."
#[derive(Clone, Debug, PartialEq)]
pub struct AggregatedResult {
    pub track_id: TrackId,
    pub score: f32,
    /// Count of distinct seeds that surfaced this track. Useful for
    /// diagnostics and for client-side tiebreaking if `score` ties.
    pub seed_hits: u32,
}

/// Combine per-seed ANN results into a Σ-similarity ranking.
///
/// - `per_seed` — one entry per queried seed; the `Vec<AnnQueryResult>` is
///   that seed's top-K (already sorted by similarity descending).
/// - `exclude` — track ids to drop unconditionally. Pass the full sampled
///   seed set here plus any caller-supplied exclusions; otherwise a seed
///   that happens to land in another seed's neighborhood would surface as
///   a recommendation.
/// - `top_n` — final cap. Returned vec is sorted by `score` descending,
///   ties broken by `seed_hits` then by `track_id` for determinism.
///
/// Empty `per_seed` is allowed and returns an empty vec. Empty inner
/// result vecs are allowed and contribute nothing.
pub fn aggregate_seed_results<S: std::hash::BuildHasher>(
    per_seed: &[Vec<AnnQueryResult>],
    exclude: &HashSet<TrackId, S>,
    top_n: usize,
) -> Vec<AggregatedResult> {
    if top_n == 0 {
        return Vec::new();
    }
    let mut scores: HashMap<TrackId, (f32, u32)> = HashMap::new();
    for seed_results in per_seed {
        for hit in seed_results {
            if exclude.contains(&hit.track_id) {
                continue;
            }
            let entry = scores.entry(hit.track_id.clone()).or_insert((0.0, 0));
            entry.0 += hit.similarity;
            entry.1 += 1;
        }
    }
    let mut ranked: Vec<AggregatedResult> = scores
        .into_iter()
        .map(|(track_id, (score, seed_hits))| AggregatedResult {
            track_id,
            score,
            seed_hits,
        })
        .collect();
    // Stable, deterministic ordering: score desc, hits desc, then id asc.
    // The id tiebreak matters for tests and for parity with hash-map
    // iteration order across runs.
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.seed_hits.cmp(&a.seed_hits))
            .then_with(|| a.track_id.as_str().cmp(b.track_id.as_str()))
    });
    ranked.truncate(top_n);
    ranked
}

/// Pick `k` distinct indices from `[0, len)` without replacement.
///
/// Partial Fisher–Yates: O(k) instead of O(len) — we don't care about
/// the rest of the permutation. Mirrors the TS `sampleN` helper that
/// this is replacing.
///
/// If `k >= len` returns `[0, len)` in order. If `len == 0` returns
/// empty. Sortable: caller can `.sort()` if they want a positional
/// sample rather than a random-order one.
pub fn sample_indices<R: Rng + ?Sized>(rng: &mut R, len: usize, k: usize) -> Vec<usize> {
    if len == 0 || k == 0 {
        return Vec::new();
    }
    if k >= len {
        return (0..len).collect();
    }
    let mut pool: Vec<usize> = (0..len).collect();
    let mut out = Vec::with_capacity(k);
    for i in 0..k {
        let j = i + rng.gen_range(0..(len - i));
        pool.swap(i, j);
        out.push(pool[i]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::SmallRng;

    fn t(s: &str) -> TrackId {
        TrackId::from(s)
    }

    fn hit(id: &str, sim: f32) -> AnnQueryResult {
        AnnQueryResult {
            track_id: t(id),
            similarity: sim,
        }
    }

    // --- aggregate_seed_results ---

    #[test]
    fn empty_per_seed_returns_empty() {
        let out = aggregate_seed_results(&[], &HashSet::new(), 10);
        assert!(out.is_empty());
    }

    #[test]
    fn single_seed_passes_through_in_score_order() {
        let per_seed = vec![vec![hit("a", 0.9), hit("b", 0.5), hit("c", 0.7)]];
        let out = aggregate_seed_results(&per_seed, &HashSet::new(), 10);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].track_id, t("a"));
        assert_eq!(out[1].track_id, t("c"));
        assert_eq!(out[2].track_id, t("b"));
        // seed_hits = 1 for everyone (one seed)
        for r in &out {
            assert_eq!(r.seed_hits, 1);
        }
    }

    #[test]
    fn track_under_two_seeds_outranks_track_under_one() {
        // "shared" appears under both seeds with smaller similarity each
        // ("shared" 0.4 + 0.4 = 0.8); "solo" appears under only the first
        // seed at 0.7. Σ-similarity must rank "shared" above "solo".
        let per_seed = vec![
            vec![hit("solo", 0.7), hit("shared", 0.4)],
            vec![hit("shared", 0.4), hit("only_b", 0.6)],
        ];
        let out = aggregate_seed_results(&per_seed, &HashSet::new(), 10);
        let ids: Vec<&str> = out.iter().map(|r| r.track_id.as_str()).collect();
        assert_eq!(ids, vec!["shared", "solo", "only_b"]);
        assert_eq!(out[0].seed_hits, 2);
        assert!((out[0].score - 0.8).abs() < 1e-6);
        assert_eq!(out[1].seed_hits, 1);
        assert_eq!(out[2].seed_hits, 1);
    }

    #[test]
    fn exclude_set_drops_candidates() {
        let per_seed = vec![vec![hit("a", 0.9), hit("b", 0.5)], vec![hit("c", 0.7)]];
        let mut exclude = HashSet::new();
        exclude.insert(t("a"));
        exclude.insert(t("c"));
        let out = aggregate_seed_results(&per_seed, &exclude, 10);
        let ids: Vec<&str> = out.iter().map(|r| r.track_id.as_str()).collect();
        assert_eq!(ids, vec!["b"]);
    }

    #[test]
    fn exclude_set_drops_a_seed_track_that_surfaced_under_another_seed() {
        // Caller passes the seed set as part of `exclude` to keep seeds
        // out of their own recommendations. Verify that contract.
        let per_seed = vec![vec![hit("seed_b", 0.95), hit("c", 0.5)]];
        let mut exclude = HashSet::new();
        exclude.insert(t("seed_a"));
        exclude.insert(t("seed_b"));
        let out = aggregate_seed_results(&per_seed, &exclude, 10);
        let ids: Vec<&str> = out.iter().map(|r| r.track_id.as_str()).collect();
        assert_eq!(ids, vec!["c"]);
    }

    #[test]
    fn top_n_truncates_after_sort() {
        let per_seed = vec![vec![
            hit("a", 0.1),
            hit("b", 0.9),
            hit("c", 0.5),
            hit("d", 0.7),
        ]];
        let out = aggregate_seed_results(&per_seed, &HashSet::new(), 2);
        let ids: Vec<&str> = out.iter().map(|r| r.track_id.as_str()).collect();
        assert_eq!(ids, vec!["b", "d"]);
    }

    #[test]
    fn top_n_zero_returns_empty() {
        let per_seed = vec![vec![hit("a", 0.9)]];
        let out = aggregate_seed_results(&per_seed, &HashSet::new(), 0);
        assert!(out.is_empty());
    }

    #[test]
    fn ties_broken_by_seed_hits_then_track_id() {
        // Two tracks with same total score: one from 2 seeds (0.4+0.4),
        // the other from 1 seed (0.8). Hits-desc puts the 2-seed track
        // first. Then we add a third track that ties on both score and
        // hits — track_id ascending breaks that.
        let per_seed = vec![
            vec![hit("two_seeds", 0.4), hit("zzz_one_seed", 0.8)],
            vec![hit("two_seeds", 0.4), hit("aaa_one_seed", 0.8)],
        ];
        let out = aggregate_seed_results(&per_seed, &HashSet::new(), 10);
        let ids: Vec<&str> = out.iter().map(|r| r.track_id.as_str()).collect();
        // two_seeds: score 0.8, hits 2
        // aaa_one_seed: score 0.8, hits 1 (sorts before zzz alphabetically)
        // zzz_one_seed: score 0.8, hits 1
        assert_eq!(ids, vec!["two_seeds", "aaa_one_seed", "zzz_one_seed"]);
    }

    #[test]
    fn empty_inner_results_contribute_nothing() {
        let per_seed = vec![vec![], vec![hit("a", 0.5)], vec![]];
        let out = aggregate_seed_results(&per_seed, &HashSet::new(), 10);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].track_id, t("a"));
        assert_eq!(out[0].seed_hits, 1);
    }

    // --- sample_indices ---

    #[test]
    fn sample_empty_or_zero_k_returns_empty() {
        let mut rng = SmallRng::seed_from_u64(1);
        assert!(sample_indices(&mut rng, 0, 5).is_empty());
        assert!(sample_indices(&mut rng, 5, 0).is_empty());
    }

    #[test]
    fn sample_k_ge_len_returns_full_range_in_order() {
        let mut rng = SmallRng::seed_from_u64(1);
        let out = sample_indices(&mut rng, 3, 5);
        assert_eq!(out, vec![0, 1, 2]);
        let out = sample_indices(&mut rng, 3, 3);
        assert_eq!(out, vec![0, 1, 2]);
    }

    #[test]
    fn sample_returns_k_distinct_indices_in_range() {
        let mut rng = SmallRng::seed_from_u64(42);
        let out = sample_indices(&mut rng, 100, 10);
        assert_eq!(out.len(), 10);
        let unique: HashSet<usize> = out.iter().copied().collect();
        assert_eq!(unique.len(), 10);
        for i in &out {
            assert!(*i < 100);
        }
    }

    #[test]
    fn sample_is_deterministic_under_seed() {
        let mut rng_a = SmallRng::seed_from_u64(123);
        let mut rng_b = SmallRng::seed_from_u64(123);
        let out_a = sample_indices(&mut rng_a, 50, 8);
        let out_b = sample_indices(&mut rng_b, 50, 8);
        assert_eq!(out_a, out_b);
    }
}
