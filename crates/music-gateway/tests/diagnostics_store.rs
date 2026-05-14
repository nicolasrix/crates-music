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

// --- span_series ---------------------------------------------------------
//
// Time-series of (end_ms, duration_ms) for a single span name. Drives
// the per-name plot on the /diagnostics/tracing page.

#[tokio::test]
async fn span_series_returns_points_for_matching_name() {
    let store = TraceStore::open_in_memory().await.unwrap();
    store
        .insert_batch(vec![
            span("t", 1, "ingest.fetch_clip", 100, 200), // dur=100
            span("t", 2, "ingest.fetch_clip", 300, 500), // dur=200
            span("t", 3, "other.span", 0, 10),
        ])
        .await
        .unwrap();
    let pts = store
        .span_series("ingest.fetch_clip", None, 100)
        .await
        .unwrap();
    assert_eq!(pts.len(), 2);
    // Newest first.
    assert_eq!(pts[0].end_ms, 500);
    assert_eq!(pts[0].duration_ms, 200);
    assert_eq!(pts[1].end_ms, 200);
    assert_eq!(pts[1].duration_ms, 100);
}

#[tokio::test]
async fn span_series_applies_since_ms_filter() {
    let store = TraceStore::open_in_memory().await.unwrap();
    store
        .insert_batch(vec![
            span("t", 1, "x", 0, 100),
            span("t", 2, "x", 0, 500),
            span("t", 3, "x", 0, 900),
        ])
        .await
        .unwrap();
    let pts = store.span_series("x", Some(500), 100).await.unwrap();
    let ends: Vec<i64> = pts.iter().map(|p| p.end_ms).collect();
    assert_eq!(ends, [900, 500]);
}

#[tokio::test]
async fn span_series_respects_limit() {
    let store = TraceStore::open_in_memory().await.unwrap();
    let batch: Vec<SpanRecord> = (0..20)
        .map(|i| span("t", i, "x", 0, i * 10))
        .collect();
    store.insert_batch(batch).await.unwrap();
    let pts = store.span_series("x", None, 5).await.unwrap();
    assert_eq!(pts.len(), 5);
}

#[tokio::test]
async fn span_series_empty_when_no_match() {
    let store = TraceStore::open_in_memory().await.unwrap();
    store
        .insert_batch(vec![span("t", 1, "x", 0, 10)])
        .await
        .unwrap();
    let pts = store.span_series("nope", None, 10).await.unwrap();
    assert!(pts.is_empty());
}

// --- child_breakdown -----------------------------------------------------
//
// Aggregates the children of every span named `<parent_name>` into a
// per-child-name summary. Drives the "where did the time go?" panel on
// the expanded histogram row.

fn nested_span(
    trace_id: &str,
    span_id: i64,
    parent_span_id: Option<i64>,
    name: &str,
    start_ms: i64,
    end_ms: i64,
) -> SpanRecord {
    SpanRecord {
        trace_id: trace_id.to_string(),
        span_id,
        parent_span_id,
        name: name.to_string(),
        target: "test".to_string(),
        start_ms,
        end_ms,
        fields_json: "{}".to_string(),
    }
}

#[tokio::test]
async fn child_breakdown_aggregates_children_per_name() {
    let store = TraceStore::open_in_memory().await.unwrap();
    // Parent "ingest.fetch_clip" with three children: get_song (10),
    // stream_request (20), stream_body (970). Sum = 1000.
    store
        .insert_batch(vec![
            nested_span("t1", 1, None, "ingest.fetch_clip", 0, 1000),
            nested_span("t1", 2, Some(1), "fetch_clip.get_song", 0, 10),
            nested_span("t1", 3, Some(1), "fetch_clip.stream_request", 10, 30),
            nested_span("t1", 4, Some(1), "fetch_clip.stream_body", 30, 1000),
            // Second parent run.
            nested_span("t2", 5, None, "ingest.fetch_clip", 0, 2000),
            nested_span("t2", 6, Some(5), "fetch_clip.get_song", 0, 20),
            nested_span("t2", 7, Some(5), "fetch_clip.stream_body", 20, 2000),
        ])
        .await
        .unwrap();

    let breakdown = store
        .child_breakdown("ingest.fetch_clip", None)
        .await
        .unwrap();
    assert_eq!(breakdown.parent_count, 2);
    assert_eq!(breakdown.parent_sum_ms, 1000 + 2000);

    // Aggregate per child name.
    let by_name: std::collections::HashMap<&str, &music_gateway::diagnostics::ChildAgg> = breakdown
        .children
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();
    assert_eq!(by_name["fetch_clip.get_song"].count, 2);
    assert_eq!(by_name["fetch_clip.get_song"].sum_ms, 10 + 20);
    assert_eq!(by_name["fetch_clip.stream_body"].count, 2);
    assert_eq!(by_name["fetch_clip.stream_body"].sum_ms, 970 + 1980);
    assert_eq!(by_name["fetch_clip.stream_request"].count, 1);
}

#[tokio::test]
async fn child_breakdown_empty_when_parent_has_no_instances() {
    let store = TraceStore::open_in_memory().await.unwrap();
    store
        .insert_batch(vec![nested_span("t", 1, None, "unrelated", 0, 5)])
        .await
        .unwrap();
    let r = store.child_breakdown("ingest.fetch_clip", None).await.unwrap();
    assert_eq!(r.parent_count, 0);
    assert_eq!(r.parent_sum_ms, 0);
    assert!(r.children.is_empty());
}

#[tokio::test]
async fn child_breakdown_excludes_grandchildren() {
    // The breakdown is one level deep — direct children only. A
    // grandchild span shouldn't be double-counted under its grandparent.
    let store = TraceStore::open_in_memory().await.unwrap();
    store
        .insert_batch(vec![
            nested_span("t", 1, None, "parent", 0, 100),
            nested_span("t", 2, Some(1), "child", 0, 80),
            nested_span("t", 3, Some(2), "grandchild", 0, 70),
        ])
        .await
        .unwrap();
    let r = store.child_breakdown("parent", None).await.unwrap();
    let names: Vec<&str> = r.children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["child"], "grandchild must not appear");
}

#[tokio::test]
async fn child_breakdown_applies_since_ms_to_parent_only() {
    let store = TraceStore::open_in_memory().await.unwrap();
    // Old parent (end=100): excluded by since_ms=500.
    // New parent (end=1000): included; its children counted.
    store
        .insert_batch(vec![
            nested_span("t", 1, None, "p", 0, 100),
            nested_span("t", 2, Some(1), "c", 0, 100),
            nested_span("t", 3, None, "p", 900, 1000),
            nested_span("t", 4, Some(3), "c", 900, 1000),
        ])
        .await
        .unwrap();
    let r = store.child_breakdown("p", Some(500)).await.unwrap();
    assert_eq!(r.parent_count, 1);
    assert_eq!(r.children.len(), 1);
    assert_eq!(r.children[0].count, 1);
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
