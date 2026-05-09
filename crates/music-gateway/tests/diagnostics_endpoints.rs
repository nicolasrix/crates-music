//! Integration tests for the `/v1/diagnostics/*` HTTP surface.
//!
//! Each test pre-populates the trace store (or the embedding queue)
//! against a fresh in-memory `AppState`, then drives `build_router`
//! via `tower::ServiceExt::oneshot`. Auth is enforced by the protected
//! sub-router, so every request includes the test bearer token.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_gateway::build_router;
use music_gateway::diagnostics::SpanRecord;
use music_recommend::types::{EmbeddingKey, ModelVersion};
use serde_json::Value;
use tower::ServiceExt;

mod common;

const AUTH: &str = "Bearer test-bearer-token";

fn span(name: &str, trace_id: &str, span_id: i64, dur_ms: i64) -> SpanRecord {
    SpanRecord {
        trace_id: trace_id.to_string(),
        span_id,
        parent_span_id: None,
        name: name.to_string(),
        target: "music_recommend::ingest".to_string(),
        start_ms: 1_700_000_000_000,
        end_ms: 1_700_000_000_000 + dur_ms,
        fields_json: r#"{"track":"t-7"}"#.to_string(),
    }
}

async fn fetch_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("authorization", AUTH)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response is JSON")
    };
    (status, json)
}

// --- /v1/diagnostics/traces ------------------------------------------------

#[tokio::test]
async fn traces_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/traces")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn traces_returns_empty_array_when_store_is_empty() {
    let state = common::build_state(common::test_config()).await;
    let (status, json) = fetch_json(build_router(state), "/v1/diagnostics/traces").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["traces"], serde_json::json!([]));
}

#[tokio::test]
async fn traces_returns_inserted_spans_newest_first_with_derived_fields() {
    let state = common::build_state(common::test_config()).await;
    state
        .trace_store()
        .insert_batch(vec![
            span("ingest.embed_one", "trace-a", 1, 100),
            span("ingest.embed_one", "trace-b", 2, 250),
        ])
        .await
        .unwrap();

    let (status, json) = fetch_json(build_router(state), "/v1/diagnostics/traces").await;
    assert_eq!(status, StatusCode::OK);
    let traces = json["traces"].as_array().unwrap();
    assert_eq!(traces.len(), 2);
    // Newest first: trace-b inserted second, so it lands at index 0.
    assert_eq!(traces[0]["trace_id"], "trace-b");
    assert_eq!(traces[0]["duration_ms"], 250);
    // fields_json parsed back into an object.
    assert_eq!(traces[0]["fields"]["track"], "t-7");
}

#[tokio::test]
async fn traces_filter_by_name_excludes_other_spans() {
    let state = common::build_state(common::test_config()).await;
    state
        .trace_store()
        .insert_batch(vec![
            span("ingest.embed_one", "a", 1, 50),
            span("ann.upsert", "b", 2, 5),
            span("ingest.embed_one", "c", 3, 75),
        ])
        .await
        .unwrap();

    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/traces?name=ingest.embed_one",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let traces = json["traces"].as_array().unwrap();
    assert_eq!(traces.len(), 2);
    assert!(
        traces
            .iter()
            .all(|t| t["name"] == "ingest.embed_one"),
        "every returned span should match the name filter"
    );
}

#[tokio::test]
async fn traces_filter_by_since_ms_excludes_older_spans() {
    let state = common::build_state(common::test_config()).await;
    let mut old = span("ingest.embed_one", "old", 1, 10);
    old.start_ms = 1_000;
    old.end_ms = 1_500;
    let mut new = span("ingest.embed_one", "new", 2, 10);
    new.start_ms = 2_000;
    new.end_ms = 2_500;
    state
        .trace_store()
        .insert_batch(vec![old, new])
        .await
        .unwrap();

    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/traces?since_ms=2000",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let traces = json["traces"].as_array().unwrap();
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0]["trace_id"], "new");
}

#[tokio::test]
async fn traces_clamps_limit_to_max() {
    let state = common::build_state(common::test_config()).await;
    let many: Vec<SpanRecord> = (0..50)
        .map(|i| span("noise", &format!("t-{i}"), i, 5))
        .collect();
    state.trace_store().insert_batch(many).await.unwrap();

    // Request a stupid large limit; server should clamp internally and
    // still succeed.
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/traces?limit=999999999",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let traces = json["traces"].as_array().unwrap();
    // Inserted 50, clamp is 1000, so all 50 come back without panic.
    assert_eq!(traces.len(), 50);
}

#[tokio::test]
async fn traces_field_parse_failure_falls_back_to_raw() {
    let state = common::build_state(common::test_config()).await;
    let mut s = span("ingest.embed_one", "trace-broken", 1, 10);
    s.fields_json = "not json at all".to_string();
    state.trace_store().insert_batch(vec![s]).await.unwrap();

    let (status, json) = fetch_json(build_router(state), "/v1/diagnostics/traces").await;
    assert_eq!(status, StatusCode::OK);
    let entry = &json["traces"][0];
    assert_eq!(entry["fields"]["_raw"], "not json at all");
}

// --- /v1/diagnostics/histogram --------------------------------------------

#[tokio::test]
async fn histogram_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/histogram")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn histogram_returns_empty_when_no_spans() {
    let state = common::build_state(common::test_config()).await;
    let (status, json) = fetch_json(build_router(state), "/v1/diagnostics/histogram").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["buckets"], serde_json::json!([]));
}

#[tokio::test]
async fn histogram_groups_by_name_and_reports_p50_p95_p99() {
    let state = common::build_state(common::test_config()).await;
    // 100 spans of `embed_one` with durations 1..=100 — easy quantiles
    // to verify (p50 = 50, p95 = 95, p99 = 99 by nearest-rank).
    let batch: Vec<SpanRecord> = (1..=100)
        .map(|d| span("ingest.embed_one", &format!("t-{d}"), d, d))
        .collect();
    state.trace_store().insert_batch(batch).await.unwrap();

    let (status, json) = fetch_json(build_router(state), "/v1/diagnostics/histogram").await;
    assert_eq!(status, StatusCode::OK);
    let buckets = json["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 1);
    let b = &buckets[0];
    assert_eq!(b["name"], "ingest.embed_one");
    assert_eq!(b["count"], 100);
    assert_eq!(b["min_ms"], 1);
    assert_eq!(b["max_ms"], 100);
    assert_eq!(b["p50_ms"], 50);
    assert_eq!(b["p95_ms"], 95);
    assert_eq!(b["p99_ms"], 99);
}

// --- /v1/diagnostics/queue_depth ------------------------------------------

#[tokio::test]
async fn queue_depth_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/queue_depth")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn queue_depth_uses_default_model_version_when_unspecified() {
    let state = common::build_state(common::test_config()).await;
    // Test fixture sets the default to "test-v1".
    let model = state.recommend_model_version().clone();
    state
        .embedding_store()
        .enqueue(&EmbeddingKey::new("track-1", model.clone()))
        .await
        .unwrap();
    state
        .embedding_store()
        .enqueue(&EmbeddingKey::new("track-2", model.clone()))
        .await
        .unwrap();

    let (status, json) = fetch_json(build_router(state), "/v1/diagnostics/queue_depth").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["model_version"], model.as_str());
    assert_eq!(json["not_started"], 2);
    assert_eq!(json["in_progress"], 0);
    assert_eq!(json["done"], 0);
    assert_eq!(json["failed"], 0);
}

#[tokio::test]
async fn queue_depth_filters_by_explicit_model_version() {
    let state = common::build_state(common::test_config()).await;
    let v1 = ModelVersion::from("v1");
    let v2 = ModelVersion::from("v2");
    state
        .embedding_store()
        .enqueue(&EmbeddingKey::new("track-1", v1.clone()))
        .await
        .unwrap();
    state
        .embedding_store()
        .enqueue(&EmbeddingKey::new("track-2", v2.clone()))
        .await
        .unwrap();
    state
        .embedding_store()
        .enqueue(&EmbeddingKey::new("track-3", v2.clone()))
        .await
        .unwrap();

    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/queue_depth?model_version=v2",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["model_version"], "v2");
    assert_eq!(json["not_started"], 2);
}
