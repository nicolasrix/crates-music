//! Recommendation provenance log.
//!
//! Captures, at serve time, *what the recommender served and in what
//! context* — the data needed to later train models on the system's own
//! errors (served-but-skipped vs served-and-kept). The user-interaction
//! signal (see [`crate::events`], [`crate::feedback`], [`crate::rating`])
//! records what the listener *did*; this records what they were *shown*.
//! Joining the two at training time closes the loop.
//!
//! Write-once at serve time, never mutated afterwards. Outcomes are NOT
//! stored here — they are joined in from the event log at training time
//! (`recommendation_item.entity_id` → `events.track_id` where
//! `events.occurred_at > recommendation.served_ms`). That keeps this a
//! faithful snapshot of each decision.
//!
//! Two tables, mirroring the request → slate shape: a `recommendation`
//! row (the context) owns N `recommendation_item` rows (the ordered
//! slate). See `migrations/0016_recommendation_log.sql`.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::Result;

/// Which endpoint produced a recommendation. Stringly-typed in the DB
/// (like [`crate::events::EventType`]) so new variants don't need a
/// migration; unknown strings on read parse to [`RecommendationKind::Other`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationKind {
    Next,
    Station,
    FromSeeds,
    FromAny,
    SimilarAlbums,
    SimilarArtists,
    Other,
}

impl RecommendationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Next => "next",
            Self::Station => "station",
            Self::FromSeeds => "from_seeds",
            Self::FromAny => "from_any",
            Self::SimilarAlbums => "similar_albums",
            Self::SimilarArtists => "similar_artists",
            Self::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "next" => Self::Next,
            "station" => Self::Station,
            "from_seeds" => Self::FromSeeds,
            "from_any" => Self::FromAny,
            "similar_albums" => Self::SimilarAlbums,
            "similar_artists" => Self::SimilarArtists,
            _ => Self::Other,
        }
    }

    /// Whether this endpoint's `entity_id`s are track ids (so a served
    /// item can be joined to the per-track event log for an outcome). The
    /// `similar_*` endpoints serve album / artist ids, which don't join.
    pub fn serves_tracks(self) -> bool {
        matches!(
            self,
            Self::Next | Self::Station | Self::FromSeeds | Self::FromAny
        )
    }
}

/// What the listener did with a served track, joined from the event log.
/// Only meaningful for track-serving kinds (see
/// [`RecommendationKind::serves_tracks`]); `None` on an item means "not
/// applicable" (an album/artist recommendation), distinct from
/// [`RecommendationOutcome::Pending`] ("a track, but no event yet").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationOutcome {
    /// A `scrobble` landed — the track played through. Positive label.
    Kept,
    /// A `skip` landed first. Negative label.
    Skipped,
    /// No scrobble/skip yet in this session+window — still in flight.
    Pending,
}

impl RecommendationOutcome {
    /// Map the earliest joined event type to a label. `None` (no event)
    /// → `Pending`.
    fn from_event(event_type: Option<&str>) -> Self {
        match event_type {
            Some("scrobble") => Self::Kept,
            Some("skip") => Self::Skipped,
            _ => Self::Pending,
        }
    }
}

/// One served candidate in a recommendation slate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecommendationItemRecord {
    /// `track_id`, or `album_id` / `artist_id` for the similar_* paths.
    pub entity_id: String,
    /// The scalar the recommender ordered by at this rank.
    pub score: Option<f32>,
    /// Per-candidate features computed at serve time (similarity,
    /// seed_hits, affinity_bonus, supporting_tracks, …). Opaque JSON.
    pub features: serde_json::Value,
    /// The listener's outcome for this item, joined from the event log on
    /// the read path. `None` on the write path and for non-track kinds;
    /// skipped from serialization when absent so the stored/write shape is
    /// unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RecommendationOutcome>,
}

impl RecommendationItemRecord {
    /// Convenience constructor for a bare score (empty feature blob).
    pub fn new(entity_id: impl Into<String>, score: Option<f32>) -> Self {
        Self {
            entity_id: entity_id.into(),
            score,
            features: serde_json::Value::Object(serde_json::Map::new()),
            outcome: None,
        }
    }

    #[must_use]
    pub fn with_features(mut self, features: serde_json::Value) -> Self {
        self.features = features;
        self
    }
}

/// A recommendation to persist. Built by the gateway handler from the
/// final, ordered slate just before it serializes the HTTP response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecommendationRecord {
    pub kind: RecommendationKind,
    pub session_id: Option<String>,
    pub model_version: String,
    /// Seed/candidate track ids the request was built from. `None` for
    /// the text station.
    pub seeds: Option<Vec<String>>,
    /// Natural-language station query. `None` for seed-based paths.
    pub text_query: Option<String>,
    /// Request knobs in force (see migration doc). Opaque JSON object.
    pub params: serde_json::Value,
    pub degraded: bool,
    /// The ordered served slate (rank = index).
    pub items: Vec<RecommendationItemRecord>,
}

/// A persisted recommendation with its gateway-assigned id and serve
/// timestamp. Returned by [`RecommendationLogStore::recent`] for the
/// diagnostics surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredRecommendation {
    pub id: i64,
    pub kind: RecommendationKind,
    pub session_id: Option<String>,
    pub model_version: String,
    pub seeds: Option<Vec<String>>,
    pub text_query: Option<String>,
    pub params: serde_json::Value,
    pub degraded: bool,
    pub result_count: i64,
    pub served_ms: i64,
    /// The slate, rank-ascending. Empty in summary listings that don't
    /// hydrate items.
    pub items: Vec<RecommendationItemRecord>,
}

#[derive(Clone, Debug)]
pub struct RecommendationLogStore {
    pool: SqlitePool,
}

impl RecommendationLogStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Persist one recommendation + its items in a single transaction
    /// (all-or-nothing, so a partial slate never lands). Returns the new
    /// `recommendation.id`. `served_ms` is stamped here from the gateway
    /// clock.
    pub async fn record(&self, user_id: i64, rec: &RecommendationRecord) -> Result<i64> {
        let served_ms = now_ms();
        let seeds_json = rec
            .seeds
            .as_ref()
            .map(|s| serde_json::to_string(s).expect("Vec<String> serializes"));
        let params_json = serde_json::to_string(&rec.params).expect("Value serializes");

        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "INSERT INTO recommendation
                 (user_id, kind, session_id, model_version, seeds_json, text_query,
                  params_json, degraded, result_count, served_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(user_id)
        .bind(rec.kind.as_str())
        .bind(rec.session_id.as_deref())
        .bind(&rec.model_version)
        .bind(seeds_json)
        .bind(rec.text_query.as_deref())
        .bind(params_json)
        .bind(i64::from(rec.degraded))
        .bind(i64::try_from(rec.items.len()).unwrap_or(i64::MAX))
        .bind(served_ms)
        .fetch_one(&mut *tx)
        .await?;
        let id: i64 = row.get("id");

        for (rank, item) in rec.items.iter().enumerate() {
            let features_json = serde_json::to_string(&item.features).expect("Value serializes");
            sqlx::query(
                "INSERT INTO recommendation_item
                     (recommendation_id, rank, entity_id, score, features_json)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(i64::try_from(rank).unwrap_or(i64::MAX))
            .bind(&item.entity_id)
            .bind(item.score.map(f64::from))
            .bind(features_json)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(id)
    }

    /// Total recommendations logged. Diagnostic.
    pub async fn count(&self) -> Result<u64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM recommendation")
            .fetch_one(&self.pool)
            .await?;
        let n: i64 = row.get("n");
        Ok(u64::try_from(n.max(0)).unwrap_or(0))
    }

    /// Most recent `limit` recommendations, newest first, with their
    /// items hydrated (rank-ascending). Diagnostic / inspection.
    pub async fn recent(&self, limit: u32) -> Result<Vec<StoredRecommendation>> {
        let rows = sqlx::query(
            "SELECT id, kind, session_id, model_version, seeds_json, text_query,
                    params_json, degraded, result_count, served_ms
                 FROM recommendation
                 ORDER BY id DESC
                 LIMIT ?",
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: i64 = row.get("id");
            let items = self.items_for(id).await?;
            out.push(StoredRecommendation {
                id,
                kind: RecommendationKind::parse(row.get::<String, _>("kind").as_str()),
                session_id: row.get("session_id"),
                model_version: row.get("model_version"),
                seeds: row
                    .get::<Option<String>, _>("seeds_json")
                    .and_then(|s| serde_json::from_str(&s).ok()),
                text_query: row.get("text_query"),
                params: row
                    .get::<Option<String>, _>("params_json")
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(serde_json::Value::Null),
                degraded: row.get::<i64, _>("degraded") != 0,
                result_count: row.get("result_count"),
                served_ms: row.get("served_ms"),
                items,
            });
        }
        Ok(out)
    }

    // SQLite stores `score` as REAL (f64); we keep f32 in the domain type.
    // The narrowing cast is intentional and lossless for our score range.
    #[allow(clippy::cast_possible_truncation)]
    async fn items_for(&self, recommendation_id: i64) -> Result<Vec<RecommendationItemRecord>> {
        let rows = sqlx::query(
            "SELECT entity_id, score, features_json
                 FROM recommendation_item
                 WHERE recommendation_id = ?
                 ORDER BY rank ASC",
        )
        .bind(recommendation_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| RecommendationItemRecord {
                entity_id: row.get("entity_id"),
                score: row.get::<Option<f64>, _>("score").map(|v| v as f32),
                features: serde_json::from_str(&row.get::<String, _>("features_json"))
                    .unwrap_or(serde_json::Value::Null),
                outcome: None,
            })
            .collect())
    }

    /// Like [`Self::recent`], but each track item carries the listener's
    /// [`RecommendationOutcome`] joined from the event log. The join is
    /// **session-scoped** when the recommendation has a `session_id` (the
    /// from_seeds / from_any autoplay paths) — so a track served, ignored,
    /// then served+skipped in a *different* session can't mis-label this
    /// one. It also bounds the forward window to [`OUTCOME_WINDOW_MS`] and
    /// allows [`OUTCOME_SKEW_MS`] of client-clock lag on the lower edge.
    /// `similar_*` items (album/artist ids) get `outcome = None`.
    pub async fn recent_with_outcomes(&self, limit: u32) -> Result<Vec<StoredRecommendation>> {
        let mut recs = self.recent(limit).await?;
        for rec in &mut recs {
            if !rec.kind.serves_tracks() {
                continue; // album/artist items don't join to the track event log
            }
            for item in &mut rec.items {
                item.outcome = Some(
                    self.outcome_for(&item.entity_id, rec.served_ms, rec.session_id.as_deref())
                        .await?,
                );
            }
        }
        Ok(recs)
    }

    /// Earliest scrobble/skip for `track_id` in the serve window, scoped
    /// to `session_id` when present. Returns the mapped outcome.
    async fn outcome_for(
        &self,
        track_id: &str,
        served_ms: i64,
        session_id: Option<&str>,
    ) -> Result<RecommendationOutcome> {
        let row = sqlx::query(
            "SELECT event_type FROM events
                 WHERE track_id = ?
                   AND event_type IN ('scrobble', 'skip')
                   AND occurred_at >= ?
                   AND occurred_at <= ?
                   AND (? IS NULL OR session_id = ?)
                 ORDER BY occurred_at ASC
                 LIMIT 1",
        )
        .bind(track_id)
        .bind(served_ms - OUTCOME_SKEW_MS)
        .bind(served_ms + OUTCOME_WINDOW_MS)
        .bind(session_id)
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(RecommendationOutcome::from_event(
            row.as_ref()
                .map(|r| r.get::<String, _>("event_type"))
                .as_deref(),
        ))
    }
}

/// Client clocks can lag the gateway slightly; allow the outcome event to
/// have occurred up to this many ms *before* the recorded serve time.
const OUTCOME_SKEW_MS: i64 = 2_000;

/// Forward window for attributing an event to a served item. A queued
/// autoplay track normally plays within minutes; this generous bound (6h)
/// keeps the session-scoped join robust and bounds the no-session
/// fallback (next / station carry no session).
const OUTCOME_WINDOW_MS: i64 = 6 * 60 * 60 * 1_000;

fn now_ms() -> i64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::EmbeddingStore;
    use serde_json::json;

    async fn store() -> RecommendationLogStore {
        let embed = EmbeddingStore::open_in_memory().await.unwrap();
        RecommendationLogStore::new(embed.pool().clone())
    }

    #[tokio::test]
    async fn record_round_trips_parent_and_items() {
        let store = store().await;
        let rec = RecommendationRecord {
            kind: RecommendationKind::FromSeeds,
            session_id: Some("sess-1".into()),
            model_version: "model-v9".into(),
            seeds: Some(vec!["a".into(), "b".into()]),
            text_query: None,
            params: json!({"per_seed_n": 20, "leash": {"tau": 0.28, "lambda": 16.0}}),
            degraded: false,
            items: vec![
                RecommendationItemRecord::new("t1", Some(0.9))
                    .with_features(json!({"seed_hits": 2, "similarity": 0.91})),
                RecommendationItemRecord::new("t2", Some(0.7))
                    .with_features(json!({"seed_hits": 1})),
            ],
        };
        let id = store.record(1, &rec).await.unwrap();
        assert!(id > 0);
        assert_eq!(store.count().await.unwrap(), 1);

        let recent = store.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        let got = &recent[0];
        assert_eq!(got.kind, RecommendationKind::FromSeeds);
        assert_eq!(got.session_id.as_deref(), Some("sess-1"));
        assert_eq!(got.model_version, "model-v9");
        assert_eq!(got.seeds, Some(vec!["a".into(), "b".into()]));
        assert_eq!(got.result_count, 2);
        assert_eq!(got.params["per_seed_n"], 20);
        // Items come back rank-ascending with score + features intact.
        assert_eq!(got.items.len(), 2);
        assert_eq!(got.items[0].entity_id, "t1");
        assert_eq!(got.items[0].score, Some(0.9));
        assert_eq!(got.items[0].features["seed_hits"], 2);
        assert_eq!(got.items[1].entity_id, "t2");
    }

    #[tokio::test]
    async fn text_station_has_no_seeds() {
        let store = store().await;
        let rec = RecommendationRecord {
            kind: RecommendationKind::Station,
            session_id: None,
            model_version: "m".into(),
            seeds: None,
            text_query: Some("rainy sunday afternoon".into()),
            params: json!({"n": 20}),
            degraded: false,
            items: vec![RecommendationItemRecord::new("t1", Some(0.5))],
        };
        store.record(1, &rec).await.unwrap();
        let got = &store.recent(1).await.unwrap()[0];
        assert_eq!(got.seeds, None);
        assert_eq!(got.text_query.as_deref(), Some("rainy sunday afternoon"));
        assert_eq!(got.kind, RecommendationKind::Station);
    }

    #[tokio::test]
    async fn empty_slate_records_zero_items() {
        let store = store().await;
        let rec = RecommendationRecord {
            kind: RecommendationKind::FromSeeds,
            session_id: None,
            model_version: "m".into(),
            seeds: Some(vec!["a".into()]),
            text_query: None,
            params: json!({}),
            degraded: true,
            items: vec![],
        };
        store.record(1, &rec).await.unwrap();
        let got = &store.recent(1).await.unwrap()[0];
        assert_eq!(got.result_count, 0);
        assert!(got.degraded);
        assert!(got.items.is_empty());
    }

    async fn insert_event(
        pool: &SqlitePool,
        event_type: &str,
        track_id: &str,
        at: i64,
        session_id: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO events (event_type, track_id, occurred_at, received_at, metadata, session_id)
             VALUES (?, ?, ?, ?, NULL, ?)",
        )
        .bind(event_type)
        .bind(track_id)
        .bind(at)
        .bind(at)
        .bind(session_id)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn outcomes_are_session_scoped() {
        let embed = EmbeddingStore::open_in_memory().await.unwrap();
        let pool = embed.pool().clone();
        let store = RecommendationLogStore::new(pool.clone());

        store
            .record(1, &RecommendationRecord {
                kind: RecommendationKind::FromSeeds,
                session_id: Some("A".into()),
                model_version: "m".into(),
                seeds: Some(vec!["seed".into()]),
                text_query: None,
                params: json!({}),
                degraded: false,
                items: vec![
                    RecommendationItemRecord::new("t_keep", Some(1.0)),
                    RecommendationItemRecord::new("t_skip", Some(0.9)),
                    RecommendationItemRecord::new("t_pending", Some(0.8)),
                ],
            })
            .await
            .unwrap();
        // record() stamps served_ms from the wall clock; anchor events to it.
        let t = store.recent(1).await.unwrap()[0].served_ms;

        // t_keep: an *earlier* skip in a DIFFERENT session (B) must NOT win —
        // session scoping ignores it, leaving the in-session scrobble → Kept.
        insert_event(&pool, "skip", "t_keep", t + 500, Some("B")).await;
        insert_event(&pool, "scrobble", "t_keep", t + 1_000, Some("A")).await;
        // t_skip: skipped in-session.
        insert_event(&pool, "skip", "t_skip", t + 800, Some("A")).await;
        // t_pending: its only event is before the serve window (beyond skew).
        insert_event(&pool, "scrobble", "t_pending", t - 60_000, Some("A")).await;

        let recs = store.recent_with_outcomes(10).await.unwrap();
        let items = &recs[0].items;
        let outcome = |id: &str| items.iter().find(|i| i.entity_id == id).unwrap().outcome;
        assert_eq!(
            outcome("t_keep"),
            Some(RecommendationOutcome::Kept),
            "cross-session skip must not override the in-session scrobble"
        );
        assert_eq!(outcome("t_skip"), Some(RecommendationOutcome::Skipped));
        assert_eq!(
            outcome("t_pending"),
            Some(RecommendationOutcome::Pending),
            "out-of-window event is ignored"
        );
    }

    #[tokio::test]
    async fn no_session_outcome_falls_back_to_track_and_window() {
        // next / station carry no session_id — the join still labels by
        // track id within the forward window.
        let embed = EmbeddingStore::open_in_memory().await.unwrap();
        let pool = embed.pool().clone();
        let store = RecommendationLogStore::new(pool.clone());
        store
            .record(1, &RecommendationRecord {
                kind: RecommendationKind::Next,
                session_id: None,
                model_version: "m".into(),
                seeds: Some(vec!["seed".into()]),
                text_query: None,
                params: json!({}),
                degraded: false,
                items: vec![RecommendationItemRecord::new("t1", Some(1.0))],
            })
            .await
            .unwrap();
        let t = store.recent(1).await.unwrap()[0].served_ms;
        insert_event(&pool, "scrobble", "t1", t + 1_000, Some("whatever")).await;
        let recs = store.recent_with_outcomes(10).await.unwrap();
        assert_eq!(recs[0].items[0].outcome, Some(RecommendationOutcome::Kept));
    }

    #[tokio::test]
    async fn similar_items_have_no_outcome() {
        let store = store().await;
        store
            .record(1, &RecommendationRecord {
                kind: RecommendationKind::SimilarAlbums,
                session_id: None,
                model_version: "m".into(),
                seeds: Some(vec!["seed".into()]),
                text_query: None,
                params: json!({}),
                degraded: false,
                items: vec![RecommendationItemRecord::new("album-1", Some(1.0))],
            })
            .await
            .unwrap();
        let recs = store.recent_with_outcomes(10).await.unwrap();
        assert_eq!(
            recs[0].items[0].outcome, None,
            "album/artist items don't join to the per-track event log"
        );
    }

    #[test]
    fn kind_round_trips_through_strings() {
        for k in [
            RecommendationKind::Next,
            RecommendationKind::Station,
            RecommendationKind::FromSeeds,
            RecommendationKind::FromAny,
            RecommendationKind::SimilarAlbums,
            RecommendationKind::SimilarArtists,
        ] {
            assert_eq!(RecommendationKind::parse(k.as_str()), k);
        }
        assert_eq!(
            RecommendationKind::parse("nonsense"),
            RecommendationKind::Other
        );
    }
}
