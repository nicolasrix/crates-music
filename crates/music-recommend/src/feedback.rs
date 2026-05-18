//! Per-(track, session) recommendation feedback.
//!
//! Records explicit user signal on whether a *recommended* track was a
//! good fit for the listening moment — distinct from Subsonic's
//! `starred`, which is a track-level preference. The recommender does
//! not consume this yet (single-tenant, recently-shipped); this module
//! is the durable capture channel so future training has signal to
//! pull.
//!
//! Semantics:
//! - `record(track_id, session_id, +1|-1)` UPSERTs. The same session
//!   can flip its vote freely.
//! - `clear(track_id, session_id)` removes the row. Pressing an
//!   already-active thumb undoes the vote in the UI; we lower that
//!   to a DELETE rather than recording a "neutral" sentinel.
//! - `for_track(track_id)` returns aggregated counts for the response
//!   body of the POST endpoint.
//! - `aggregate(since_ms, limit)` returns per-track counts joined by
//!   the caller against metadata; used by the diagnostics endpoint.

use sqlx::{Row, SqlitePool};

use music_core::TrackId;

use crate::Result;

#[derive(Clone, Debug)]
pub struct FeedbackStore {
    pool: SqlitePool,
}

/// Aggregated up/down counts for a single track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackCounts {
    pub up: i64,
    pub down: i64,
}

/// A row in the diagnostics "leaderboard" view — one per track that
/// has received at least one vote in the window. `last_voted_ms` is
/// the gateway-stamped `received_ms`, used to sort recent feedback
/// without trusting client clocks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackAggregate {
    pub track_id: String,
    pub up: i64,
    pub down: i64,
    pub last_voted_ms: i64,
}

impl FeedbackStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert or replace a vote. The `(track_id, session_id)` pair is
    /// unique, so a second call with a different `vote` flips the
    /// stored value without creating a duplicate row.
    pub async fn record(
        &self,
        track_id: &TrackId,
        session_id: &str,
        vote: i8,
        occurred_ms: i64,
        received_ms: i64,
    ) -> Result<()> {
        debug_assert!(vote == 1 || vote == -1, "vote must be ±1, got {vote}");
        sqlx::query(
            "INSERT INTO recommend_feedback
                 (track_id, session_id, vote, occurred_ms, received_ms)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(track_id, session_id) DO UPDATE SET
                 vote = excluded.vote,
                 occurred_ms = excluded.occurred_ms,
                 received_ms = excluded.received_ms",
        )
        .bind(track_id.as_str())
        .bind(session_id)
        .bind(i64::from(vote))
        .bind(occurred_ms)
        .bind(received_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Drop the (track_id, session_id) vote, if any. Idempotent —
    /// no error when there's nothing to remove.
    pub async fn clear(&self, track_id: &TrackId, session_id: &str) -> Result<()> {
        sqlx::query(
            "DELETE FROM recommend_feedback
             WHERE track_id = ? AND session_id = ?",
        )
        .bind(track_id.as_str())
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Aggregate up/down counts for a single track across *all*
    /// sessions. Used by the POST response so the client can render
    /// the new totals after voting.
    pub async fn for_track(&self, track_id: &TrackId) -> Result<FeedbackCounts> {
        let row = sqlx::query(
            "SELECT
               COALESCE(SUM(CASE WHEN vote = 1 THEN 1 ELSE 0 END), 0) AS up,
               COALESCE(SUM(CASE WHEN vote = -1 THEN 1 ELSE 0 END), 0) AS down
             FROM recommend_feedback WHERE track_id = ?",
        )
        .bind(track_id.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(FeedbackCounts {
            up: row.get::<i64, _>("up"),
            down: row.get::<i64, _>("down"),
        })
    }

    /// All track ids the given `session_id` has downvoted (`vote = -1`).
    /// Empty when the session has no rows or only upvotes. Used by the
    /// recommender to keep "I just thumbs-downed this" tracks out of
    /// further suggestions within the same recommend-session — without
    /// poisoning a future session's results.
    pub async fn downvoted_in_session(&self, session_id: &str) -> Result<Vec<TrackId>> {
        let rows = sqlx::query(
            "SELECT track_id FROM recommend_feedback
             WHERE session_id = ? AND vote = -1",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| TrackId::from(r.get::<String, _>("track_id")))
            .collect())
    }

    /// Per-track aggregate over `received_ms >= since_ms` (or all-time
    /// if `since_ms` is `None`), newest-voted first, capped at `limit`.
    /// The caller joins these against the metadata cache to render the
    /// diagnostics table.
    pub async fn aggregate(
        &self,
        since_ms: Option<i64>,
        limit: i64,
    ) -> Result<Vec<FeedbackAggregate>> {
        // Two queries (with-since vs without) rather than one with an
        // optional WHERE — sqlx::query_as wants a fixed parameter
        // count and SQLite's `AND ? IS NULL` trick obscures the index.
        let rows = if let Some(since) = since_ms {
            sqlx::query(
                "SELECT track_id,
                        SUM(CASE WHEN vote = 1 THEN 1 ELSE 0 END) AS up,
                        SUM(CASE WHEN vote = -1 THEN 1 ELSE 0 END) AS down,
                        MAX(received_ms) AS last_voted_ms
                 FROM recommend_feedback
                 WHERE received_ms >= ?
                 GROUP BY track_id
                 ORDER BY last_voted_ms DESC, MAX(id) DESC
                 LIMIT ?",
            )
            .bind(since)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT track_id,
                        SUM(CASE WHEN vote = 1 THEN 1 ELSE 0 END) AS up,
                        SUM(CASE WHEN vote = -1 THEN 1 ELSE 0 END) AS down,
                        MAX(received_ms) AS last_voted_ms
                 FROM recommend_feedback
                 GROUP BY track_id
                 ORDER BY last_voted_ms DESC, MAX(id) DESC
                 LIMIT ?",
            )
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows
            .into_iter()
            .map(|r| FeedbackAggregate {
                track_id: r.get::<String, _>("track_id"),
                up: r.get::<i64, _>("up"),
                down: r.get::<i64, _>("down"),
                last_voted_ms: r.get::<i64, _>("last_voted_ms"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::EmbeddingStore;

    async fn store() -> FeedbackStore {
        let embed = EmbeddingStore::open_in_memory().await.unwrap();
        FeedbackStore::new(embed.pool().clone())
    }

    fn tid(s: &str) -> TrackId {
        TrackId::from(s.to_string())
    }

    #[tokio::test]
    async fn for_track_returns_zero_when_empty() {
        let s = store().await;
        assert_eq!(
            s.for_track(&tid("unseen")).await.unwrap(),
            FeedbackCounts { up: 0, down: 0 }
        );
    }

    #[tokio::test]
    async fn record_then_for_track_counts_upvote() {
        let s = store().await;
        s.record(&tid("t1"), "sess-A", 1, 1_000, 1_001)
            .await
            .unwrap();
        assert_eq!(
            s.for_track(&tid("t1")).await.unwrap(),
            FeedbackCounts { up: 1, down: 0 }
        );
    }

    #[tokio::test]
    async fn flipping_vote_replaces_rather_than_appends() {
        // Same session, same track, opposite vote. The UPSERT must
        // overwrite the previous row — otherwise the user "earns" two
        // votes by changing their mind.
        let s = store().await;
        s.record(&tid("t1"), "sess-A", 1, 1_000, 1_001)
            .await
            .unwrap();
        s.record(&tid("t1"), "sess-A", -1, 2_000, 2_001)
            .await
            .unwrap();
        assert_eq!(
            s.for_track(&tid("t1")).await.unwrap(),
            FeedbackCounts { up: 0, down: 1 }
        );
    }

    #[tokio::test]
    async fn different_sessions_accumulate() {
        let s = store().await;
        s.record(&tid("t1"), "sess-A", 1, 1_000, 1_001)
            .await
            .unwrap();
        s.record(&tid("t1"), "sess-B", 1, 1_100, 1_101)
            .await
            .unwrap();
        s.record(&tid("t1"), "sess-C", -1, 1_200, 1_201)
            .await
            .unwrap();
        assert_eq!(
            s.for_track(&tid("t1")).await.unwrap(),
            FeedbackCounts { up: 2, down: 1 }
        );
    }

    #[tokio::test]
    async fn clear_removes_an_existing_vote() {
        let s = store().await;
        s.record(&tid("t1"), "sess-A", 1, 1_000, 1_001)
            .await
            .unwrap();
        s.clear(&tid("t1"), "sess-A").await.unwrap();
        assert_eq!(
            s.for_track(&tid("t1")).await.unwrap(),
            FeedbackCounts { up: 0, down: 0 }
        );
    }

    #[tokio::test]
    async fn clear_on_missing_row_is_a_noop() {
        let s = store().await;
        s.clear(&tid("never"), "sess-Z").await.unwrap();
    }

    #[tokio::test]
    async fn aggregate_groups_per_track_and_sorts_by_recency() {
        let s = store().await;
        s.record(&tid("t1"), "a", 1, 100, 100).await.unwrap();
        s.record(&tid("t1"), "b", 1, 200, 200).await.unwrap();
        s.record(&tid("t2"), "a", -1, 500, 500).await.unwrap();
        s.record(&tid("t3"), "a", 1, 300, 300).await.unwrap();

        let got = s.aggregate(None, 100).await.unwrap();
        // newest-voted first → t2 (500), t3 (300), t1 (200)
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].track_id, "t2");
        assert_eq!(got[0].down, 1);
        assert_eq!(got[1].track_id, "t3");
        assert_eq!(got[2].track_id, "t1");
        assert_eq!(got[2].up, 2);
    }

    #[tokio::test]
    async fn aggregate_applies_since_ms_filter() {
        let s = store().await;
        s.record(&tid("t1"), "a", 1, 100, 100).await.unwrap();
        s.record(&tid("t2"), "a", 1, 500, 500).await.unwrap();

        let got = s.aggregate(Some(300), 100).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].track_id, "t2");
    }

    #[tokio::test]
    async fn downvoted_in_session_returns_only_minus_one_votes_for_that_session() {
        let s = store().await;
        s.record(&tid("t-up"), "sess-A", 1, 100, 100).await.unwrap();
        s.record(&tid("t-down-a"), "sess-A", -1, 200, 200)
            .await
            .unwrap();
        s.record(&tid("t-also-down"), "sess-A", -1, 300, 300)
            .await
            .unwrap();
        // Different session — same track downvoted there must not show up.
        s.record(&tid("t-other"), "sess-B", -1, 400, 400)
            .await
            .unwrap();

        let mut got = s.downvoted_in_session("sess-A").await.unwrap();
        got.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(got, vec![tid("t-also-down"), tid("t-down-a")]);
    }

    #[tokio::test]
    async fn downvoted_in_session_returns_empty_for_unknown_session() {
        let s = store().await;
        s.record(&tid("t1"), "sess-A", -1, 100, 100).await.unwrap();
        assert!(
            s.downvoted_in_session("sess-nope")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn aggregate_respects_limit() {
        let s = store().await;
        for i in 0..5 {
            s.record(&tid(&format!("t{i}")), "s", 1, i, i)
                .await
                .unwrap();
        }
        let got = s.aggregate(None, 3).await.unwrap();
        assert_eq!(got.len(), 3);
    }
}
