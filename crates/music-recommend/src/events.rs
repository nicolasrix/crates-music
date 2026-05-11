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

use music_core::TrackId;
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
                     (event_type, track_id, occurred_at, received_at, metadata)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(ev.event_type.as_str())
            .bind(ev.track_id.as_str())
            .bind(ev.occurred_at)
            .bind(received_at)
            .bind(metadata_json)
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
                "SELECT id, event_type, track_id, occurred_at, received_at, metadata
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
                "SELECT id, event_type, track_id, occurred_at, received_at, metadata
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
            "SELECT id, event_type, track_id, occurred_at, received_at, metadata
                 FROM events
                 ORDER BY id DESC
                 LIMIT ?",
        )
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
    Ok(StoredEvent {
        id: row.get("id"),
        event_type: EventType::parse(row.get::<String, _>("event_type").as_str()),
        track_id: TrackId::from(row.get::<String, _>("track_id")),
        occurred_at: row.get("occurred_at"),
        received_at: row.get("received_at"),
        metadata,
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
            },
            EventInput {
                event_type: EventType::Skip,
                track_id: TrackId::from("t2"),
                occurred_at: 1_500,
                metadata: None,
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
                },
                EventInput {
                    event_type: EventType::Skip,
                    track_id: TrackId::from("t2"),
                    occurred_at: 1_500,
                    metadata: None,
                },
                EventInput {
                    event_type: EventType::Like,
                    track_id: TrackId::from("t3"),
                    occurred_at: 2_000,
                    metadata: None,
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
            }])
            .await
            .unwrap();

        let recent = store.recently_played(1, None).await.unwrap();
        assert_eq!(recent[0].metadata.as_ref().unwrap(), &payload);
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
