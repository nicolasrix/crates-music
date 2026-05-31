//! Durable per-track like/dislike store — the user's explicit taste.
//!
//! Unlike [`crate::track_affinity`] (a decayed counter folding implicit
//! signal: plays, skips, recommendation thumbs), a rating here is an
//! explicit, *non-decaying* verdict on the song itself:
//!
//!   * **like** (`+1`) — boosts the track's recommendation relevance and
//!     surfaces it on the "Liked songs" page.
//!   * **dislike** (`-1`) — hard-excludes the track from *all* recommender
//!     candidate generation, and drives the web player's auto-skip.
//!   * **neutral** — no row; clearing a rating deletes it.
//!
//! These two stores are intentionally separate channels (see the doc
//! comment on `0014_track_rating.sql`): a like must never fade, and
//! folding it into the decaying counter would double-count the thumb-up
//! path in [`crate::feedback`]. The gateway owns this store outright and
//! never writes it back to Navidrome.
//!
//! Persisted in the same SQLite pool as the embedding store (the shared
//! `gateway-state.recommend.sqlite`); construct from
//! `EmbeddingStore::pool().clone()`.

use std::collections::{HashMap, HashSet};

use sqlx::{Row, SqlitePool};

use music_core::TrackId;

use crate::Result;

/// Default additive relevance bonus applied to a liked candidate when
/// rescoring recommendations (`relevance = sim + … + LIKE_BONUS`). Tuned
/// to the same order as the preference weight — enough to pull a liked
/// track in from just outside the raw top-N without swamping acoustic
/// similarity. Overridable via the `[recommend] like_bonus` config knob.
pub const LIKE_BONUS: f32 = 0.15;

/// An explicit, durable per-track verdict. Stored as `+1` / `-1`;
/// neutral is the absence of a row, so it has no variant here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rating {
    Like,
    Dislike,
}

impl Rating {
    /// The on-disk integer (`+1` like / `-1` dislike), matching the
    /// `CHECK (rating IN (-1, 1))` constraint in the migration.
    #[must_use]
    pub fn as_i64(self) -> i64 {
        match self {
            Rating::Like => 1,
            Rating::Dislike => -1,
        }
    }

    /// Parse the on-disk integer back to a `Rating`. Returns `None` for
    /// anything other than `±1` — which the CHECK constraint forbids, so
    /// this only guards against a corrupt row.
    #[must_use]
    pub fn from_i64(value: i64) -> Option<Self> {
        match value {
            1 => Some(Rating::Like),
            -1 => Some(Rating::Dislike),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RatingStore {
    pool: SqlitePool,
}

impl RatingStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Set (or replace) a track's rating. Upsert — re-rating a track
    /// overwrites the prior verdict and stamps `updated_ms`.
    pub async fn set(&self, track_id: &TrackId, rating: Rating, now_ms: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO track_rating (track_id, rating, updated_ms)
                 VALUES (?, ?, ?)
             ON CONFLICT(track_id) DO UPDATE SET
                 rating = excluded.rating,
                 updated_ms = excluded.updated_ms",
        )
        .bind(track_id.as_str())
        .bind(rating.as_i64())
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Clear a track's rating (back to neutral). No-op if absent.
    pub async fn clear(&self, track_id: &TrackId) -> Result<()> {
        sqlx::query("DELETE FROM track_rating WHERE track_id = ?")
            .bind(track_id.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The current rating for one track, or `None` if neutral.
    pub async fn get(&self, track_id: &TrackId) -> Result<Option<Rating>> {
        let row = sqlx::query("SELECT rating FROM track_rating WHERE track_id = ?")
            .bind(track_id.as_str())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| Rating::from_i64(r.get::<i64, _>("rating"))))
    }

    /// All disliked track ids, as a set for O(1) exclusion membership in
    /// the recommend hot path. Single-user scale keeps this small; read
    /// once per recommend request.
    pub async fn disliked_ids(&self) -> Result<HashSet<TrackId>> {
        let rows = sqlx::query("SELECT track_id FROM track_rating WHERE rating = -1")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| TrackId::from(r.get::<String, _>("track_id")))
            .collect())
    }

    /// All liked track ids, newest-rated first — the "Liked songs" page
    /// ordering (hydration of titles/art is the caller's job).
    pub async fn liked_ids(&self) -> Result<Vec<TrackId>> {
        let rows = sqlx::query(
            "SELECT track_id FROM track_rating WHERE rating = 1 ORDER BY updated_ms DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| TrackId::from(r.get::<String, _>("track_id")))
            .collect())
    }

    /// Every rated track with its verdict — the `GET /v1/library/ratings`
    /// payload. Newest-rated first.
    pub async fn all(&self) -> Result<Vec<(TrackId, Rating)>> {
        let rows =
            sqlx::query("SELECT track_id, rating FROM track_rating ORDER BY updated_ms DESC")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let tid = TrackId::from(r.get::<String, _>("track_id"));
                Rating::from_i64(r.get::<i64, _>("rating")).map(|rt| (tid, rt))
            })
            .collect())
    }

    /// For a candidate pool, the additive like-bonus each liked candidate
    /// earns (`bonus` per like; disliked/neutral tracks are absent). One
    /// `IN (…)` query, mirroring [`crate::track_affinity::TrackAffinityStore::affinity_many`].
    /// Disliked tracks never reach scoring (they're excluded upstream), so
    /// this need only surface the likes.
    pub async fn liked_bonus_many(
        &self,
        track_ids: &[TrackId],
        bonus: f32,
    ) -> Result<HashMap<TrackId, f32>> {
        if track_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = std::iter::repeat_n("?", track_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT track_id FROM track_rating
             WHERE rating = 1 AND track_id IN ({placeholders})"
        );
        let mut q = sqlx::query(&sql);
        for id in track_ids {
            q = q.bind(id.as_str());
        }
        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|r| (TrackId::from(r.get::<String, _>("track_id")), bonus))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::EmbeddingStore;

    async fn store() -> RatingStore {
        let embed = EmbeddingStore::open_in_memory().await.unwrap();
        RatingStore::new(embed.pool().clone())
    }

    fn tid(s: &str) -> TrackId {
        TrackId::from(s.to_string())
    }

    #[tokio::test]
    async fn set_get_roundtrip() {
        let s = store().await;
        s.set(&tid("t1"), Rating::Like, 1_000).await.unwrap();
        assert_eq!(s.get(&tid("t1")).await.unwrap(), Some(Rating::Like));
    }

    #[tokio::test]
    async fn neutral_track_has_no_rating() {
        let s = store().await;
        assert_eq!(s.get(&tid("never")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn re_rating_overwrites() {
        let s = store().await;
        s.set(&tid("t1"), Rating::Like, 1_000).await.unwrap();
        s.set(&tid("t1"), Rating::Dislike, 2_000).await.unwrap();
        assert_eq!(s.get(&tid("t1")).await.unwrap(), Some(Rating::Dislike));
    }

    #[tokio::test]
    async fn clear_returns_to_neutral() {
        let s = store().await;
        s.set(&tid("t1"), Rating::Like, 1_000).await.unwrap();
        s.clear(&tid("t1")).await.unwrap();
        assert_eq!(s.get(&tid("t1")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn clear_absent_is_noop() {
        let s = store().await;
        s.clear(&tid("never")).await.unwrap();
        assert_eq!(s.get(&tid("never")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn disliked_ids_only_returns_dislikes() {
        let s = store().await;
        s.set(&tid("liked"), Rating::Like, 1_000).await.unwrap();
        s.set(&tid("hated"), Rating::Dislike, 1_000).await.unwrap();
        let disliked = s.disliked_ids().await.unwrap();
        assert!(disliked.contains(&tid("hated")));
        assert!(!disliked.contains(&tid("liked")));
        assert_eq!(disliked.len(), 1);
    }

    #[tokio::test]
    async fn liked_ids_newest_first() {
        let s = store().await;
        s.set(&tid("old"), Rating::Like, 1_000).await.unwrap();
        s.set(&tid("new"), Rating::Like, 2_000).await.unwrap();
        s.set(&tid("hated"), Rating::Dislike, 3_000).await.unwrap();
        assert_eq!(s.liked_ids().await.unwrap(), vec![tid("new"), tid("old")]);
    }

    #[tokio::test]
    async fn all_reports_every_rating_newest_first() {
        let s = store().await;
        s.set(&tid("a"), Rating::Like, 1_000).await.unwrap();
        s.set(&tid("b"), Rating::Dislike, 2_000).await.unwrap();
        assert_eq!(
            s.all().await.unwrap(),
            vec![(tid("b"), Rating::Dislike), (tid("a"), Rating::Like)]
        );
    }

    #[tokio::test]
    async fn liked_bonus_only_for_liked_candidates() {
        let s = store().await;
        s.set(&tid("liked"), Rating::Like, 1_000).await.unwrap();
        s.set(&tid("hated"), Rating::Dislike, 1_000).await.unwrap();
        let bonus = s
            .liked_bonus_many(&[tid("liked"), tid("hated"), tid("neutral")], 0.15)
            .await
            .unwrap();
        assert_eq!(bonus.get(&tid("liked")), Some(&0.15));
        assert!(!bonus.contains_key(&tid("hated")));
        assert!(!bonus.contains_key(&tid("neutral")));
    }

    #[tokio::test]
    async fn liked_bonus_empty_batch() {
        let s = store().await;
        assert!(s.liked_bonus_many(&[], 0.15).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rating_enum_roundtrips_through_i64() {
        assert_eq!(Rating::from_i64(Rating::Like.as_i64()), Some(Rating::Like));
        assert_eq!(
            Rating::from_i64(Rating::Dislike.as_i64()),
            Some(Rating::Dislike)
        );
        assert_eq!(Rating::from_i64(0), None);
    }
}
