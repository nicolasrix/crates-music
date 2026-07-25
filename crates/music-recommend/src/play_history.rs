//! Per-track recency clock.
//!
//! Maintains the most-recent submitted scrobble timestamp for each
//! track id, so the MMR recency penalty (B1) can do O(1) "was this
//! played recently?" lookups during candidate scoring.
//!
//! Source of truth for *displayed* play counts remains Navidrome
//! (Subsonic's `playCount` / `played` fields). This table is a
//! recommender-internal index — it does not need to be reconciled
//! with anything; if it drifts, the worst that happens is the recency
//! filter is slightly stale, and the next scrobble heals it.

use sqlx::{Row, SqlitePool};

use music_core::TrackId;

use crate::Result;

#[derive(Clone, Debug)]
pub struct PlayHistoryStore {
    pool: SqlitePool,
}

impl PlayHistoryStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Record a submission scrobble. Idempotent and order-tolerant:
    /// MAX-merge on the timestamp prevents an out-of-order retry from
    /// dragging the recency clock backwards. `played_at_ms` is the
    /// wall-clock millisecond timestamp the client recorded for the
    /// listen — we trust it; the recommender doesn't care about
    /// sub-minute precision.
    pub async fn record_submission(
        &self,
        user_id: i64,
        track_id: &TrackId,
        played_at_ms: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO play_history (user_id, track_id, last_played_ms)
             VALUES (?, ?, ?)
             ON CONFLICT(user_id, track_id) DO UPDATE
                 SET last_played_ms = MAX(last_played_ms, excluded.last_played_ms)",
        )
        .bind(user_id)
        .bind(track_id.as_str())
        .bind(played_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Most recent submission timestamp for the given track, or
    /// `None` if it has never been scrobbled (or the row was lost in
    /// a SQLite restore — degraded mode is "no recency penalty for
    /// this track," which is the same as "we don't know," so the
    /// caller can treat the two cases interchangeably).
    pub async fn last_played(&self, user_id: i64, track_id: &TrackId) -> Result<Option<i64>> {
        let row = sqlx::query(
            "SELECT last_played_ms FROM play_history WHERE user_id = ? AND track_id = ?",
        )
        .bind(user_id)
        .bind(track_id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get::<i64, _>("last_played_ms")))
    }

    /// Track ids whose most-recent submission is at or after `since_ms`,
    /// for the given user. The autoplay recency exclusion: the recommender
    /// hard-excludes these from candidate generation so a song the listener
    /// heard minutes ago doesn't resurface in the next refill. A small
    /// listening window yields a handful of ids — cheap to fold into the
    /// exclude set. Empty vec when nothing was played in the window.
    pub async fn played_since(&self, user_id: i64, since_ms: i64) -> Result<Vec<TrackId>> {
        let rows = sqlx::query(
            "SELECT track_id FROM play_history
             WHERE user_id = ? AND last_played_ms >= ?",
        )
        .bind(user_id)
        .bind(since_ms)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| TrackId::from(r.get::<String, _>("track_id")))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::EmbeddingStore;

    async fn store() -> PlayHistoryStore {
        let embed = EmbeddingStore::open_in_memory().await.unwrap();
        PlayHistoryStore::new(embed.pool().clone())
    }

    fn tid(s: &str) -> TrackId {
        TrackId::from(s.to_string())
    }

    #[tokio::test]
    async fn last_played_returns_none_for_unknown_track() {
        let s = store().await;
        assert_eq!(s.last_played(1, &tid("never")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn record_then_lookup_round_trips() {
        let s = store().await;
        s.record_submission(1, &tid("t1"), 1_700_000_000_000)
            .await
            .unwrap();
        assert_eq!(
            s.last_played(1, &tid("t1")).await.unwrap(),
            Some(1_700_000_000_000)
        );
    }

    #[tokio::test]
    async fn newer_scrobble_overwrites_older() {
        let s = store().await;
        s.record_submission(1, &tid("t1"), 1_000).await.unwrap();
        s.record_submission(1, &tid("t1"), 2_000).await.unwrap();
        assert_eq!(s.last_played(1, &tid("t1")).await.unwrap(), Some(2_000));
    }

    #[tokio::test]
    async fn out_of_order_older_does_not_overwrite_newer() {
        // Offline batch retry case: client sends an older scrobble
        // after we've already recorded a newer one. The recency clock
        // must not move backwards — MMR's recency penalty would then
        // start re-recommending tracks the user just heard.
        let s = store().await;
        s.record_submission(1, &tid("t1"), 2_000).await.unwrap();
        s.record_submission(1, &tid("t1"), 1_000).await.unwrap();
        assert_eq!(s.last_played(1, &tid("t1")).await.unwrap(), Some(2_000));
    }

    #[tokio::test]
    async fn equal_timestamp_is_a_noop() {
        let s = store().await;
        s.record_submission(1, &tid("t1"), 1_000).await.unwrap();
        s.record_submission(1, &tid("t1"), 1_000).await.unwrap();
        assert_eq!(s.last_played(1, &tid("t1")).await.unwrap(), Some(1_000));
    }

    #[tokio::test]
    async fn played_since_returns_only_window_and_respects_user() {
        let s = store().await;
        // user 1: t-old (before window), t-new (in window)
        s.record_submission(1, &tid("t-old"), 1_000).await.unwrap();
        s.record_submission(1, &tid("t-new"), 5_000).await.unwrap();
        // user 2: t-new in window — must not leak into user 1's result
        s.record_submission(2, &tid("t-other"), 6_000)
            .await
            .unwrap();

        let mut got = s.played_since(1, 4_000).await.unwrap();
        got.sort();
        assert_eq!(got, vec![tid("t-new")]);

        // Boundary is inclusive (>= since_ms).
        let at_boundary = s.played_since(1, 5_000).await.unwrap();
        assert_eq!(at_boundary, vec![tid("t-new")]);

        // Nothing recent enough → empty.
        assert!(s.played_since(1, 9_000).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn separate_tracks_isolated() {
        let s = store().await;
        s.record_submission(1, &tid("t1"), 1_000).await.unwrap();
        s.record_submission(1, &tid("t2"), 2_000).await.unwrap();
        assert_eq!(s.last_played(1, &tid("t1")).await.unwrap(), Some(1_000));
        assert_eq!(s.last_played(1, &tid("t2")).await.unwrap(), Some(2_000));
        assert_eq!(s.last_played(1, &tid("t3")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn users_have_independent_recency_clocks() {
        // Same track, two users, different play times: each user's clock
        // is private — user 2's play must not move user 1's, and a track
        // user 1 never played is `None` for them even though user 2 did.
        let s = store().await;
        s.record_submission(1, &tid("shared"), 1_000).await.unwrap();
        s.record_submission(2, &tid("shared"), 9_000).await.unwrap();
        s.record_submission(2, &tid("only-u2"), 5_000)
            .await
            .unwrap();
        assert_eq!(s.last_played(1, &tid("shared")).await.unwrap(), Some(1_000));
        assert_eq!(s.last_played(2, &tid("shared")).await.unwrap(), Some(9_000));
        assert_eq!(s.last_played(1, &tid("only-u2")).await.unwrap(), None);
        assert_eq!(s.last_played(2, &tid("only-u2")).await.unwrap(), Some(5_000));
    }
}
