//! Append-only event log.
//!
//! Captures user-interaction signal (scrobble, skip, like/unlike, seek)
//! that the behavioural-similarity index will consume in a later
//! phase. For now this is pure persistence — no consumers, no
//! aggregation. The point is to not lose data while the recommender
//! is being built.
//!
//! Writes are pure append: clients are expected to coalesce locally
//! and POST in batches every few seconds. Duplicates are accepted —
//! deduping happens at consumer time when we know which signal
//! interpretation the consumer wants.

use std::time::{SystemTime, UNIX_EPOCH};

use music_core::{SessionId, TrackId};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::{Error, Result};

/// Recognised event kinds. Stringly-typed in the DB so we can add new
/// variants without a migration. Unknown variants on read parse to
/// `Other`, so the column is forward-compatible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Scrobble,
    Skip,
    Like,
    Unlike,
    Seek,
    /// Anything else the client wants to record. Stored verbatim.
    #[serde(untagged)]
    Other(String),
}

impl EventType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Scrobble => "scrobble",
            Self::Skip => "skip",
            Self::Like => "like",
            Self::Unlike => "unlike",
            Self::Seek => "seek",
            Self::Other(s) => s.as_str(),
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "scrobble" => Self::Scrobble,
            "skip" => Self::Skip,
            "like" => Self::Like,
            "unlike" => Self::Unlike,
            "seek" => Self::Seek,
            other => Self::Other(other.to_string()),
        }
    }
}

/// One user-interaction event. `metadata` is opaque JSON — the
/// recommender will pick out type-specific fields (e.g. `played_ms`
/// for scrobble, `seek_to_ms` for seek) when it lands.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventInput {
    pub event_type: EventType,
    pub track_id: TrackId,
    /// Client-supplied unix milliseconds.
    pub occurred_at: i64,
    /// Optional opaque JSON. Default: `null`.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// Active recommend-session at write time. `None` means the event
    /// fired outside any session (or the caller hasn't been wired to
    /// stamp it yet); persisted as SQL NULL.
    #[serde(default)]
    pub session_id: Option<SessionId>,
}

/// A persisted event with the gateway-assigned id and `received_at`
/// timestamp. Returned by `list_*` / `get` for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEvent {
    pub id: i64,
    pub event_type: EventType,
    pub track_id: TrackId,
    pub occurred_at: i64,
    pub received_at: i64,
    pub metadata: Option<serde_json::Value>,
    pub session_id: Option<SessionId>,
}

#[derive(Clone, Debug)]
pub struct EventStore {
    pool: SqlitePool,
}

impl EventStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Append a batch of events. Returns the number persisted.
    /// All-or-nothing within a single transaction so partial batches
    /// don't sneak in on a SQLite hiccup.
    pub async fn append_batch(&self, events: &[EventInput]) -> Result<u64> {
        if events.is_empty() {
            return Ok(0);
        }
        let received_at = now_ms();
        let mut tx = self.pool.begin().await?;
        let mut count = 0u64;
        for ev in events {
            let metadata_json = ev
                .metadata
                .as_ref()
                .map(|v| serde_json::to_string(v).expect("Value serializes"));
            sqlx::query(
                "INSERT INTO events
                     (event_type, track_id, occurred_at, received_at, metadata, session_id)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(ev.event_type.as_str())
            .bind(ev.track_id.as_str())
            .bind(ev.occurred_at)
            .bind(received_at)
            .bind(metadata_json)
            .bind(ev.session_id.as_ref().map(SessionId::as_str))
            .execute(&mut *tx)
            .await?;
            count += 1;
        }
        tx.commit().await?;
        Ok(count)
    }

    /// Total number of events persisted. Diagnostic.
    pub async fn count(&self) -> Result<u64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM events")
            .fetch_one(&self.pool)
            .await?;
        let n: i64 = row.get("n");
        Ok(u64::try_from(n.max(0)).unwrap_or(0))
    }

    /// Recently scrobbled tracks, newest first by `occurred_at`.
    ///
    /// Filters server-side to `event_type = 'scrobble'` and optionally
    /// bounds by `occurred_at >= since_ms`. Ordering is by the
    /// user-facing clock (`occurred_at`), tiebroken by `id DESC` for
    /// stability when two scrobbles arrive in the same millisecond.
    /// Diagnostic-only — used to eyeball recency-window defaults
    /// before MMR's recency penalty consumes the same column.
    pub async fn recently_played(
        &self,
        limit: u32,
        since_ms: Option<i64>,
    ) -> Result<Vec<StoredEvent>> {
        let rows = if let Some(since) = since_ms {
            sqlx::query(
                "SELECT id, event_type, track_id, occurred_at, received_at, metadata, session_id
                     FROM events
                     WHERE event_type = 'scrobble' AND occurred_at >= ?
                     ORDER BY occurred_at DESC, id DESC
                     LIMIT ?",
            )
            .bind(since)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, event_type, track_id, occurred_at, received_at, metadata, session_id
                     FROM events
                     WHERE event_type = 'scrobble'
                     ORDER BY occurred_at DESC, id DESC
                     LIMIT ?",
            )
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await?
        };

        rows.iter().map(row_to_stored).collect()
    }

    /// Most recent `limit` events, newest first. Diagnostic / debug.
    pub async fn recent(&self, limit: u32) -> Result<Vec<StoredEvent>> {
        let rows = sqlx::query(
            "SELECT id, event_type, track_id, occurred_at, received_at, metadata, session_id
                 FROM events
                 ORDER BY id DESC
                 LIMIT ?",
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(row_to_stored).collect()
    }

    /// Per-session event counts for the given session ids. Used by
    /// `/v1/diagnostics/recommend/sessions` to show how much signal
    /// each session generated. Missing keys (no events at all) are
    /// not present in the returned map — callers default to 0.
    /// Single SELECT with GROUP BY; N+1 query avoidance.
    pub async fn count_events_per_session(
        &self,
        session_ids: &[SessionId],
    ) -> Result<std::collections::HashMap<SessionId, i64>> {
        let mut out = std::collections::HashMap::new();
        if session_ids.is_empty() {
            return Ok(out);
        }
        // Build a parameterised "WHERE session_id IN (?, ?, …)". sqlx
        // doesn't expand `Vec` natively for SQLite — inline the right
        // number of placeholders by hand.
        let placeholders = std::iter::repeat_n("?", session_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT session_id, COUNT(*) AS n
                 FROM events
                 WHERE session_id IN ({placeholders})
                 GROUP BY session_id"
        );
        let mut query = sqlx::query(&sql);
        for sid in session_ids {
            query = query.bind(sid.as_str());
        }
        let rows = query.fetch_all(&self.pool).await?;
        for row in rows {
            let sid: String = row.get("session_id");
            let n: i64 = row.get("n");
            out.insert(SessionId::from(sid), n);
        }
        Ok(out)
    }

    /// All events stamped with the given `session_id`, oldest first by
    /// `occurred_at`. This is the per-session reconstruction primitive —
    /// the diagnostic story behind a session ("what did the user do
    /// during s_abc?") is one call to this method. `id ASC` tiebreaks
    /// same-millisecond events stably.
    pub async fn by_session(&self, session_id: &SessionId, limit: u32) -> Result<Vec<StoredEvent>> {
        let rows = sqlx::query(
            "SELECT id, event_type, track_id, occurred_at, received_at, metadata, session_id
                 FROM events
                 WHERE session_id = ?
                 ORDER BY occurred_at ASC, id ASC
                 LIMIT ?",
        )
        .bind(session_id.as_str())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(row_to_stored).collect()
    }
}

fn row_to_stored(row: &sqlx::sqlite::SqliteRow) -> Result<StoredEvent> {
    let metadata: Option<String> = row.get("metadata");
    let metadata = metadata
        .map(|s| serde_json::from_str(&s))
        .transpose()
        .map_err(|e| Error::InvalidStatus(format!("metadata json: {e}")))?;
    let session_id: Option<String> = row.get("session_id");
    Ok(StoredEvent {
        id: row.get("id"),
        event_type: EventType::parse(row.get::<String, _>("event_type").as_str()),
        track_id: TrackId::from(row.get::<String, _>("track_id")),
        occurred_at: row.get("occurred_at"),
        received_at: row.get("received_at"),
        metadata,
        session_id: session_id.map(SessionId::from),
    })
}

fn now_ms() -> i64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MIGRATIONS;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> SqlitePool {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        MIGRATIONS.run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn append_and_count() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);

        assert_eq!(store.count().await.unwrap(), 0);

        let events = vec![
            EventInput {
                event_type: EventType::Scrobble,
                track_id: TrackId::from("t1"),
                occurred_at: 1_000,
                metadata: Some(serde_json::json!({"played_ms": 180_000})),
                session_id: None,
            },
            EventInput {
                event_type: EventType::Skip,
                track_id: TrackId::from("t2"),
                occurred_at: 1_500,
                metadata: None,
                session_id: None,
            },
        ];
        let n = store.append_batch(&events).await.unwrap();
        assert_eq!(n, 2);
        assert_eq!(store.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn empty_batch_is_noop() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);
        assert_eq!(store.append_batch(&[]).await.unwrap(), 0);
        assert_eq!(store.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn recent_returns_newest_first() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);

        for i in 0..5 {
            store
                .append_batch(&[EventInput {
                    event_type: EventType::Scrobble,
                    track_id: TrackId::from(format!("t{i}")),
                    occurred_at: 1_000 + i64::from(i),
                    metadata: None,
                    session_id: None,
                }])
                .await
                .unwrap();
        }

        let recent = store.recent(3).await.unwrap();
        assert_eq!(recent.len(), 3);
        // Newest first: t4, t3, t2
        assert_eq!(recent[0].track_id.as_str(), "t4");
        assert_eq!(recent[2].track_id.as_str(), "t2");
    }

    #[tokio::test]
    async fn metadata_roundtrips() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);

        let payload = serde_json::json!({"seek_to_ms": 30_500, "from_ms": 0});
        store
            .append_batch(&[EventInput {
                event_type: EventType::Seek,
                track_id: TrackId::from("t1"),
                occurred_at: 5_000,
                metadata: Some(payload.clone()),
                session_id: None,
            }])
            .await
            .unwrap();

        let recent = store.recent(1).await.unwrap();
        assert_eq!(recent[0].metadata.as_ref().unwrap(), &payload);
    }

    #[tokio::test]
    async fn unknown_event_type_roundtrips_via_other() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);

        store
            .append_batch(&[EventInput {
                event_type: EventType::Other("hover".to_string()),
                track_id: TrackId::from("t1"),
                occurred_at: 5_000,
                metadata: None,
                session_id: None,
            }])
            .await
            .unwrap();

        let recent = store.recent(1).await.unwrap();
        assert_eq!(recent[0].event_type, EventType::Other("hover".to_string()),);
    }

    #[tokio::test]
    async fn batch_is_atomic() {
        // Truncating the underlying file mid-batch is hard to simulate,
        // but we can at least assert that append_batch presents one
        // consistent count.
        let pool = test_pool().await;
        let store = EventStore::new(pool);

        let mut events = Vec::new();
        for i in 0..50 {
            events.push(EventInput {
                event_type: EventType::Scrobble,
                track_id: TrackId::from(format!("t{i}")),
                occurred_at: i64::from(i),
                metadata: None,
                session_id: None,
            });
        }
        store.append_batch(&events).await.unwrap();
        assert_eq!(store.count().await.unwrap(), 50);
    }

    #[tokio::test]
    async fn recently_played_filters_to_scrobble_only() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);

        store
            .append_batch(&[
                EventInput {
                    event_type: EventType::Scrobble,
                    track_id: TrackId::from("t1"),
                    occurred_at: 1_000,
                    metadata: None,
                    session_id: None,
                },
                EventInput {
                    event_type: EventType::Skip,
                    track_id: TrackId::from("t2"),
                    occurred_at: 1_500,
                    metadata: None,
                    session_id: None,
                },
                EventInput {
                    event_type: EventType::Like,
                    track_id: TrackId::from("t3"),
                    occurred_at: 2_000,
                    metadata: None,
                    session_id: None,
                },
            ])
            .await
            .unwrap();

        let recent = store.recently_played(10, None).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].track_id.as_str(), "t1");
        assert_eq!(recent[0].event_type, EventType::Scrobble);
    }

    #[tokio::test]
    async fn recently_played_orders_by_occurred_at_desc() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);

        // Insert in non-monotonic occurred_at order — out-of-order
        // delivery is the realistic case after offline batches.
        for occurred in [3_000_i64, 1_000, 5_000, 2_000, 4_000] {
            store
                .append_batch(&[EventInput {
                    event_type: EventType::Scrobble,
                    track_id: TrackId::from(format!("t-{occurred}")),
                    occurred_at: occurred,
                    metadata: None,
                    session_id: None,
                }])
                .await
                .unwrap();
        }

        let recent = store.recently_played(10, None).await.unwrap();
        let occurred: Vec<i64> = recent.iter().map(|e| e.occurred_at).collect();
        assert_eq!(occurred, vec![5_000, 4_000, 3_000, 2_000, 1_000]);
    }

    #[tokio::test]
    async fn recently_played_respects_limit() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);

        for i in 0..10 {
            store
                .append_batch(&[EventInput {
                    event_type: EventType::Scrobble,
                    track_id: TrackId::from(format!("t{i}")),
                    occurred_at: 1_000 + i64::from(i),
                    metadata: None,
                    session_id: None,
                }])
                .await
                .unwrap();
        }

        let recent = store.recently_played(3, None).await.unwrap();
        assert_eq!(recent.len(), 3);
        // Newest 3: t9, t8, t7
        assert_eq!(recent[0].track_id.as_str(), "t9");
        assert_eq!(recent[2].track_id.as_str(), "t7");
    }

    #[tokio::test]
    async fn recently_played_filters_by_since_ms() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);

        for occurred in [1_000_i64, 2_000, 3_000, 4_000, 5_000] {
            store
                .append_batch(&[EventInput {
                    event_type: EventType::Scrobble,
                    track_id: TrackId::from(format!("t-{occurred}")),
                    occurred_at: occurred,
                    metadata: None,
                    session_id: None,
                }])
                .await
                .unwrap();
        }

        // since_ms is inclusive: 3_000 → t-3000, t-4000, t-5000.
        let recent = store.recently_played(10, Some(3_000)).await.unwrap();
        assert_eq!(recent.len(), 3);
        assert!(recent.iter().all(|e| e.occurred_at >= 3_000));
    }

    #[tokio::test]
    async fn recently_played_empty_when_only_non_scrobble_events() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);

        store
            .append_batch(&[EventInput {
                event_type: EventType::Skip,
                track_id: TrackId::from("t1"),
                occurred_at: 1_000,
                metadata: None,
                session_id: None,
            }])
            .await
            .unwrap();

        let recent = store.recently_played(10, None).await.unwrap();
        assert!(recent.is_empty());
    }

    #[tokio::test]
    async fn recently_played_preserves_metadata() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);

        let payload = serde_json::json!({"played_ms": 180_000});
        store
            .append_batch(&[EventInput {
                event_type: EventType::Scrobble,
                track_id: TrackId::from("t1"),
                occurred_at: 1_000,
                metadata: Some(payload.clone()),
                session_id: None,
            }])
            .await
            .unwrap();

        let recent = store.recently_played(1, None).await.unwrap();
        assert_eq!(recent[0].metadata.as_ref().unwrap(), &payload);
    }

    #[tokio::test]
    async fn session_id_round_trips() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);
        store
            .append_batch(&[EventInput {
                event_type: EventType::Scrobble,
                track_id: TrackId::from("t1"),
                occurred_at: 1_000,
                metadata: None,
                session_id: Some(SessionId::from("s-abc")),
            }])
            .await
            .unwrap();
        let recent = store.recent(1).await.unwrap();
        assert_eq!(recent[0].session_id.as_ref().unwrap().as_str(), "s-abc");
    }

    #[tokio::test]
    async fn missing_session_id_stores_null_and_round_trips_to_none() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);
        store
            .append_batch(&[EventInput {
                event_type: EventType::Scrobble,
                track_id: TrackId::from("t1"),
                occurred_at: 1_000,
                metadata: None,
                session_id: None,
            }])
            .await
            .unwrap();
        let recent = store.recent(1).await.unwrap();
        assert!(recent[0].session_id.is_none());
    }

    #[tokio::test]
    async fn by_session_returns_only_matching_session_ordered_oldest_first() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);
        let s1 = SessionId::from("s1");
        let s2 = SessionId::from("s2");
        // Interleaved insert order shouldn't affect output order.
        store
            .append_batch(&[
                EventInput {
                    event_type: EventType::Scrobble,
                    track_id: TrackId::from("a"),
                    occurred_at: 3_000,
                    metadata: None,
                    session_id: Some(s1.clone()),
                },
                EventInput {
                    event_type: EventType::Skip,
                    track_id: TrackId::from("b"),
                    occurred_at: 1_000,
                    metadata: None,
                    session_id: Some(s1.clone()),
                },
                EventInput {
                    event_type: EventType::Scrobble,
                    track_id: TrackId::from("c"),
                    occurred_at: 2_000,
                    metadata: None,
                    session_id: Some(s2.clone()),
                },
                EventInput {
                    event_type: EventType::Scrobble,
                    track_id: TrackId::from("d"),
                    occurred_at: 4_000,
                    metadata: None,
                    session_id: None,
                },
            ])
            .await
            .unwrap();
        let s1_events = store.by_session(&s1, 100).await.unwrap();
        let tracks: Vec<&str> = s1_events.iter().map(|e| e.track_id.as_str()).collect();
        assert_eq!(tracks, vec!["b", "a"], "ordered by occurred_at ASC");
        let s2_events = store.by_session(&s2, 100).await.unwrap();
        assert_eq!(s2_events.len(), 1);
        assert_eq!(s2_events[0].track_id.as_str(), "c");
    }

    #[tokio::test]
    async fn count_events_per_session_aggregates_correctly() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);
        let s1 = SessionId::from("s1");
        let s2 = SessionId::from("s2");
        store
            .append_batch(&[
                EventInput {
                    event_type: EventType::Scrobble,
                    track_id: TrackId::from("a"),
                    occurred_at: 1,
                    metadata: None,
                    session_id: Some(s1.clone()),
                },
                EventInput {
                    event_type: EventType::Skip,
                    track_id: TrackId::from("b"),
                    occurred_at: 2,
                    metadata: None,
                    session_id: Some(s1.clone()),
                },
                EventInput {
                    event_type: EventType::Scrobble,
                    track_id: TrackId::from("c"),
                    occurred_at: 3,
                    metadata: None,
                    session_id: Some(s2.clone()),
                },
                EventInput {
                    // NULL session_id should not count against either bucket.
                    event_type: EventType::Scrobble,
                    track_id: TrackId::from("d"),
                    occurred_at: 4,
                    metadata: None,
                    session_id: None,
                },
            ])
            .await
            .unwrap();
        let counts = store
            .count_events_per_session(&[s1.clone(), s2.clone()])
            .await
            .unwrap();
        assert_eq!(counts.get(&s1).copied(), Some(2));
        assert_eq!(counts.get(&s2).copied(), Some(1));
    }

    #[tokio::test]
    async fn count_events_per_session_empty_input_returns_empty_map() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);
        assert!(
            store
                .count_events_per_session(&[])
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn count_events_per_session_omits_sessions_with_zero_events() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);
        let counts = store
            .count_events_per_session(&[SessionId::from("nobody")])
            .await
            .unwrap();
        assert!(
            !counts.contains_key(&SessionId::from("nobody")),
            "zero-event sessions are absent from the map (callers default to 0)"
        );
    }

    #[tokio::test]
    async fn by_session_returns_empty_for_unknown_session() {
        let pool = test_pool().await;
        let store = EventStore::new(pool);
        let rows = store
            .by_session(&SessionId::from("never"), 10)
            .await
            .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn event_input_deserializes_with_legacy_payload_missing_session_id() {
        // Backwards compat: pre-0007 clients POST without session_id.
        // serde default must kick in, not 422 the request.
        let parsed: EventInput =
            serde_json::from_str(r#"{"event_type":"scrobble","track_id":"t1","occurred_at":1000}"#)
                .expect("parses without session_id");
        assert!(parsed.session_id.is_none());
    }

    #[test]
    fn event_type_serde() {
        // Standard variants serialize as snake_case strings.
        let s = serde_json::to_string(&EventType::Scrobble).unwrap();
        assert_eq!(s, r#""scrobble""#);

        let parsed: EventType = serde_json::from_str(r#""skip""#).unwrap();
        assert_eq!(parsed, EventType::Skip);

        // Unknown strings deserialize to Other (via the untagged variant).
        let parsed: EventType = serde_json::from_str(r#""hover""#).unwrap();
        assert_eq!(parsed, EventType::Other("hover".to_string()));
    }
}
