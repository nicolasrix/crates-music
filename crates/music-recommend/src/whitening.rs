//! All-but-the-Top (ABTT) whitening for content embeddings.
//!
//! CLaMP 3 audio + text embeddings are anisotropic: they occupy a narrow
//! cone of the sphere, so raw cosine similarities are inflated and poorly
//! separated (a "rainy afternoon" text query and a "thrash metal" query
//! land in nearly the same place). ABTT (Mu & Viswanath, ICLR 2018,
//! "All-but-the-Top") removes the shared structure: subtract the corpus
//! mean, project out the top `k` principal directions, and renormalize
//! for cosine.
//!
//! The transform is *post-hoc* over the existing raw embeddings — no
//! re-embedding. It is fit once over the audio corpus (the only vectors
//! we store at scale) and applied to everything that enters the content
//! ANN, including text station queries. Fitting on audio only leaves a
//! residual text-modality offset (the cross-modal gap); correcting that
//! with a separate text mean is a tracked follow-up.
//!
//! Dimensionality is preserved — ABTT projects directions *out* of the
//! space but keeps all `dim` coordinates, so the ANN's fixed dim and the
//! cosine metric are unchanged. The matrix this stores is tiny
//! (`mean` + `k`×`dim`, k≈dim/100), unlike full whitening's dim×dim.

// Numerics: this module accumulates in f64 for stability and stores/returns
// f32 to match the embedding wire format, so f32↔f64 casts are intentional
// (lossless widening, or accepted narrowing on the way out).
#![allow(
    clippy::cast_lossless,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use crate::{Error, Result};

/// Fixed seed for the power-iteration init. Fitting must be deterministic:
/// the same corpus has to yield the same transform across boots, or the
/// ANN and incoming queries would whiten inconsistently.
const FIT_SEED: u64 = 0x5715_3300_ABCD_1234;
/// Power-iteration caps. `k` is tiny and the spectrum is well-separated
/// at the top, so convergence is fast; these are safety bounds.
const MAX_POWER_ITERS: usize = 256;
const POWER_CONVERGE_TOL: f64 = 1e-9;
/// Below this eigen-norm the residual data is numerically rank-exhausted;
/// stop adding components rather than emit noise directions.
const RANK_EPS: f64 = 1e-12;

/// Number of principal directions to remove. The ABTT paper uses ≈d/100;
/// we round down and clamp to at least 1.
pub fn default_k(dim: usize) -> usize {
    (dim / 100).max(1)
}

/// A fitted All-but-the-Top transform: a mean vector and `k` orthonormal
/// principal directions to project out.
#[derive(Clone, Debug, PartialEq)]
pub struct Whitening {
    /// Audio corpus mean. Subtracted from audio vectors on the way into
    /// the ANN and from audio (seed) queries.
    mean: Vec<f32>,
    /// `k` unit principal directions, each `dim`-long, in descending
    /// variance order. May be shorter than the requested `k` if the
    /// corpus is rank-deficient. Shared by both modalities.
    components: Vec<Vec<f32>>,
    /// Optional text-modality mean, for the cross-modal correction.
    /// CLaMP 3 text embeddings sit at an offset from audio (the modality
    /// gap); centering a text query by `text_mean` instead of the audio
    /// `mean` — then applying the same `components` — keeps text and audio
    /// comparable in the whitened space. `None` falls back to the audio
    /// mean (text queries then collapse — the pre-fix behaviour).
    text_mean: Option<Vec<f32>>,
    dim: usize,
}

impl Whitening {
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Number of directions actually removed (≤ requested k).
    pub fn k(&self) -> usize {
        self.components.len()
    }

    pub fn mean(&self) -> &[f32] {
        &self.mean
    }

    pub fn components(&self) -> &[Vec<f32>] {
        &self.components
    }

    pub fn text_mean(&self) -> Option<&[f32]> {
        self.text_mean.as_deref()
    }

    /// Whether the cross-modal text mean has been fitted. When false,
    /// `transform_text` falls back to the audio mean.
    pub fn has_text_mean(&self) -> bool {
        self.text_mean.is_some()
    }

    /// Reconstruct from stored parts (e.g. loaded from SQLite). Validates
    /// that every component — and `text_mean`, if present — matches
    /// `mean.len()`.
    pub fn from_parts(
        mean: Vec<f32>,
        components: Vec<Vec<f32>>,
        text_mean: Option<Vec<f32>>,
    ) -> Result<Self> {
        let dim = mean.len();
        if dim == 0 {
            return Err(Error::Whitening("mean is empty".into()));
        }
        for (i, c) in components.iter().enumerate() {
            if c.len() != dim {
                return Err(Error::Whitening(format!(
                    "component {i} has len {} but mean has dim {dim}",
                    c.len()
                )));
            }
        }
        if let Some(tm) = &text_mean
            && tm.len() != dim
        {
            return Err(Error::Whitening(format!(
                "text_mean has len {} but mean has dim {dim}",
                tm.len()
            )));
        }
        Ok(Self {
            mean,
            components,
            text_mean,
            dim,
        })
    }

    /// Attach (or replace) the cross-modal text mean, returning the
    /// updated transform. Rejects a dimensionality mismatch.
    pub fn with_text_mean(mut self, text_mean: Vec<f32>) -> Result<Self> {
        if text_mean.len() != self.dim {
            return Err(Error::Whitening(format!(
                "text_mean has len {} but dim is {}",
                text_mean.len(),
                self.dim
            )));
        }
        self.text_mean = Some(text_mean);
        Ok(self)
    }

    /// Fit ABTT over a corpus of equal-length raw vectors. Removes up to
    /// `k` top principal directions. Deterministic for a given corpus.
    pub fn fit(vectors: &[Vec<f32>], k: usize) -> Result<Self> {
        let n = vectors.len();
        if n == 0 {
            return Err(Error::Whitening("cannot fit on an empty corpus".into()));
        }
        let dim = vectors[0].len();
        if dim == 0 {
            return Err(Error::Whitening("vectors are zero-dimensional".into()));
        }
        for (i, v) in vectors.iter().enumerate() {
            if v.len() != dim {
                return Err(Error::Whitening(format!(
                    "vector {i} has len {} but expected {dim}",
                    v.len()
                )));
            }
        }

        // Mean accumulated in f64 to avoid drift over thousands of rows.
        let mut mean = vec![0.0_f64; dim];
        for v in vectors {
            for (m, &x) in mean.iter_mut().zip(v.iter()) {
                *m += x as f64;
            }
        }
        let n_f = n as f64;
        for m in &mut mean {
            *m /= n_f;
        }

        // Mutable centered copy (f64) — deflation rewrites it in place as
        // we strip each principal direction.
        let mut centered: Vec<Vec<f64>> = vectors
            .iter()
            .map(|v| {
                v.iter()
                    .zip(mean.iter())
                    .map(|(&x, &m)| x as f64 - m)
                    .collect()
            })
            .collect();

        // Top-k via power iteration on the covariance C = (1/n) Σ xxᵀ,
        // applied as data matvecs (O(n·d) per step, never forming C),
        // with Hotelling deflation between components.
        let effective_k = k.min(dim).min(n.saturating_sub(1));
        let mut rng = SmallRng::seed_from_u64(FIT_SEED);
        let mut components: Vec<Vec<f32>> = Vec::with_capacity(effective_k);
        for _ in 0..effective_k {
            let Some(u) = top_eigenvector(&centered, dim, &mut rng) else {
                break; // rank exhausted
            };
            deflate(&mut centered, &u);
            components.push(u.iter().map(|&x| x as f32).collect());
        }

        Ok(Self {
            mean: mean.iter().map(|&x| x as f32).collect(),
            components,
            text_mean: None,
            dim,
        })
    }

    /// Apply the transform: subtract the mean, project out each principal
    /// direction, and L2-renormalize so the result lives on the unit
    /// sphere for cosine search. A vector that collapses to ~0 (e.g. it
    /// lay entirely in the removed subspace) is returned un-normalized
    /// rather than divided by zero.
    pub fn transform(&self, raw: &[f32]) -> Result<Vec<f32>> {
        self.transform_with_center(raw, &self.mean)
    }

    /// Apply the transform to a text query, centering by the cross-modal
    /// `text_mean` (falling back to the audio mean if it hasn't been
    /// fitted). Used by the station path so text queries land in the same
    /// whitened space as the stored audio vectors.
    pub fn transform_text(&self, raw: &[f32]) -> Result<Vec<f32>> {
        let center = self.text_mean.as_deref().unwrap_or(&self.mean);
        self.transform_with_center(raw, center)
    }

    /// Subtract `center`, project out the principal directions, and
    /// L2-renormalize. A vector that collapses to ~0 (it lay entirely in
    /// the removed subspace) is returned un-normalized rather than divided
    /// by zero.
    fn transform_with_center(&self, raw: &[f32], center: &[f32]) -> Result<Vec<f32>> {
        if raw.len() != self.dim {
            return Err(Error::Whitening(format!(
                "transform input has len {} but dim is {}",
                raw.len(),
                self.dim
            )));
        }
        let mut c: Vec<f64> = raw
            .iter()
            .zip(center.iter())
            .map(|(&x, &m)| x as f64 - m as f64)
            .collect();
        for comp in &self.components {
            let dot: f64 = c.iter().zip(comp.iter()).map(|(&a, &b)| a * b as f64).sum();
            for (ci, &b) in c.iter_mut().zip(comp.iter()) {
                *ci -= dot * b as f64;
            }
        }
        let norm = c.iter().map(|&x| x * x).sum::<f64>().sqrt();
        let out = if norm > 0.0 {
            c.iter().map(|&x| (x / norm) as f32).collect()
        } else {
            c.iter().map(|&x| x as f32).collect()
        };
        Ok(out)
    }
}

/// One power-iteration solve for the dominant eigenvector of the
/// covariance of `centered`. Returns `None` if the residual is
/// rank-exhausted (dominant eigenvalue ≈ 0).
fn top_eigenvector(centered: &[Vec<f64>], dim: usize, rng: &mut SmallRng) -> Option<Vec<f64>> {
    let mut v: Vec<f64> = (0..dim).map(|_| rng.gen_range(-1.0_f64..1.0)).collect();
    normalize_in_place(&mut v)?;
    let mut prev_align = 0.0_f64;
    for _ in 0..MAX_POWER_ITERS {
        let mut cv = cov_matvec(centered, &v);
        let norm = cv.iter().map(|&x| x * x).sum::<f64>().sqrt();
        if norm < RANK_EPS {
            return None;
        }
        for x in &mut cv {
            *x /= norm;
        }
        // Alignment with the previous iterate; |·| because the sign can
        // oscillate without affecting the eigenvector (or the projection
        // that uses it).
        let align = cv
            .iter()
            .zip(v.iter())
            .map(|(&a, &b)| a * b)
            .sum::<f64>()
            .abs();
        v = cv;
        if (align - prev_align).abs() < POWER_CONVERGE_TOL && align > 1.0 - POWER_CONVERGE_TOL {
            break;
        }
        prev_align = align;
    }
    Some(v)
}

/// C·v where C = (1/n) Σ xᵢxᵢᵀ over the centered rows — computed as
/// (1/n) Σ xᵢ (xᵢᵀv), so we never materialize the d×d covariance.
fn cov_matvec(centered: &[Vec<f64>], v: &[f64]) -> Vec<f64> {
    let dim = v.len();
    let mut out = vec![0.0_f64; dim];
    for x in centered {
        let s: f64 = x.iter().zip(v.iter()).map(|(&a, &b)| a * b).sum();
        for (o, &xi) in out.iter_mut().zip(x.iter()) {
            *o += s * xi;
        }
    }
    let n = centered.len() as f64;
    for o in &mut out {
        *o /= n;
    }
    out
}

/// Hotelling deflation: strip the component along unit `u` from every row,
/// so the next power iteration finds the next-dominant direction.
fn deflate(centered: &mut [Vec<f64>], u: &[f64]) {
    for x in centered.iter_mut() {
        let p: f64 = x.iter().zip(u.iter()).map(|(&a, &b)| a * b).sum();
        for (xi, &ui) in x.iter_mut().zip(u.iter()) {
            *xi -= p * ui;
        }
    }
}

fn normalize_in_place(v: &mut [f64]) -> Option<()> {
    let norm = v.iter().map(|&x| x * x).sum::<f64>().sqrt();
    if norm < RANK_EPS {
        return None;
    }
    for x in v.iter_mut() {
        *x /= norm;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(&x, &y)| x * y).sum()
    }

    fn norm(v: &[f32]) -> f32 {
        v.iter().map(|&x| x * x).sum::<f32>().sqrt()
    }

    #[test]
    fn default_k_clamps_to_at_least_one() {
        assert_eq!(default_k(768), 7);
        assert_eq!(default_k(50), 1);
        assert_eq!(default_k(0), 1);
    }

    #[test]
    fn fit_recovers_the_corpus_mean() {
        // Symmetric around (1, -2, 0.5): mean must land there regardless
        // of the spread.
        let base = [1.0_f32, -2.0, 0.5];
        let vectors: Vec<Vec<f32>> = (0..50)
            .map(|i| {
                let s = (i as f32 - 24.5) * 0.1;
                vec![base[0] + s, base[1] - s, base[2] + 2.0 * s]
            })
            .collect();
        let w = Whitening::fit(&vectors, 1).unwrap();
        for (got, want) in w.mean().iter().zip(base.iter()) {
            assert!((got - want).abs() < 1e-4, "mean {got} != {want}");
        }
    }

    #[test]
    fn abtt_removes_the_planted_dominant_direction() {
        // Data = mean + a_i * e0 (big variance along e0) + tiny noise on
        // other axes. After removing 1 component, projections onto e0
        // must collapse to ~0.
        let dim = 8;
        let mut vectors = Vec::new();
        for i in 0..200 {
            let a = (i as f32 - 99.5) * 0.5; // wide spread along axis 0
            let mut v = vec![0.0_f32; dim];
            v[0] = 5.0 + a;
            // small structured wobble on other dims so they're not zero
            v[1] = 0.01 * ((i % 7) as f32 - 3.0);
            v[2] = 0.01 * ((i % 5) as f32 - 2.0);
            vectors.push(v);
        }
        let w = Whitening::fit(&vectors, 1).unwrap();
        assert_eq!(w.k(), 1);

        // The recovered direction should be ~axis 0.
        let comp0 = &w.components()[0];
        assert!(comp0[0].abs() > 0.99, "dominant dir not axis 0: {comp0:?}");

        // After transform, residual projection onto axis 0 (post-center)
        // is near zero for every vector.
        let e0 = {
            let mut e = vec![0.0_f32; dim];
            e[0] = 1.0;
            e
        };
        let mut max_proj = 0.0_f32;
        for v in &vectors {
            let t = w.transform(v).unwrap();
            max_proj = max_proj.max(dot(&t, &e0).abs());
        }
        // The de-coned, renormalized vectors are dominated by the noise
        // axes now; the axis-0 residual is small.
        assert!(max_proj < 0.2, "axis-0 residual too large: {max_proj}");
    }

    #[test]
    fn transform_output_is_unit_norm() {
        let vectors: Vec<Vec<f32>> = (0..30)
            .map(|i| vec![i as f32, (i * 2) as f32 + 1.0, -(i as f32)])
            .collect();
        let w = Whitening::fit(&vectors, 1).unwrap();
        for v in &vectors {
            let t = w.transform(v).unwrap();
            assert!((norm(&t) - 1.0).abs() < 1e-5, "not unit norm: {}", norm(&t));
        }
    }

    #[test]
    fn fit_is_deterministic() {
        let vectors: Vec<Vec<f32>> = (0..40)
            .map(|i| vec![(i % 3) as f32, (i % 5) as f32, (i % 7) as f32, 1.0])
            .collect();
        let a = Whitening::fit(&vectors, 2).unwrap();
        let b = Whitening::fit(&vectors, 2).unwrap();
        // Eigenvector sign can differ run-to-run in principle, but with a
        // fixed seed the whole fit is reproducible — and the *transform*
        // is sign-invariant regardless.
        let probe = vec![2.0_f32, 9.0, -1.0, 0.0];
        assert_eq!(a.transform(&probe).unwrap(), b.transform(&probe).unwrap());
    }

    #[test]
    fn transform_rejects_wrong_dim() {
        let vectors: Vec<Vec<f32>> = (0..10).map(|i| vec![i as f32, 1.0, 2.0]).collect();
        let w = Whitening::fit(&vectors, 1).unwrap();
        assert!(w.transform(&[1.0, 2.0]).is_err());
    }

    #[test]
    fn fit_rejects_empty_and_ragged() {
        assert!(Whitening::fit(&[], 1).is_err());
        let ragged = vec![vec![1.0_f32, 2.0], vec![1.0]];
        assert!(Whitening::fit(&ragged, 1).is_err());
    }

    #[test]
    fn from_parts_validates_component_shape() {
        assert!(Whitening::from_parts(vec![1.0, 2.0], vec![vec![1.0, 2.0]], None).is_ok());
        assert!(Whitening::from_parts(vec![1.0, 2.0], vec![vec![1.0]], None).is_err());
        assert!(Whitening::from_parts(vec![], vec![], None).is_err());
        // text_mean dimensionality is validated too.
        assert!(Whitening::from_parts(vec![1.0, 2.0], vec![], Some(vec![0.1, 0.2])).is_ok());
        assert!(Whitening::from_parts(vec![1.0, 2.0], vec![], Some(vec![0.1])).is_err());
    }

    #[test]
    fn transform_text_without_text_mean_equals_audio_transform() {
        let vectors: Vec<Vec<f32>> = (0..30)
            .map(|i| vec![i as f32, (i * 2) as f32 + 1.0, -(i as f32)])
            .collect();
        let w = Whitening::fit(&vectors, 1).unwrap();
        assert!(!w.has_text_mean());
        let probe = vec![3.0_f32, 9.0, -1.0];
        // No text mean fitted → text path mirrors the audio path.
        assert_eq!(
            w.transform_text(&probe).unwrap(),
            w.transform(&probe).unwrap()
        );
    }

    #[test]
    fn text_mean_decollapses_offset_queries() {
        // Model the real modality gap: audio variance lives on axis 0 (so
        // ABTT removes axis 0), while the text→audio offset lies on axis 2
        // — a direction with ~no audio variance, so de-coning does NOT
        // touch it. Centering a text query by the audio mean therefore
        // leaves that large shared offset dominant and all text queries
        // collapse together. Centering by the text mean (≈ the offset)
        // removes it, so the small per-query signal on axis 1 separates
        // them. This is exactly the live station regression + its fix.
        let audio: Vec<Vec<f32>> = (0..60)
            .map(|i| {
                let t = (i as f32) - 30.0;
                vec![0.5 * t, 0.01 * (((i % 3) as f32) - 1.0), 0.0, 0.0]
            })
            .collect();
        let w = Whitening::fit(&audio, 1).unwrap();

        // Two text queries: same big axis-2 offset, opposite axis-1 signal.
        let t1 = vec![0.0_f32, 1.0, 20.0, 0.0];
        let t2 = vec![0.0_f32, -1.0, 20.0, 0.0];

        // Without text mean: the shared axis-2 offset dominates → collapse.
        let a1 = w.transform_text(&t1).unwrap();
        let a2 = w.transform_text(&t2).unwrap();
        let cos_no_tm: f32 = a1.iter().zip(&a2).map(|(x, y)| x * y).sum();

        // With a text mean ≈ the shared offset, the queries separate.
        let w = w.with_text_mean(vec![0.0_f32, 0.0, 20.0, 0.0]).unwrap();
        let b1 = w.transform_text(&t1).unwrap();
        let b2 = w.transform_text(&t2).unwrap();
        let cos_tm: f32 = b1.iter().zip(&b2).map(|(x, y)| x * y).sum();

        assert!(
            cos_no_tm > 0.99,
            "expected collapse without text mean: {cos_no_tm}"
        );
        assert!(
            cos_tm < cos_no_tm - 0.5,
            "text mean should separate queries: no_tm={cos_no_tm} tm={cos_tm}"
        );
    }

    #[test]
    fn rank_deficient_corpus_yields_fewer_components() {
        // All rows identical → zero variance → no principal directions.
        let vectors = vec![vec![1.0_f32, 2.0, 3.0]; 20];
        let w = Whitening::fit(&vectors, 3).unwrap();
        assert_eq!(w.k(), 0, "constant corpus should produce no components");
        // Transform still works (centers to ~0, returned un-normalized).
        let t = w.transform(&[1.0, 2.0, 3.0]).unwrap();
        assert!(norm(&t) < 1e-5);
    }
}
