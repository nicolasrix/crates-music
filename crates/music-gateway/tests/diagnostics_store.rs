//! `TraceStore` — SQLite ring buffer for `tracing` spans.
//!
//! These tests pin behavior of the storage layer in isolation: insert,
//! recall, ordering, ring-buffer trim, and field-JSON round-tripping.
//! Layer-side tracing-subscriber wiring is exercised in
//! `tests/diagnostics_layer.rs`.

use music_gateway::diagnostics::{SpanRecord, TraceStore};
use serde_json::json;

fn span(trace_id: &str, span_id: i64, name: &str, start_ms: i64, end_ms: i64) -> SpanRecord {
    SpanRecord {
        trace_id: trace_id.to_string(),
        span_id,
        parent_span_id: None,
        name: name.to_string(),
        target: "test".to_string(),
        start_ms,
        end_ms,
        fields_json: "{}".to_string(),
    }
}

#[tokio::test]
async fn fresh_store_is_empty() {
    let store = TraceStore::open_in_memory().await.unwrap();
    assert_eq!(store.count().await.unwrap(), 0);
    assert!(store.recent(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn insert_then_recent_round_trips_a_span() {
    let store = TraceStore::open_in_memory().await.unwrap();
    let s = span("trace-A", 1, "ingest.process_next", 1_000, 1_500);
    store.insert_batch(vec![s.clone()]).await.unwrap();

    let got = store.recent(10).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].trace_id, "trace-A");
    assert_eq!(got[0].span_id, 1);
    assert_eq!(got[0].name, "ingest.process_next");
    assert_eq!(got[0].start_ms, 1_000);
    assert_eq!(got[0].end_ms, 1_500);
}

#[tokio::test]
async fn fields_json_round_trips_unchanged() {
    let store = TraceStore::open_in_memory().await.unwrap();
    let mut s = span("trace-A", 1, "n", 0, 1);
    s.fields_json = json!({"track_id": "t-7", "duration": 240}).to_string();
    store.insert_batch(vec![s.clone()]).await.unwrap();

    let got = store.recent(1).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&got[0].fields_json).unwrap();
    assert_eq!(parsed["track_id"], "t-7");
    assert_eq!(parsed["duration"], 240);
}

#[tokio::test]
async fn parent_span_id_is_persisted() {
    let store = TraceStore::open_in_memory().await.unwrap();
    let mut child = span("trace-A", 2, "child", 100, 200);
    child.parent_span_id = Some(1);
    store.insert_batch(vec![child]).await.unwrap();

    let got = store.recent(1).await.unwrap();
    assert_eq!(got[0].parent_span_id, Some(1));
}

#[tokio::test]
async fn recent_orders_newest_first() {
    let store = TraceStore::open_in_memory().await.unwrap();
    // Insert in mixed order; ordering is by row id (insertion order),
    // which mirrors "the newest spans we received from the layer."
    store
        .insert_batch(vec![
            span("t", 1, "first", 0, 1),
            span("t", 2, "second", 0, 1),
            span("t", 3, "third", 0, 1),
        ])
        .await
        .unwrap();

    let got = store.recent(10).await.unwrap();
    let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["third", "second", "first"]);
}

#[tokio::test]
async fn recent_respects_limit() {
    let store = TraceStore::open_in_memory().await.unwrap();
    let batch: Vec<SpanRecord> = (0..50).map(|i| span("t", i, "n", 0, 1)).collect();
    store.insert_batch(batch).await.unwrap();

    let got = store.recent(5).await.unwrap();
    assert_eq!(got.len(), 5);
}

#[tokio::test]
async fn trim_to_capacity_keeps_only_newest() {
    let store = TraceStore::open_in_memory().await.unwrap();
    // Insert 20 rows, then trim to 5. Latest 5 (span_ids 15..=19) survive.
    let batch: Vec<SpanRecord> = (0..20).map(|i| span("t", i, "n", 0, 1)).collect();
    store.insert_batch(batch).await.unwrap();
    store.trim_to_capacity(5).await.unwrap();

    let got = store.recent(100).await.unwrap();
    assert_eq!(got.len(), 5);
    let surviving_ids: Vec<i64> = got.iter().map(|s| s.span_id).collect();
    // Newest first → span_ids 19, 18, 17, 16, 15.
    assert_eq!(surviving_ids, [19, 18, 17, 16, 15]);
}

#[tokio::test]
async fn trim_is_a_noop_when_under_capacity() {
    let store = TraceStore::open_in_memory().await.unwrap();
    let batch: Vec<SpanRecord> = (0..3).map(|i| span("t", i, "n", 0, 1)).collect();
    store.insert_batch(batch).await.unwrap();

    store.trim_to_capacity(10).await.unwrap();
    assert_eq!(store.count().await.unwrap(), 3);
}

#[tokio::test]
async fn empty_batch_inserts_nothing_without_error() {
    let store = TraceStore::open_in_memory().await.unwrap();
    store.insert_batch(Vec::new()).await.unwrap();
    assert_eq!(store.count().await.unwrap(), 0);
}
