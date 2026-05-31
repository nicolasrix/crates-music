//! Per-track preference affinity store (the precomputed decayed counter).
//!
//! Persists the `(score, updated_ms)` decayed counter described in
//! [`crate::preference`] so the recommend hot path can read a candidate's
//! affinity in O(1). Writes ride the existing scrobble / feedback / skip
//! paths (best-effort, never blocking the request); reads happen once per
//! recommend call as a single batched `IN (…)` query over the candidate
//! pool.
//!
//! The arithmetic lives in [`crate::preference`] (pure, unit-tested); this
//! module is the thin SQLite read-modify-write around it. Source of truth
//! for the underlying signal stays the append-only `events` +
//! `recommend_feedback` tables — this is a rebuildable derived view, like
//! [`crate::play_history`].

use std::collections::HashMap;

use sqlx::{Row, SqlitePool};

use music_core::TrackId;

use crate::Result;
use crate::preference::{AffinityEvent, affinity_at, event_weight, fold_event};

#[derive(Clone, Debug)]
pub struct TrackAffinityStore {
    pool: SqlitePool,
}

/// Raw diagnostics view of a track's affinity row: the live decayed
/// affinity plus the lifetime tallies that explain it. Returned by
/// [`TrackAffinityStore::row`].
#[derive(Clone, Debug, PartialEq)]
pub struct AffinityRow {
    pub track_id: TrackId,
    /// Affinity decayed to the `now_ms` passed to [`TrackAffinityStore::row`].
    pub affinity: f32,
    pub play_count: i64,
    pub skip_count: i64,
    pub like_count: i64,
    pub dislike_count: i64,
}

/// Read the `score` column (stored as SQLite REAL/f64) back as the f32
/// the affinity math works in. The cast is lossless in practice —
/// affinity scores are small bounded sums — so the truncation lint is
/// suppressed here rather than at every call site.
#[allow(clippy::cast_possible_truncation)]
fn read_score(row: &sqlx::sqlite::SqliteRow) -> f32 {
    row.get::<f64, _>("score") as f32
}

/// Which raw tally an event bumps. The interpolated column name is a
/// fixed string from this match — never user input — so there's no
/// injection surface.
fn counter_column(event: AffinityEvent) -> &'static str {
    match event {
        AffinityEvent::Like => "like_count",
        AffinityEvent::Dislike => "dislike_count",
        AffinityEvent::Play { .. } => "play_count",
        AffinityEvent::Skip { .. } => "skip_count",
    }
}

impl TrackAffinityStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Fold one interaction into the track's decayed counter and bump the
    /// matching lifetime tally. Read-modify-write inside a transaction so
    /// two concurrent applies to the same track can't lose an update.
    ///
    /// `event_ms` is the client's wall-clock timestamp of the
    /// interaction; `half_life_ms` is the affinity decay half-life. A
    /// brand-new track anchors the counter at `event_ms` (so the first
    /// event neither decays nor is decayed).
    pub async fn apply_event(
        &self,
        track_id: &TrackId,
        event: AffinityEvent,
        event_ms: i64,
        half_life_ms: i64,
    ) -> Result<()> {
        let weight = event_weight(event);
        let col = counter_column(event);

        let mut tx = self.pool.begin().await?;
        let prev = sqlx::query("SELECT score, updated_ms FROM track_affinity WHERE track_id = ?")
            .bind(track_id.as_str())
            .fetch_optional(&mut *tx)
            .await?;
        let (prev_score, prev_updated_ms) = prev.map_or((0.0, event_ms), |r| {
            (read_score(&r), r.get::<i64, _>("updated_ms"))
        });

        let (new_score, new_updated_ms) =
            fold_event(prev_score, prev_updated_ms, event_ms, weight, half_life_ms);

        // `col` is one of four hard-coded identifiers — safe to format in.
        let sql = format!(
            "INSERT INTO track_affinity (track_id, score, updated_ms, {col})
                 VALUES (?, ?, ?, 1)
             ON CONFLICT(track_id) DO UPDATE SET
                 score = ?,
                 updated_ms = ?,
                 {col} = {col} + 1"
        );
        sqlx::query(&sql)
            .bind(track_id.as_str())
            .bind(f64::from(new_score))
            .bind(new_updated_ms)
            .bind(f64::from(new_score))
            .bind(new_updated_ms)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Batch-read live affinities for a candidate pool, each decayed to
    /// `now_ms`. Tracks with no row are simply absent from the map — the
    /// caller treats "absent" as affinity 0 (the discovery case). One
    /// `IN (…)` query; candidate pools are small (≤ ~80) so the parameter
    /// count stays well under SQLite's limit.
    pub async fn affinity_many(
        &self,
        track_ids: &[TrackId],
        now_ms: i64,
        half_life_ms: i64,
    ) -> Result<HashMap<TrackId, f32>> {
        if track_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = std::iter::repeat_n("?", track_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT track_id, score, updated_ms FROM track_affinity WHERE track_id IN ({placeholders})"
        );
        let mut q = sqlx::query(&sql);
        for id in track_ids {
            q = q.bind(id.as_str());
        }
        let rows = q.fetch_all(&self.pool).await?;
        let mut out = HashMap::with_capacity(rows.len());
        for r in rows {
            let tid = TrackId::from(r.get::<String, _>("track_id"));
            let score = read_score(&r);
            let updated_ms = r.get::<i64, _>("updated_ms");
            out.insert(tid, affinity_at(score, updated_ms, now_ms, half_life_ms));
        }
        Ok(out)
    }

    /// Diagnostics: the full row for one track, affinity decayed to
    /// `now_ms`. `None` when the track has never been engaged with.
    pub async fn row(
        &self,
        track_id: &TrackId,
        now_ms: i64,
        half_life_ms: i64,
    ) -> Result<Option<AffinityRow>> {
        let row = sqlx::query(
            "SELECT score, updated_ms, play_count, skip_count, like_count, dislike_count
             FROM track_affinity WHERE track_id = ?",
        )
        .bind(track_id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| {
            let score = read_score(&r);
            let updated_ms = r.get::<i64, _>("updated_ms");
            AffinityRow {
                track_id: track_id.clone(),
                affinity: affinity_at(score, updated_ms, now_ms, half_life_ms),
                play_count: r.get::<i64, _>("play_count"),
                skip_count: r.get::<i64, _>("skip_count"),
                like_count: r.get::<i64, _>("like_count"),
                dislike_count: r.get::<i64, _>("dislike_count"),
            }
        }))
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::store::EmbeddingStore;

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    const HL: i64 = 30 * DAY_MS;

    async fn store() -> TrackAffinityStore {
        let embed = EmbeddingStore::open_in_memory().await.unwrap();
        TrackAffinityStore::new(embed.pool().clone())
    }

    fn tid(s: &str) -> TrackId {
        TrackId::from(s.to_string())
    }

    #[tokio::test]
    async fn unknown_track_is_absent_from_batch() {
        let s = store().await;
        let got = s.affinity_many(&[tid("never")], 10_000, HL).await.unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn empty_batch_returns_empty() {
        let s = store().await;
        assert!(s.affinity_many(&[], 10_000, HL).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn like_produces_positive_affinity() {
        let s = store().await;
        s.apply_event(&tid("t1"), AffinityEvent::Like, 1_000, HL)
            .await
            .unwrap();
        let got = s.affinity_many(&[tid("t1")], 1_000, HL).await.unwrap();
        assert!(got[&tid("t1")] > 0.0, "got {:?}", got.get(&tid("t1")));
    }

    #[tokio::test]
    async fn early_skip_produces_negative_affinity() {
        let s = store().await;
        s.apply_event(&tid("t1"), AffinityEvent::Skip { completion: 0.0 }, 1_000, HL)
            .await
            .unwrap();
        let got = s.affinity_many(&[tid("t1")], 1_000, HL).await.unwrap();
        assert!(got[&tid("t1")] < 0.0);
    }

    #[tokio::test]
    async fn repeated_likes_accumulate() {
        let s = store().await;
        s.apply_event(&tid("t1"), AffinityEvent::Like, 1_000, HL)
            .await
            .unwrap();
        let one = s.affinity_many(&[tid("t1")], 1_000, HL).await.unwrap()[&tid("t1")];
        s.apply_event(&tid("t1"), AffinityEvent::Like, 1_000, HL)
            .await
            .unwrap();
        let two = s.affinity_many(&[tid("t1")], 1_000, HL).await.unwrap()[&tid("t1")];
        assert!(two > one, "second like should raise affinity: {one} -> {two}");
    }

    #[tokio::test]
    async fn dislike_cancels_a_like() {
        let s = store().await;
        s.apply_event(&tid("t1"), AffinityEvent::Like, 1_000, HL)
            .await
            .unwrap();
        s.apply_event(&tid("t1"), AffinityEvent::Dislike, 1_000, HL)
            .await
            .unwrap();
        let got = s.affinity_many(&[tid("t1")], 1_000, HL).await.unwrap();
        // LIKE_WEIGHT + DISLIKE_WEIGHT == 0 → affinity 0.
        assert!(got[&tid("t1")].abs() < 1e-6, "got {}", got[&tid("t1")]);
    }

    #[tokio::test]
    async fn affinity_decays_between_event_and_read() {
        let s = store().await;
        s.apply_event(&tid("t1"), AffinityEvent::Like, 0, HL)
            .await
            .unwrap();
        let fresh = s.affinity_many(&[tid("t1")], 0, HL).await.unwrap()[&tid("t1")];
        let aged = s
            .affinity_many(&[tid("t1")], 4 * HL, HL)
            .await
            .unwrap()[&tid("t1")];
        assert!(aged < fresh && aged > 0.0, "fresh {fresh}, aged {aged}");
    }

    #[tokio::test]
    async fn tracks_are_isolated() {
        let s = store().await;
        s.apply_event(&tid("liked"), AffinityEvent::Like, 1_000, HL)
            .await
            .unwrap();
        s.apply_event(&tid("disliked"), AffinityEvent::Dislike, 1_000, HL)
            .await
            .unwrap();
        let got = s
            .affinity_many(&[tid("liked"), tid("disliked"), tid("unseen")], 1_000, HL)
            .await
            .unwrap();
        assert!(got[&tid("liked")] > 0.0);
        assert!(got[&tid("disliked")] < 0.0);
        assert!(!got.contains_key(&tid("unseen")));
    }

    #[tokio::test]
    async fn row_reports_lifetime_tallies() {
        let s = store().await;
        s.apply_event(&tid("t1"), AffinityEvent::Like, 1_000, HL)
            .await
            .unwrap();
        s.apply_event(&tid("t1"), AffinityEvent::Play { completion: 1.0 }, 1_100, HL)
            .await
            .unwrap();
        s.apply_event(&tid("t1"), AffinityEvent::Play { completion: 1.0 }, 1_200, HL)
            .await
            .unwrap();
        s.apply_event(&tid("t1"), AffinityEvent::Skip { completion: 0.1 }, 1_300, HL)
            .await
            .unwrap();
        let row = s.row(&tid("t1"), 1_300, HL).await.unwrap().unwrap();
        assert_eq!(row.like_count, 1);
        assert_eq!(row.play_count, 2);
        assert_eq!(row.skip_count, 1);
        assert_eq!(row.dislike_count, 0);
    }

    #[tokio::test]
    async fn row_is_none_for_unknown_track() {
        let s = store().await;
        assert_eq!(s.row(&tid("never"), 1_000, HL).await.unwrap(), None);
    }

    #[tokio::test]
    async fn out_of_order_event_does_not_move_clock_backwards() {
        // A like at t=2·HL, then a replayed older like at t=HL. The
        // affinity must still rise (the old weight, aged forward, counts)
        // but the row's clock stays at the newer timestamp.
        let s = store().await;
        s.apply_event(&tid("t1"), AffinityEvent::Like, 2 * HL, HL)
            .await
            .unwrap();
        let before = s.affinity_many(&[tid("t1")], 2 * HL, HL).await.unwrap()[&tid("t1")];
        s.apply_event(&tid("t1"), AffinityEvent::Like, HL, HL)
            .await
            .unwrap();
        let after = s.affinity_many(&[tid("t1")], 2 * HL, HL).await.unwrap()[&tid("t1")];
        assert!(after > before, "replayed like should still add signal: {before} -> {after}");
    }
}
