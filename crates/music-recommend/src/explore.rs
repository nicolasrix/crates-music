//! Exploration jitter for recommendation ranking.
//!
//! A deterministic recommend pipeline — fixed seeds, the same ANN, the same
//! re-scoring — returns the *identical* top candidate on every refill. For a
//! travelling autoplay station that re-seeds from its own recent output, that
//! collapses onto a narrow head of the catalogue: in production the top 20
//! tracks filled a third of all autoplay picks, with individual tracks served
//! 30+ times.
//!
//! The fix is to turn the final pick from an `argmax` into a *sample*. Adding
//! `temperature · Gumbel(0,1)` noise to each candidate's relevance score and
//! then taking the max is the **Gumbel-max trick**: the winner is drawn from
//! `softmax(score / temperature)`. A modest temperature reshuffles genuine
//! near-ties (candidates within ~`temperature` of the leader) while leaving a
//! clearly-better candidate ahead and a clearly-worse one — e.g. a
//! leash-penalised, out-of-boundary track — behind. So the station still
//! travels coherently; it just stops re-picking the same neighbour every time.
//!
//! `temperature <= 0` is a no-op, preserving the deterministic ranking.

use rand::Rng;

/// One standard Gumbel(0,1) sample via inverse-CDF transform: `-ln(-ln(U))`
/// with `U` drawn from the open interval `(0, 1)`. The lower bound is clamped
/// to [`f32::EPSILON`] so the inner `ln` never sees zero (which would yield
/// `+inf` and a degenerate always-wins candidate).
fn gumbel<R: Rng + ?Sized>(rng: &mut R) -> f32 {
    let u: f32 = rng.gen_range(f32::EPSILON..1.0);
    -(-u.ln()).ln()
}

/// Add `temperature · Gumbel(0,1)` to every score in place. The caller is
/// responsible for re-sorting afterwards — the perturbation only matters once
/// the candidates are re-ranked by the mutated score. A `temperature` of 0 (or
/// negative) leaves the slice untouched, so callers can pass a config knob
/// straight through to disable exploration.
pub fn perturb_scores<R: Rng + ?Sized>(scores: &mut [f32], rng: &mut R, temperature: f32) {
    if temperature <= 0.0 {
        return;
    }
    for s in scores.iter_mut() {
        *s += temperature * gumbel(rng);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::SmallRng;

    #[test]
    fn zero_temperature_is_a_noop() {
        let mut rng = SmallRng::seed_from_u64(42);
        let mut scores = vec![1.0_f32, 0.9, 0.8];
        let before = scores.clone();
        perturb_scores(&mut scores, &mut rng, 0.0);
        assert_eq!(scores, before);
    }

    #[test]
    fn negative_temperature_is_a_noop() {
        let mut rng = SmallRng::seed_from_u64(1);
        let mut scores = vec![0.5_f32, 0.4];
        let before = scores.clone();
        perturb_scores(&mut scores, &mut rng, -1.0);
        assert_eq!(scores, before);
    }

    #[test]
    fn positive_temperature_perturbs_and_stays_finite() {
        let mut rng = SmallRng::seed_from_u64(7);
        let mut scores = vec![1.0_f32; 8];
        perturb_scores(&mut scores, &mut rng, 0.15);
        // Every score moved (Gumbel is a.s. non-zero) and stays finite —
        // the EPSILON clamp keeps the inverse-CDF away from ±inf.
        assert!(scores.iter().all(|s| s.is_finite()));
        assert!(scores.iter().any(|&s| (s - 1.0).abs() > 1e-6));
    }

    #[test]
    fn argmax_can_change_under_jitter_for_near_ties() {
        // Three near-tied candidates: with enough draws the winning index is
        // not always 0, i.e. the pick is genuinely stochastic rather than a
        // fixed argmax. (Deterministic seeds keep the test reproducible.)
        let base = [1.00_f32, 0.98, 0.96];
        let mut winners = std::collections::HashSet::new();
        for seed in 0..200u64 {
            let mut rng = SmallRng::seed_from_u64(seed);
            let mut s = base.to_vec();
            perturb_scores(&mut s, &mut rng, 0.2);
            let win = s
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(i, _)| i)
                .unwrap();
            winners.insert(win);
        }
        assert!(
            winners.len() >= 2,
            "expected jitter to let more than one candidate win, got {winners:?}"
        );
    }

    #[test]
    fn dominant_candidate_usually_survives_modest_jitter() {
        // A clearly-better leader (gap ≫ temperature) should still win the
        // large majority of the time — exploration must not torch relevance.
        let base = [2.0_f32, 0.5, 0.4];
        let mut leader_wins = 0;
        for seed in 0..200u64 {
            let mut rng = SmallRng::seed_from_u64(seed);
            let mut s = base.to_vec();
            perturb_scores(&mut s, &mut rng, 0.15);
            if s[0] > s[1] && s[0] > s[2] {
                leader_wins += 1;
            }
        }
        assert!(leader_wins > 190, "leader won only {leader_wins}/200");
    }
}
