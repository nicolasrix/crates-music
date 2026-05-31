//! Durable like/dislike store for rateable library entities — the user's
//! explicit taste. Covers tracks, albums, and artists (see [`RatedKind`]).
//!
//! Unlike [`crate::track_affinity`] (a decayed counter folding implicit
//! signal: plays, skips, recommendation thumbs), a rating here is an
//! explicit, *non-decaying* verdict on the entity itself:
//!
//!   * **like** (`+1`) — boosts the recommendation relevance of the
//!     entity's tracks and surfaces it on the "Liked" page. The boost is
//!     tiered by kind: a liked *track* contributes most, a liked *album*
//!     less, a liked *artist* least (see [`LIKE_BONUS`],
//!     [`LIKE_BONUS_ALBUM`], [`LIKE_BONUS_ARTIST`]); they stack additively.
//!   * **dislike** (`-1`) — excludes the entity from play entirely: every
//!     one of its tracks is hard-excluded from *all* recommender candidate
//!     generation, and the web player auto-skips them on queue advance.
//!   * **neutral** — no row; clearing a rating deletes it.
//!
//! This store is intentionally a separate channel from the decaying
//! affinity counter (see the doc comment on `0015_entity_rating.sql`): a
//! like must never fade, and folding it into the decaying counter would
//! double-count the thumb-up path in [`crate::feedback`]. The gateway owns
//! this store outright and never writes it back to Navidrome.
//!
//! Persisted in the same SQLite pool as the embedding store (the shared
//! `gateway-state.recommend.sqlite`); construct from
//! `EmbeddingStore::pool().clone()`.

use std::collections::{HashMap, HashSet};

use sqlx::{Row, SqlitePool};

use music_core::TrackId;

use crate::Result;

/// Default additive relevance bonus a *liked track* earns when rescoring
/// recommendations (`relevance = sim + … + LIKE_BONUS`). Tuned to the same
/// order as the preference weight — enough to pull a liked track in from
/// just outside the raw top-N without swamping acoustic similarity.
/// Overridable via the `[recommend] like_bonus` config knob.
pub const LIKE_BONUS: f32 = 0.15;

/// Default additive bonus a candidate earns for belonging to a *liked
/// album*. Lower than [`LIKE_BONUS`] so a directly-liked track always
/// outranks a same-album sibling, and `LIKE_BONUS_ALBUM + LIKE_BONUS_ARTIST`
/// stays below a single track like — the track > album > artist contribution
/// hierarchy. Overridable via `[recommend] like_bonus_album`.
pub const LIKE_BONUS_ALBUM: f32 = 0.06;

/// Default additive bonus a candidate earns for belonging to a *liked
/// artist*. The smallest of the three (artist is the broadest, least
/// specific signal). Overridable via `[recommend] like_bonus_artist`.
pub const LIKE_BONUS_ARTIST: f32 = 0.03;

/// What kind of library entity a rating applies to. The on-disk `kind`
/// column (and the API wire field) use the lowercase string forms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RatedKind {
    Track,
    Album,
    Artist,
}

impl RatedKind {
    /// The on-disk / wire string, matching the `CHECK (kind IN (…))`
    /// constraint in the migration.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RatedKind::Track => "track",
            RatedKind::Album => "album",
            RatedKind::Artist => "artist",
        }
    }

    /// Parse the on-disk / wire string back to a `RatedKind`. Returns
    /// `None` for anything the CHECK constraint forbids.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "track" => Some(RatedKind::Track),
            "album" => Some(RatedKind::Album),
            "artist" => Some(RatedKind::Artist),
            _ => None,
        }
    }
}

/// An explicit, durable verdict on a library entity. Stored as `+1` / `-1`;
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

    /// Set (or replace) an entity's rating. Upsert — re-rating overwrites
    /// the prior verdict and stamps `updated_ms`. `entity_id` is generic
    /// because album/artist ids are not `TrackId`; `kind` namespaces it.
    pub async fn set(
        &self,
        kind: RatedKind,
        entity_id: &str,
        rating: Rating,
        now_ms: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO entity_rating (kind, entity_id, rating, updated_ms)
                 VALUES (?, ?, ?, ?)
             ON CONFLICT(kind, entity_id) DO UPDATE SET
                 rating = excluded.rating,
                 updated_ms = excluded.updated_ms",
        )
        .bind(kind.as_str())
        .bind(entity_id)
        .bind(rating.as_i64())
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Clear an entity's rating (back to neutral). No-op if absent.
    pub async fn clear(&self, kind: RatedKind, entity_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM entity_rating WHERE kind = ? AND entity_id = ?")
            .bind(kind.as_str())
            .bind(entity_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The current rating for one entity, or `None` if neutral.
    pub async fn get(&self, kind: RatedKind, entity_id: &str) -> Result<Option<Rating>> {
        let row = sqlx::query("SELECT rating FROM entity_rating WHERE kind = ? AND entity_id = ?")
            .bind(kind.as_str())
            .bind(entity_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|r| Rating::from_i64(r.get::<i64, _>("rating"))))
    }

    /// All disliked entity ids of a given kind, as a set for O(1)
    /// exclusion membership in the recommend hot path. Single-user scale
    /// keeps this small; read once per recommend request.
    pub async fn disliked_ids(&self, kind: RatedKind) -> Result<HashSet<String>> {
        let rows =
            sqlx::query("SELECT entity_id FROM entity_rating WHERE kind = ? AND rating = -1")
                .bind(kind.as_str())
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|r| r.get::<String, _>("entity_id"))
            .collect())
    }

    /// All liked entity ids of a given kind, newest-rated first — the
    /// "Liked" page ordering (hydration of titles/art is the caller's job).
    pub async fn liked_ids(&self, kind: RatedKind) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT entity_id FROM entity_rating
             WHERE kind = ? AND rating = 1 ORDER BY updated_ms DESC",
        )
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| r.get::<String, _>("entity_id"))
            .collect())
    }

    /// Every rated entity with its kind + verdict — the
    /// `GET /v1/library/ratings` payload. Newest-rated first.
    pub async fn all(&self) -> Result<Vec<(RatedKind, String, Rating)>> {
        let rows = sqlx::query(
            "SELECT kind, entity_id, rating FROM entity_rating ORDER BY updated_ms DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let kind = RatedKind::parse(&r.get::<String, _>("kind"))?;
                let id = r.get::<String, _>("entity_id");
                let rt = Rating::from_i64(r.get::<i64, _>("rating"))?;
                Some((kind, id, rt))
            })
            .collect())
    }

    /// For a candidate track pool, the additive like-bonus each *liked
    /// track* earns (`bonus` per like; disliked/neutral tracks are absent).
    /// One `IN (…)` query, mirroring
    /// [`crate::track_affinity::TrackAffinityStore::affinity_many`]. Disliked
    /// tracks never reach scoring (they're excluded upstream), so this need
    /// only surface the likes. Album/artist bonuses are applied separately
    /// by the caller (it needs candidate metadata for the album/artist id).
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
            "SELECT entity_id FROM entity_rating
             WHERE kind = 'track' AND rating = 1 AND entity_id IN ({placeholders})"
        );
        let mut q = sqlx::query(&sql);
        for id in track_ids {
            q = q.bind(id.as_str());
        }
        let rows = q.fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|r| (TrackId::from(r.get::<String, _>("entity_id")), bonus))
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
        s.set(RatedKind::Track, "t1", Rating::Like, 1_000)
            .await
            .unwrap();
        assert_eq!(
            s.get(RatedKind::Track, "t1").await.unwrap(),
            Some(Rating::Like)
        );
    }

    #[tokio::test]
    async fn kinds_are_independent_namespaces() {
        // Same id string under different kinds is two distinct rows.
        let s = store().await;
        s.set(RatedKind::Track, "x", Rating::Like, 1_000)
            .await
            .unwrap();
        s.set(RatedKind::Album, "x", Rating::Dislike, 1_000)
            .await
            .unwrap();
        assert_eq!(
            s.get(RatedKind::Track, "x").await.unwrap(),
            Some(Rating::Like)
        );
        assert_eq!(
            s.get(RatedKind::Album, "x").await.unwrap(),
            Some(Rating::Dislike)
        );
        assert_eq!(s.get(RatedKind::Artist, "x").await.unwrap(), None);
    }

    #[tokio::test]
    async fn neutral_entity_has_no_rating() {
        let s = store().await;
        assert_eq!(s.get(RatedKind::Track, "never").await.unwrap(), None);
    }

    #[tokio::test]
    async fn re_rating_overwrites() {
        let s = store().await;
        s.set(RatedKind::Album, "a1", Rating::Like, 1_000)
            .await
            .unwrap();
        s.set(RatedKind::Album, "a1", Rating::Dislike, 2_000)
            .await
            .unwrap();
        assert_eq!(
            s.get(RatedKind::Album, "a1").await.unwrap(),
            Some(Rating::Dislike)
        );
    }

    #[tokio::test]
    async fn clear_returns_to_neutral() {
        let s = store().await;
        s.set(RatedKind::Artist, "ar1", Rating::Like, 1_000)
            .await
            .unwrap();
        s.clear(RatedKind::Artist, "ar1").await.unwrap();
        assert_eq!(s.get(RatedKind::Artist, "ar1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn clear_absent_is_noop() {
        let s = store().await;
        s.clear(RatedKind::Track, "never").await.unwrap();
        assert_eq!(s.get(RatedKind::Track, "never").await.unwrap(), None);
    }

    #[tokio::test]
    async fn disliked_ids_only_returns_dislikes_of_kind() {
        let s = store().await;
        s.set(RatedKind::Album, "liked", Rating::Like, 1_000)
            .await
            .unwrap();
        s.set(RatedKind::Album, "hated", Rating::Dislike, 1_000)
            .await
            .unwrap();
        // A disliked artist must NOT bleed into the album set.
        s.set(RatedKind::Artist, "hated-artist", Rating::Dislike, 1_000)
            .await
            .unwrap();
        let disliked = s.disliked_ids(RatedKind::Album).await.unwrap();
        assert!(disliked.contains("hated"));
        assert!(!disliked.contains("liked"));
        assert!(!disliked.contains("hated-artist"));
        assert_eq!(disliked.len(), 1);
    }

    #[tokio::test]
    async fn liked_ids_newest_first_per_kind() {
        let s = store().await;
        s.set(RatedKind::Artist, "old", Rating::Like, 1_000)
            .await
            .unwrap();
        s.set(RatedKind::Artist, "new", Rating::Like, 2_000)
            .await
            .unwrap();
        s.set(RatedKind::Artist, "hated", Rating::Dislike, 3_000)
            .await
            .unwrap();
        assert_eq!(
            s.liked_ids(RatedKind::Artist).await.unwrap(),
            vec!["new".to_string(), "old".to_string()]
        );
    }

    #[tokio::test]
    async fn all_reports_every_rating_newest_first() {
        let s = store().await;
        s.set(RatedKind::Track, "a", Rating::Like, 1_000)
            .await
            .unwrap();
        s.set(RatedKind::Album, "b", Rating::Dislike, 2_000)
            .await
            .unwrap();
        assert_eq!(
            s.all().await.unwrap(),
            vec![
                (RatedKind::Album, "b".to_string(), Rating::Dislike),
                (RatedKind::Track, "a".to_string(), Rating::Like)
            ]
        );
    }

    #[tokio::test]
    async fn liked_bonus_only_for_liked_tracks() {
        let s = store().await;
        s.set(RatedKind::Track, "liked", Rating::Like, 1_000)
            .await
            .unwrap();
        s.set(RatedKind::Track, "hated", Rating::Dislike, 1_000)
            .await
            .unwrap();
        // A liked album with the same id must NOT count as a track like.
        s.set(RatedKind::Album, "liked", Rating::Like, 1_000)
            .await
            .unwrap();
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

    #[tokio::test]
    async fn kind_roundtrips_through_str() {
        for k in [RatedKind::Track, RatedKind::Album, RatedKind::Artist] {
            assert_eq!(RatedKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(RatedKind::parse("playlist"), None);
    }

    /// Migration 0015 must carry the live 0014 `track_rating` rows forward
    /// into `entity_rating` as `kind='track'` before dropping the old table
    /// — the production cutover relies on no verdict being lost. We exercise
    /// the two migration files directly (legacy schema seeded with data,
    /// then 0015 applied) because the embedded migrator runs over an empty
    /// `track_rating` in the other tests, so the row-copy path is otherwise
    /// never hit.
    #[tokio::test]
    async fn migration_0015_carries_track_rating_rows_forward() {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();

        // Legacy 0014 table + two live-shaped rows (a like and a dislike).
        sqlx::raw_sql(include_str!("../migrations/0014_track_rating.sql"))
            .execute(&pool)
            .await
            .unwrap();
        for (id, rating) in [("liked-song", 1_i64), ("hated-song", -1_i64)] {
            sqlx::query("INSERT INTO track_rating (track_id, rating, updated_ms) VALUES (?, ?, ?)")
                .bind(id)
                .bind(rating)
                .bind(1_000_i64)
                .execute(&pool)
                .await
                .unwrap();
        }

        // Apply 0015: creates entity_rating, copies rows as kind='track',
        // drops track_rating.
        sqlx::raw_sql(include_str!("../migrations/0015_entity_rating.sql"))
            .execute(&pool)
            .await
            .unwrap();

        let store = RatingStore::new(pool.clone());
        assert_eq!(
            store.get(RatedKind::Track, "liked-song").await.unwrap(),
            Some(Rating::Like)
        );
        assert_eq!(
            store.get(RatedKind::Track, "hated-song").await.unwrap(),
            Some(Rating::Dislike)
        );

        // The old table is gone.
        let legacy_exists =
            sqlx::query("SELECT name FROM sqlite_master WHERE type='table' AND name='track_rating'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(legacy_exists.is_none(), "track_rating should be dropped");
    }
}
