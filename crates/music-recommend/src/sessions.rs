//! Recommend-session lifetime store.
//!
//! Durable record of `SyncOp::StartSession` / `SyncOp::StopSession`
//! events. The in-memory `PlaybackState.session_anchor` is a view of
//! whichever row is currently open (`ended_ms IS NULL`); this store
//! is the persisted source of truth that survives gateway restarts
//! and lets us reconstruct what a user listened to in a given
//! session by joining on `events.session_id`.
//!
//! Sync state enforces "at most one active session." This store
//! mirrors that invariant: [`Self::start`] closes any existing active
//! row at the new `started_ms` before inserting the new one, so the
//! `ended_ms IS NULL` set has cardinality ≤ 1 by construction.

use sqlx::{Row, SqlitePool};

use music_core::{SessionId, TrackId};

use crate::Result;

/// One row from `recommend_sessions`. `ended_ms = None` means the
/// session is still live (and, by invariant, is the unique active
/// session — see module doc).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub session_id: SessionId,
    pub anchor_track_id: TrackId,
    pub items_count: i64,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct SessionStore {
    pool: SqlitePool,
}

impl SessionStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Open a new session. Closes any currently-active row at
    /// `started_ms` first — sync state only allows one active session
    /// at a time, and we mirror that here so subsequent queries against
    /// the `ended_ms IS NULL` partial index find at most one row.
    ///
    /// Errors with a UNIQUE-violation if `session_id` already exists in
    /// any state. Callers should never re-use a session id.
    pub async fn start(
        &self,
        session_id: &SessionId,
        anchor_track_id: &TrackId,
        items_count: i64,
        started_ms: i64,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE recommend_sessions
                SET ended_ms = ?
              WHERE ended_ms IS NULL",
        )
        .bind(started_ms)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO recommend_sessions
                 (session_id, anchor_track_id, items_count, started_ms, ended_ms)
             VALUES (?, ?, ?, ?, NULL)",
        )
        .bind(session_id.as_str())
        .bind(anchor_track_id.as_str())
        .bind(items_count)
        .bind(started_ms)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Close a specific session. No-op if the row doesn't exist or is
    /// already closed (we don't move `ended_ms` backwards on a duplicate
    /// stop — first-stop wins, same posture as the play_history clock).
    pub async fn stop(&self, session_id: &SessionId, ended_ms: i64) -> Result<()> {
        sqlx::query(
            "UPDATE recommend_sessions
                SET ended_ms = ?
              WHERE session_id = ? AND ended_ms IS NULL",
        )
        .bind(ended_ms)
        .bind(session_id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The currently-active session, if any. By invariant at most one
    /// row qualifies; in the pathological "two active rows" case
    /// (would only happen on direct SQL surgery) we return the most
    /// recently started.
    pub async fn active(&self) -> Result<Option<SessionRow>> {
        let row = sqlx::query(
            "SELECT session_id, anchor_track_id, items_count, started_ms, ended_ms
               FROM recommend_sessions
              WHERE ended_ms IS NULL
              ORDER BY started_ms DESC
              LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_to_session))
    }

    /// Full row by id, regardless of open/closed state. Used by the
    /// diagnostics endpoint to render "session N details."
    pub async fn get(&self, session_id: &SessionId) -> Result<Option<SessionRow>> {
        let row = sqlx::query(
            "SELECT session_id, anchor_track_id, items_count, started_ms, ended_ms
               FROM recommend_sessions
              WHERE session_id = ?",
        )
        .bind(session_id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_to_session))
    }

    /// Newest-first by `started_ms`. Drives the diagnostics history
    /// panel. `limit` is capped server-side by the caller — this method
    /// does not clamp.
    pub async fn recent(&self, limit: i64) -> Result<Vec<SessionRow>> {
        let rows = sqlx::query(
            "SELECT session_id, anchor_track_id, items_count, started_ms, ended_ms
               FROM recommend_sessions
              ORDER BY started_ms DESC
              LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_session).collect())
    }
}

fn row_to_session(r: sqlx::sqlite::SqliteRow) -> SessionRow {
    SessionRow {
        session_id: SessionId::from(r.get::<String, _>("session_id")),
        anchor_track_id: TrackId::from(r.get::<String, _>("anchor_track_id")),
        items_count: r.get::<i64, _>("items_count"),
        started_ms: r.get::<i64, _>("started_ms"),
        ended_ms: r.get::<Option<i64>, _>("ended_ms"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::EmbeddingStore;

    async fn store() -> SessionStore {
        let embed = EmbeddingStore::open_in_memory().await.unwrap();
        SessionStore::new(embed.pool().clone())
    }

    fn sid(s: &str) -> SessionId {
        SessionId::from(s.to_string())
    }

    fn tid(s: &str) -> TrackId {
        TrackId::from(s.to_string())
    }

    #[tokio::test]
    async fn active_returns_none_with_no_sessions() {
        let s = store().await;
        assert!(s.active().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn start_records_row_as_active() {
        let s = store().await;
        s.start(&sid("s1"), &tid("t1"), 10, 1_000).await.unwrap();
        let row = s.active().await.unwrap().expect("active row");
        assert_eq!(row.session_id, sid("s1"));
        assert_eq!(row.anchor_track_id, tid("t1"));
        assert_eq!(row.items_count, 10);
        assert_eq!(row.started_ms, 1_000);
        assert_eq!(row.ended_ms, None);
    }

    #[tokio::test]
    async fn stop_sets_ended_ms() {
        let s = store().await;
        s.start(&sid("s1"), &tid("t1"), 10, 1_000).await.unwrap();
        s.stop(&sid("s1"), 5_000).await.unwrap();
        assert!(s.active().await.unwrap().is_none());
        let row = s.get(&sid("s1")).await.unwrap().expect("row exists");
        assert_eq!(row.ended_ms, Some(5_000));
    }

    #[tokio::test]
    async fn starting_new_closes_previous_at_new_started_ms() {
        // The single-active invariant: opening s2 must stamp s1's
        // ended_ms with s2's started_ms, so the timeline is contiguous
        // and there are never two active rows.
        let s = store().await;
        s.start(&sid("s1"), &tid("t1"), 10, 1_000).await.unwrap();
        s.start(&sid("s2"), &tid("t2"), 5, 2_000).await.unwrap();
        let s1 = s.get(&sid("s1")).await.unwrap().unwrap();
        assert_eq!(s1.ended_ms, Some(2_000));
        let active = s.active().await.unwrap().unwrap();
        assert_eq!(active.session_id, sid("s2"));
    }

    #[tokio::test]
    async fn starting_new_does_not_disturb_already_closed_rows() {
        let s = store().await;
        s.start(&sid("s1"), &tid("t1"), 1, 1_000).await.unwrap();
        s.stop(&sid("s1"), 1_500).await.unwrap();
        s.start(&sid("s2"), &tid("t2"), 1, 2_000).await.unwrap();
        let s1 = s.get(&sid("s1")).await.unwrap().unwrap();
        assert_eq!(s1.ended_ms, Some(1_500));
    }

    #[tokio::test]
    async fn duplicate_stop_does_not_move_ended_ms() {
        let s = store().await;
        s.start(&sid("s1"), &tid("t1"), 1, 1_000).await.unwrap();
        s.stop(&sid("s1"), 2_000).await.unwrap();
        s.stop(&sid("s1"), 3_000).await.unwrap();
        let s1 = s.get(&sid("s1")).await.unwrap().unwrap();
        assert_eq!(s1.ended_ms, Some(2_000));
    }

    #[tokio::test]
    async fn recent_returns_newest_first() {
        let s = store().await;
        s.start(&sid("s1"), &tid("t1"), 1, 1_000).await.unwrap();
        s.start(&sid("s2"), &tid("t2"), 1, 2_000).await.unwrap();
        s.start(&sid("s3"), &tid("t3"), 1, 3_000).await.unwrap();
        let rows = s.recent(10).await.unwrap();
        let ids: Vec<_> = rows
            .iter()
            .map(|r| r.session_id.as_str().to_owned())
            .collect();
        assert_eq!(ids, vec!["s3", "s2", "s1"]);
    }

    #[tokio::test]
    async fn recent_respects_limit() {
        let s = store().await;
        s.start(&sid("s1"), &tid("t1"), 1, 1_000).await.unwrap();
        s.start(&sid("s2"), &tid("t2"), 1, 2_000).await.unwrap();
        let rows = s.recent(1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, sid("s2"));
    }

    #[tokio::test]
    async fn get_returns_none_for_unknown_id() {
        let s = store().await;
        assert!(s.get(&sid("unknown")).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn reusing_session_id_errors() {
        // Defensive: PK on session_id catches a caller-side bug
        // (uuid generation collision, accidental re-submission).
        // We assert it errors rather than silently overwriting.
        let s = store().await;
        s.start(&sid("s1"), &tid("t1"), 1, 1_000).await.unwrap();
        s.stop(&sid("s1"), 1_500).await.unwrap();
        let result = s.start(&sid("s1"), &tid("t2"), 1, 2_000).await;
        assert!(result.is_err(), "re-using session_id must fail");
    }
}
