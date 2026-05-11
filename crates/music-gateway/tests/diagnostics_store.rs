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

// --- recommend_summaries -------------------------------------------------
//
// Parses /v1/recommend/* spans into a typed struct ready for aggregation
// in the diagnostics handlers. Pulls only spans named `recommend.from_any`
// or `recommend.from_seeds`; everything else (sync ops, cache misses,
// etc.) is invisible to this surface.

fn recommend_span(
    name: &str,
    end_ms: i64,
    fields: &serde_json::Value,
) -> SpanRecord {
    SpanRecord {
        trace_id: format!("trace-{end_ms}"),
        span_id: end_ms,
        parent_span_id: None,
        name: name.to_string(),
        target: "music_gateway::recommend".to_string(),
        start_ms: end_ms.saturating_sub(5),
        end_ms,
        fields_json: fields.to_string(),
    }
}

#[tokio::test]
async fn recommend_summaries_empty_when_no_recommend_spans() {
    let store = TraceStore::open_in_memory().await.unwrap();
    store
        .insert_batch(vec![span("t", 1, "ingest.process_next", 0, 1)])
        .await
        .unwrap();
    let got = store.recommend_summaries(None).await.unwrap();
    assert!(got.is_empty(), "non-recommend spans must not be returned");
}

#[tokio::test]
async fn recommend_summaries_extracts_full_from_any_fields() {
    let store = TraceStore::open_in_memory().await.unwrap();
    store
        .insert_batch(vec![recommend_span(
            "recommend.from_any",
            1_000,
            &json!({
                "requested_n": 20,
                "results": 17,
                "shortfall_reason": "filter_starved_artist",
                "result_track_ids_json": "[\"t-1\",\"t-2\",\"t-3\"]",
                "filter_admitted_sims_json": "[0.840000,0.812500,0.500000]",
            }),
        )])
        .await
        .unwrap();
    let got = store.recommend_summaries(None).await.unwrap();
    assert_eq!(got.len(), 1);
    let s = &got[0];
    assert_eq!(s.name, "recommend.from_any");
    assert_eq!(s.end_ms, 1_000);
    assert_eq!(s.requested_n, Some(20));
    assert_eq!(s.results, Some(17));
    assert_eq!(
        s.shortfall_reason.as_deref(),
        Some("filter_starved_artist")
    );
    assert_eq!(s.result_track_ids, vec!["t-1", "t-2", "t-3"]);
    assert!((s.admitted_sims[0] - 0.84_f32).abs() < 1e-5);
}

#[tokio::test]
async fn recommend_summaries_tolerates_missing_optional_fields() {
    // Old span shape (pre-R1) → fields_json may not contain
    // requested_n / shortfall_reason / result_track_ids_json. The parser
    // must yield Nones / empties rather than crashing the diagnostics
    // page.
    let store = TraceStore::open_in_memory().await.unwrap();
    store
        .insert_batch(vec![recommend_span(
            "recommend.from_any",
            500,
            &json!({"results": 5}),
        )])
        .await
        .unwrap();
    let got = store.recommend_summaries(None).await.unwrap();
    assert_eq!(got.len(), 1);
    let s = &got[0];
    assert_eq!(s.results, Some(5));
    assert_eq!(s.requested_n, None);
    assert_eq!(s.shortfall_reason, None);
    assert!(s.result_track_ids.is_empty());
    assert!(s.admitted_sims.is_empty());
}

#[tokio::test]
async fn recommend_summaries_includes_both_from_any_and_from_seeds() {
    let store = TraceStore::open_in_memory().await.unwrap();
    store
        .insert_batch(vec![
            recommend_span("recommend.from_any", 100, &json!({"results": 10})),
            recommend_span("recommend.from_seeds", 200, &json!({"results": 5})),
        ])
        .await
        .unwrap();
    let got = store.recommend_summaries(None).await.unwrap();
    assert_eq!(got.len(), 2);
    let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
    // Newest first (DESC by id), matches `recent` ordering.
    assert_eq!(names, ["recommend.from_seeds", "recommend.from_any"]);
}

#[tokio::test]
async fn recommend_summaries_applies_since_ms_filter() {
    let store = TraceStore::open_in_memory().await.unwrap();
    store
        .insert_batch(vec![
            recommend_span("recommend.from_any", 100, &json!({"results": 1})),
            recommend_span("recommend.from_any", 500, &json!({"results": 2})),
            recommend_span("recommend.from_any", 900, &json!({"results": 3})),
        ])
        .await
        .unwrap();
    let got = store.recommend_summaries(Some(500)).await.unwrap();
    assert_eq!(got.len(), 2);
    let results: Vec<Option<u32>> = got.iter().map(|s| s.results).collect();
    assert_eq!(results, [Some(3), Some(2)]);
}
