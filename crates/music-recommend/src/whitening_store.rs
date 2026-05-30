//! Persistence for the ABTT [`Whitening`] transform — one cached fit per
//! `model_version` in the recommend SQLite.
//!
//! Like the ANN, the transform is *derived* data (refittable from the raw
//! `track_embeddings` corpus), so this store is a cache, not a source of
//! truth: a missing or stale row is recoverable by refitting. It exists
//! so a gateway restart needn't refit from scratch, and so an explicit
//! refit is auditable via `(n_samples, fitted_at_ms)`.

use sqlx::{Row, SqlitePool};

use crate::store::{blob_to_vector, vector_to_blob};
use crate::types::ModelVersion;
use crate::whitening::Whitening;
use crate::{Error, Result};

#[derive(Clone, Debug)]
pub struct WhiteningStore {
    pool: SqlitePool,
}

impl WhiteningStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Load the cached transform for `model_version`, or `None` if none is
    /// stored yet (cold start — caller should fit + `upsert`).
    pub async fn get(&self, model_version: &ModelVersion) -> Result<Option<Whitening>> {
        let row = sqlx::query(
            "SELECT dim, k, mean, components, text_mean
             FROM embedding_whitening WHERE model_version = ?",
        )
        .bind(model_version.as_str())
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let dim: i64 = row.get("dim");
        let k: i64 = row.get("k");
        let mean_blob: Vec<u8> = row.get("mean");
        let comp_blob: Vec<u8> = row.get("components");
        let text_mean_blob: Option<Vec<u8>> = row.get("text_mean");

        let dim = usize::try_from(dim).map_err(|_| Error::Whitening("negative dim".into()))?;
        let k = usize::try_from(k).map_err(|_| Error::Whitening("negative k".into()))?;

        let mean = blob_to_vector(&mean_blob)?;
        if mean.len() != dim {
            return Err(Error::Whitening(format!(
                "stored mean len {} != dim {dim}",
                mean.len()
            )));
        }

        let flat = blob_to_vector(&comp_blob)?;
        if flat.len() != k * dim {
            return Err(Error::Whitening(format!(
                "stored components len {} != k*dim {}",
                flat.len(),
                k * dim
            )));
        }
        let components: Vec<Vec<f32>> = flat.chunks_exact(dim).map(<[f32]>::to_vec).collect();

        let text_mean = text_mean_blob.map(|b| blob_to_vector(&b)).transpose()?;

        Ok(Some(Whitening::from_parts(mean, components, text_mean)?))
    }

    /// Insert or replace the cached transform for `model_version`. The
    /// caller supplies `fitted_at_ms` (this crate avoids a wall clock so
    /// the store stays deterministic and easy to test).
    pub async fn upsert(
        &self,
        model_version: &ModelVersion,
        whitening: &Whitening,
        n_samples: usize,
        fitted_at_ms: i64,
    ) -> Result<()> {
        let dim = whitening.dim();
        let k = whitening.k();
        let mean_blob = vector_to_blob(whitening.mean());
        // Row-major flatten: k rows of `dim`, matching the `chunks_exact`
        // split on read.
        let mut comp_blob = Vec::with_capacity(k * dim * 4);
        for comp in whitening.components() {
            comp_blob.extend_from_slice(&vector_to_blob(comp));
        }
        let text_mean_blob: Option<Vec<u8>> = whitening.text_mean().map(vector_to_blob);

        sqlx::query(
            "INSERT INTO embedding_whitening
                 (model_version, dim, k, mean, components, n_samples, fitted_at_ms, text_mean)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(model_version) DO UPDATE SET
                 dim = excluded.dim,
                 k = excluded.k,
                 mean = excluded.mean,
                 components = excluded.components,
                 n_samples = excluded.n_samples,
                 fitted_at_ms = excluded.fitted_at_ms,
                 text_mean = excluded.text_mean",
        )
        .bind(model_version.as_str())
        .bind(i64::try_from(dim).map_err(|_| Error::Whitening("dim too large".into()))?)
        .bind(i64::try_from(k).map_err(|_| Error::Whitening("k too large".into()))?)
        .bind(mean_blob)
        .bind(comp_blob)
        .bind(i64::try_from(n_samples).map_err(|_| Error::Whitening("n_samples too large".into()))?)
        .bind(fitted_at_ms)
        .bind(text_mean_blob)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Fixtures build vectors from small integer indices; the f32 casts are
    // exact at these magnitudes.
    #![allow(clippy::cast_precision_loss)]
    use super::*;
    use crate::store::EmbeddingStore;

    async fn pool() -> SqlitePool {
        EmbeddingStore::open_in_memory()
            .await
            .expect("in-memory store")
            .pool()
            .clone()
    }

    fn sample_whitening() -> Whitening {
        let vectors: Vec<Vec<f32>> = (0..40)
            .map(|i| vec![i as f32, (i * 2) as f32 + 1.0, -(i as f32), 3.0])
            .collect();
        Whitening::fit(&vectors, 2).unwrap()
    }

    #[tokio::test]
    async fn get_returns_none_when_absent() {
        let store = WhiteningStore::new(pool().await);
        let got = store.get(&ModelVersion::from("nope")).await.unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn upsert_then_get_round_trips() {
        let store = WhiteningStore::new(pool().await);
        let mv = ModelVersion::from("clamp3-test");
        let w = sample_whitening();

        store.upsert(&mv, &w, 40, 1_700_000_000_000).await.unwrap();
        let got = store.get(&mv).await.unwrap().expect("present");

        assert_eq!(got.dim(), w.dim());
        assert_eq!(got.k(), w.k());
        // The transform must reproduce exactly — same mean + components.
        let probe = vec![5.0_f32, 11.0, -3.0, 3.0];
        assert_eq!(got.transform(&probe).unwrap(), w.transform(&probe).unwrap());
    }

    #[tokio::test]
    async fn text_mean_round_trips() {
        let store = WhiteningStore::new(pool().await);
        let mv = ModelVersion::from("clamp3-test");
        let w = sample_whitening()
            .with_text_mean(vec![0.5_f32, -0.25, 0.1, 0.0])
            .unwrap();
        assert!(w.has_text_mean());

        store.upsert(&mv, &w, 40, 1).await.unwrap();
        let got = store.get(&mv).await.unwrap().expect("present");

        assert!(got.has_text_mean());
        assert_eq!(got.text_mean(), w.text_mean());
        // A text query transforms identically after the round-trip.
        let probe = vec![1.0_f32, 2.0, 3.0, 4.0];
        assert_eq!(
            got.transform_text(&probe).unwrap(),
            w.transform_text(&probe).unwrap()
        );
    }

    #[tokio::test]
    async fn upsert_replaces_existing_row() {
        let store = WhiteningStore::new(pool().await);
        let mv = ModelVersion::from("clamp3-test");

        let w1 = Whitening::fit(&[vec![1.0_f32, 0.0], vec![-1.0, 0.0], vec![0.0, 0.0]], 1).unwrap();
        store.upsert(&mv, &w1, 3, 1).await.unwrap();

        let w2 = sample_whitening(); // different dim/shape
        store.upsert(&mv, &w2, 40, 2).await.unwrap();

        let got = store.get(&mv).await.unwrap().expect("present");
        assert_eq!(got.dim(), w2.dim());
    }
}
