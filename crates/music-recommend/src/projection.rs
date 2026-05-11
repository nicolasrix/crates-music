//! 2D UMAP-projected embeddings, persisted by `embedder.reduce`.
//!
//! This store is a read-side companion to the Python reducer in
//! `services/embedder/embedder/reduce.py`. The Python side writes;
//! the gateway reads. We intentionally don't expose a write API in
//! Rust — projections are *derived* data, recomputable by re-running
//! the reducer, and keeping writes single-language avoids the
//! cross-language coordination headache (e.g. concurrent UMAP runs
//! racing on the same `(track_id, model_version, proj_version)` PK).
//!
//! Two reads matter:
//! - `list_by_proj_version(...)` — the diagnostics page hot path;
//!   pulls all `(track_id, x, y)` points for the requested projection.
//! - `proj_versions_for_model(model_version)` — small enumeration so
//!   the diagnostics UI can offer a dropdown without round-tripping
//!   through the Python side.

use sqlx::{Row, SqlitePool};

use crate::Result;
use crate::types::ModelVersion;

#[derive(Clone, Debug)]
pub struct ProjectionStore {
    pool: SqlitePool,
}

/// One point in the 2-D latent-space scatter.
#[derive(Clone, Debug, PartialEq)]
pub struct Projection2D {
    pub track_id: String,
    pub x: f64,
    pub y: f64,
}

/// One entry in the projection-version catalogue: which `proj_version`
/// strings exist for a given `model_version`, with how many points and
/// when they were last written.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectionVersionSummary {
    pub proj_version: String,
    pub point_count: i64,
    pub created_at_ms: i64,
}

impl ProjectionStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// All points for `(proj_version, model_version)`, ordered by
    /// `track_id` for stable iteration. Returns an empty vec rather
    /// than `None` so the caller doesn't have to special-case "no
    /// projection has been written yet".
    pub async fn list_by_proj_version(
        &self,
        proj_version: &str,
        model_version: &ModelVersion,
    ) -> Result<Vec<Projection2D>> {
        let rows = sqlx::query(
            "SELECT track_id, x, y
               FROM embedding_projection_2d
              WHERE proj_version = ? AND model_version = ?
              ORDER BY track_id",
        )
        .bind(proj_version)
        .bind(model_version.as_str())
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| Projection2D {
                track_id: r.get::<String, _>("track_id"),
                x: r.get::<f64, _>("x"),
                y: r.get::<f64, _>("y"),
            })
            .collect())
    }

    /// Distinct `proj_version` values known for the given
    /// `model_version`, with their point count and the *latest*
    /// `created_at_ms` across the rows. Used by the diagnostics UI to
    /// populate the proj-version dropdown, newest projection first.
    pub async fn proj_versions_for_model(
        &self,
        model_version: &ModelVersion,
    ) -> Result<Vec<ProjectionVersionSummary>> {
        let rows = sqlx::query(
            "SELECT proj_version,
                    COUNT(*)        AS point_count,
                    MAX(created_at_ms) AS created_at_ms
               FROM embedding_projection_2d
              WHERE model_version = ?
              GROUP BY proj_version
              ORDER BY MAX(created_at_ms) DESC",
        )
        .bind(model_version.as_str())
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| ProjectionVersionSummary {
                proj_version: r.get::<String, _>("proj_version"),
                point_count: r.get::<i64, _>("point_count"),
                created_at_ms: r.get::<i64, _>("created_at_ms"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::EmbeddingStore;

    async fn store() -> (ProjectionStore, SqlitePool) {
        let embed = EmbeddingStore::open_in_memory().await.unwrap();
        let pool = embed.pool().clone();
        (ProjectionStore::new(pool.clone()), pool)
    }

    async fn insert_point(
        pool: &SqlitePool,
        track_id: &str,
        model_version: &str,
        proj_version: &str,
        x: f64,
        y: f64,
        created_at_ms: i64,
    ) {
        sqlx::query(
            "INSERT INTO embedding_projection_2d
                 (track_id, model_version, proj_version, x, y, created_at_ms)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(track_id)
        .bind(model_version)
        .bind(proj_version)
        .bind(x)
        .bind(y)
        .bind(created_at_ms)
        .execute(pool)
        .await
        .unwrap();
    }

    fn mv(s: &str) -> ModelVersion {
        ModelVersion::from(s.to_string())
    }

    #[tokio::test]
    async fn list_by_proj_version_returns_empty_when_no_rows() {
        let (s, _) = store().await;
        let got = s.list_by_proj_version("nope", &mv("m1")).await.unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn list_by_proj_version_returns_only_matching_proj_and_model() {
        let (s, pool) = store().await;
        // Target row.
        insert_point(&pool, "t1", "m1", "pv1", 1.0, 2.0, 100).await;
        // Different proj_version — filtered out.
        insert_point(&pool, "t1", "m1", "pv2", 9.0, 9.0, 100).await;
        // Different model — filtered out.
        insert_point(&pool, "t1", "m2", "pv1", 8.0, 8.0, 100).await;

        let got = s.list_by_proj_version("pv1", &mv("m1")).await.unwrap();
        assert_eq!(
            got,
            vec![Projection2D {
                track_id: "t1".into(),
                x: 1.0,
                y: 2.0,
            }]
        );
    }

    #[tokio::test]
    async fn list_by_proj_version_is_ordered_by_track_id() {
        let (s, pool) = store().await;
        // Insert out of order.
        for (tid, x) in [("tc", 3.0), ("ta", 1.0), ("tb", 2.0)] {
            insert_point(&pool, tid, "m1", "pv1", x, 0.0, 100).await;
        }
        let got = s.list_by_proj_version("pv1", &mv("m1")).await.unwrap();
        let ids: Vec<_> = got.iter().map(|p| p.track_id.as_str()).collect();
        assert_eq!(ids, vec!["ta", "tb", "tc"]);
    }

    #[tokio::test]
    async fn proj_versions_for_model_returns_empty_when_no_rows() {
        let (s, _) = store().await;
        let got = s.proj_versions_for_model(&mv("m1")).await.unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn proj_versions_for_model_groups_and_counts() {
        let (s, pool) = store().await;
        // pv1: two points
        insert_point(&pool, "t1", "m1", "pv1", 0.0, 0.0, 100).await;
        insert_point(&pool, "t2", "m1", "pv1", 0.0, 0.0, 200).await;
        // pv2: one point
        insert_point(&pool, "t1", "m1", "pv2", 0.0, 0.0, 300).await;
        // Different model — must be absent from m1's enumeration.
        insert_point(&pool, "t1", "m2", "pv1", 0.0, 0.0, 999).await;

        let got = s.proj_versions_for_model(&mv("m1")).await.unwrap();
        // Sorted by MAX(created_at_ms) DESC → pv2 (300), pv1 (200).
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].proj_version, "pv2");
        assert_eq!(got[0].point_count, 1);
        assert_eq!(got[0].created_at_ms, 300);
        assert_eq!(got[1].proj_version, "pv1");
        assert_eq!(got[1].point_count, 2);
        assert_eq!(got[1].created_at_ms, 200);
    }
}
