//! Integration tests for the `/v1/diagnostics/*` HTTP surface.
//!
//! Each test pre-populates the trace store (or the embedding queue)
//! against a fresh in-memory `AppState`, then drives `build_router`
//! via `tower::ServiceExt::oneshot`. Auth is enforced by the protected
//! sub-router, so every request includes the test bearer token.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_core::TrackId;
use music_gateway::build_router;
use music_gateway::diagnostics::SpanRecord;
use music_recommend::types::{EmbeddingKey, ModelVersion};
use music_recommend::{EventInput, EventType};
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
        traces.iter().all(|t| t["name"] == "ingest.embed_one"),
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

    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/traces?since_ms=2000").await;
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

// --- /v1/diagnostics/span_series ------------------------------------------

#[tokio::test]
async fn span_series_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/span_series?name=x")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn span_series_returns_points_newest_first() {
    let state = common::build_state(common::test_config()).await;
    state
        .trace_store()
        .insert_batch(vec![
            span("ingest.fetch_clip", "t-1", 1, 100),
            span("ingest.fetch_clip", "t-2", 2, 250),
            span("other.span", "t-3", 3, 5),
        ])
        .await
        .unwrap();

    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/span_series?name=ingest.fetch_clip",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["name"], "ingest.fetch_clip");
    let pts = json["points"].as_array().unwrap();
    assert_eq!(pts.len(), 2);
    // Newest first.
    assert_eq!(pts[0]["duration_ms"], 250);
    assert_eq!(pts[1]["duration_ms"], 100);
    assert!(pts[0]["end_ms"].as_i64().unwrap() >= pts[1]["end_ms"].as_i64().unwrap());
}

#[tokio::test]
async fn span_series_filters_by_since_ms() {
    let state = common::build_state(common::test_config()).await;
    // Span 1 ends at 1.7e12; build two so we can filter past the first.
    let s1 = span("x", "t-1", 1, 5); // end_ms = base + 5
    let mut s2 = span("x", "t-2", 2, 5);
    s2.end_ms = s1.end_ms + 1_000; // 1 s newer
    let cutoff = s1.end_ms + 100;
    state
        .trace_store()
        .insert_batch(vec![s1, s2])
        .await
        .unwrap();

    let (status, json) = fetch_json(
        build_router(state),
        &format!("/v1/diagnostics/span_series?name=x&since_ms={cutoff}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pts = json["points"].as_array().unwrap();
    assert_eq!(pts.len(), 1);
}

#[tokio::test]
async fn span_series_missing_name_param_400s() {
    let state = common::build_state(common::test_config()).await;
    let resp = build_router(state)
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/span_series")
                .header("authorization", AUTH)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// --- /v1/diagnostics/span_children ---------------------------------------

fn span_with_parent(
    name: &str,
    trace_id: &str,
    span_id: i64,
    parent_span_id: Option<i64>,
    dur_ms: i64,
) -> SpanRecord {
    let mut s = span(name, trace_id, span_id, dur_ms);
    s.parent_span_id = parent_span_id;
    s
}

#[tokio::test]
async fn span_children_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let resp = build_router(state)
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/span_children?name=x")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn span_children_aggregates_under_parent() {
    let state = common::build_state(common::test_config()).await;
    state
        .trace_store()
        .insert_batch(vec![
            span_with_parent("ingest.fetch_clip", "t1", 1, None, 1000),
            span_with_parent("fetch_clip.get_song", "t1", 2, Some(1), 10),
            span_with_parent("fetch_clip.stream_body", "t1", 3, Some(1), 970),
        ])
        .await
        .unwrap();
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/span_children?name=ingest.fetch_clip",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["parent_name"], "ingest.fetch_clip");
    assert_eq!(json["parent_count"], 1);
    assert_eq!(json["parent_sum_ms"], 1000);
    let children = json["children"].as_array().unwrap();
    let names: Vec<&str> = children
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"fetch_clip.get_song"));
    assert!(names.contains(&"fetch_clip.stream_body"));
    // Newest-to-largest order; stream_body should come first (970 > 10).
    assert_eq!(children[0]["name"], "fetch_clip.stream_body");
    assert_eq!(children[0]["sum_ms"], 970);
}

#[tokio::test]
async fn span_children_missing_name_param_400s() {
    let state = common::build_state(common::test_config()).await;
    let resp = build_router(state)
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/span_children")
                .header("authorization", AUTH)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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

// --- /v1/diagnostics/recently_played ---------------------------------------

fn scrobble(track_id: &str, occurred_at: i64) -> EventInput {
    EventInput {
        event_type: EventType::Scrobble,
        track_id: TrackId::from(track_id.to_string()),
        occurred_at,
        metadata: None,
        session_id: None,
    }
}

#[tokio::test]
async fn recently_played_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/recently_played")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn recently_played_returns_empty_when_no_scrobbles() {
    let state = common::build_state(common::test_config()).await;
    let (status, json) = fetch_json(build_router(state), "/v1/diagnostics/recently_played").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["events"], serde_json::json!([]));
}

#[tokio::test]
async fn recently_played_returns_scrobbles_newest_first() {
    let state = common::build_state(common::test_config()).await;
    state
        .event_store()
        .append_batch(&[
            scrobble("t-a", 1_000),
            scrobble("t-b", 3_000),
            scrobble("t-c", 2_000),
        ])
        .await
        .unwrap();

    let (status, json) = fetch_json(build_router(state), "/v1/diagnostics/recently_played").await;
    assert_eq!(status, StatusCode::OK);
    let events = json["events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["track_id"], "t-b");
    assert_eq!(events[0]["occurred_at_ms"], 3_000);
    assert_eq!(events[1]["track_id"], "t-c");
    assert_eq!(events[2]["track_id"], "t-a");
    // received_at_ms is gateway-stamped; we don't assert its exact
    // value but it must be a finite integer.
    assert!(events[0]["received_at_ms"].is_i64());
}

#[tokio::test]
async fn recently_played_excludes_non_scrobble_events() {
    let state = common::build_state(common::test_config()).await;
    state
        .event_store()
        .append_batch(&[
            scrobble("t-a", 1_000),
            EventInput {
                event_type: EventType::Skip,
                track_id: TrackId::from("t-b".to_string()),
                occurred_at: 2_000,
                metadata: None,
                session_id: None,
            },
            EventInput {
                event_type: EventType::Like,
                track_id: TrackId::from("t-c".to_string()),
                occurred_at: 3_000,
                metadata: None,
                session_id: None,
            },
        ])
        .await
        .unwrap();

    let (_, json) = fetch_json(build_router(state), "/v1/diagnostics/recently_played").await;
    let events = json["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["track_id"], "t-a");
}

#[tokio::test]
async fn recently_played_respects_limit_param() {
    let state = common::build_state(common::test_config()).await;
    let mut batch = Vec::new();
    for i in 0..10_i64 {
        batch.push(scrobble(&format!("t-{i}"), 1_000 + i));
    }
    state.event_store().append_batch(&batch).await.unwrap();

    let (_, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recently_played?limit=3",
    )
    .await;
    let events = json["events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
}

#[tokio::test]
async fn recently_played_filters_by_since_ms() {
    let state = common::build_state(common::test_config()).await;
    state
        .event_store()
        .append_batch(&[
            scrobble("t-old", 1_000),
            scrobble("t-mid", 2_500),
            scrobble("t-new", 5_000),
        ])
        .await
        .unwrap();

    let (_, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recently_played?since_ms=2000",
    )
    .await;
    let events = json["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    let ids: Vec<&str> = events
        .iter()
        .map(|e| e["track_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"t-mid"));
    assert!(ids.contains(&"t-new"));
    assert!(!ids.contains(&"t-old"));
}

#[tokio::test]
async fn recently_played_clamps_oversize_limit() {
    // Hostile / accidental ?limit=999999 must not crash or return more
    // than the server-side cap. We don't have 1000+ events here; the
    // assertion is "the response succeeds and matches the inserted set".
    let state = common::build_state(common::test_config()).await;
    state
        .event_store()
        .append_batch(&[scrobble("t-1", 1_000)])
        .await
        .unwrap();
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recently_played?limit=999999",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = json["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
}

#[tokio::test]
async fn recently_played_joins_track_metadata_when_present() {
    use music_recommend::TrackMetadata;
    use music_recommend::metadata::normalize_title;

    let state = common::build_state(common::test_config()).await;

    // Two tracks: one with metadata in the cache, one without. The handler
    // must succeed for both and surface metadata fields only for the
    // resolvable id — the other falls back to null fields so the UI can
    // render the raw track_id without crashing.
    state
        .event_store()
        .append_batch(&[scrobble("t-known", 1_000), scrobble("t-orphan", 2_000)])
        .await
        .unwrap();

    state
        .metadata_store()
        .upsert(&TrackMetadata {
            track_id: TrackId::from("t-known".to_string()),
            artist_id: Some("ar-1".to_string()),
            artist: "Brian Eno".to_string(),
            album_id: Some("al-1".to_string()),
            album: Some("Music for Airports".to_string()),
            title: "1/1".to_string(),
            title_normalized: normalize_title("1/1"),
            duration_seconds: Some(1057),
            genre: Some("Ambient".to_string()),
            year: Some(1978),
            track_number: Some(1),
            disc_number: None,
            bpm: None,
            musical_key: None,
        })
        .await
        .unwrap();

    let (status, json) = fetch_json(build_router(state), "/v1/diagnostics/recently_played").await;
    assert_eq!(status, StatusCode::OK);
    let events = json["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);

    // Newest first → t-orphan at [0] (no metadata), t-known at [1].
    assert_eq!(events[0]["track_id"], "t-orphan");
    assert!(events[0]["title"].is_null());
    assert!(events[0]["artist"].is_null());
    assert!(events[0]["album"].is_null());
    assert!(events[0]["year"].is_null());

    assert_eq!(events[1]["track_id"], "t-known");
    assert_eq!(events[1]["title"], "1/1");
    assert_eq!(events[1]["artist"], "Brian Eno");
    assert_eq!(events[1]["artist_id"], "ar-1");
    assert_eq!(events[1]["album"], "Music for Airports");
    assert_eq!(events[1]["album_id"], "al-1");
    assert_eq!(events[1]["year"], 1978);
}

// --- /v1/diagnostics/recommend/* ------------------------------------------
//
// Aggregation endpoints over the recommend.from_any / recommend.from_seeds
// spans. R1 records the requested_n, results, shortfall_reason, and
// result_track_ids_json fields these tests assert on.

fn recommend_span(name: &str, end_ms: i64, fields: &serde_json::Value) -> SpanRecord {
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
async fn recommend_queue_fill_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/recommend/queue_fill")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn recommend_queue_fill_returns_empty_when_no_recommend_spans() {
    let state = common::build_state(common::test_config()).await;
    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recommend/queue_fill").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["total"], 0);
    // Buckets always present so the UI doesn't have to special-case
    // the empty state.
    let buckets = json["buckets"].as_array().expect("buckets array");
    assert!(!buckets.is_empty());
}

#[tokio::test]
async fn recommend_queue_fill_buckets_by_fill_ratio() {
    let state = common::build_state(common::test_config()).await;
    state
        .trace_store()
        .insert_batch(vec![
            // 20/20 → 100%
            recommend_span(
                "recommend.from_any",
                100,
                &serde_json::json!({"requested_n": 20, "results": 20}),
            ),
            // 0/20 → 0%
            recommend_span(
                "recommend.from_any",
                200,
                &serde_json::json!({"requested_n": 20, "results": 0}),
            ),
            // 10/20 → 50%
            recommend_span(
                "recommend.from_any",
                300,
                &serde_json::json!({"requested_n": 20, "results": 10}),
            ),
            // 19/20 → 95% (lands in the 80–100 bucket exclusive of 100)
            recommend_span(
                "recommend.from_any",
                400,
                &serde_json::json!({"requested_n": 20, "results": 19}),
            ),
        ])
        .await
        .unwrap();
    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recommend/queue_fill").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["total"], 4);
    let buckets = json["buckets"].as_array().unwrap();
    // Find each labeled bucket.
    let count_for = |label: &str| -> u64 {
        buckets
            .iter()
            .find(|b| b["label"] == label)
            .and_then(|b| b["count"].as_u64())
            .unwrap_or(u64::MAX)
    };
    assert_eq!(count_for("0%"), 1);
    assert_eq!(count_for("40-60%"), 1);
    assert_eq!(count_for("80-100%"), 1);
    assert_eq!(count_for("100%"), 1);
}

#[tokio::test]
async fn recommend_shortfall_groups_by_reason() {
    let state = common::build_state(common::test_config()).await;
    state
        .trace_store()
        .insert_batch(vec![
            recommend_span(
                "recommend.from_any",
                100,
                &serde_json::json!({"shortfall_reason": "none"}),
            ),
            recommend_span(
                "recommend.from_any",
                200,
                &serde_json::json!({"shortfall_reason": "filter_starved_artist"}),
            ),
            recommend_span(
                "recommend.from_any",
                300,
                &serde_json::json!({"shortfall_reason": "filter_starved_artist"}),
            ),
            recommend_span(
                "recommend.from_any",
                400,
                &serde_json::json!({"shortfall_reason": "pool_exhausted"}),
            ),
            // Old-shape row, no shortfall_reason → counts as "unknown".
            recommend_span("recommend.from_any", 500, &serde_json::json!({})),
        ])
        .await
        .unwrap();
    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recommend/shortfall").await;
    assert_eq!(status, StatusCode::OK);
    let counts = json["counts"].as_object().unwrap();
    assert_eq!(counts["none"], 1);
    assert_eq!(counts["filter_starved_artist"], 2);
    assert_eq!(counts["pool_exhausted"], 1);
    assert_eq!(counts["unknown"], 1);
}

#[tokio::test]
async fn recommend_similarity_returns_quantiles_of_admitted_sims() {
    let state = common::build_state(common::test_config()).await;
    state
        .trace_store()
        .insert_batch(vec![recommend_span(
            "recommend.from_any",
            100,
            &serde_json::json!({
                "filter_admitted_sims_json":
                    "[0.10,0.20,0.30,0.40,0.50,0.60,0.70,0.80,0.90,1.00]"
            }),
        )])
        .await
        .unwrap();
    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recommend/similarity").await;
    assert_eq!(status, StatusCode::OK);
    // Combined sample is the 10 admits above; quantiles via nearest-rank.
    // At n=10 the rank formula is idx = ceil(p*n/100) - 1, so p50 → idx 4
    // (0.5) and p95 → idx 9 (1.0). Both are correct for nearest-rank;
    // the result echoes what `TraceStore::histogram` would compute on the
    // same sample, which is the property we actually care about.
    assert_eq!(json["count"], 10);
    let p50 = json["p50"].as_f64().unwrap();
    let p95 = json["p95"].as_f64().unwrap();
    let mean = json["mean"].as_f64().unwrap();
    assert!((p50 - 0.50).abs() < 1e-3, "p50 was {p50}");
    assert!((p95 - 1.00).abs() < 1e-3, "p95 was {p95}");
    assert!((mean - 0.55).abs() < 1e-2, "mean was {mean}");
}

#[tokio::test]
async fn recommend_top_results_ranks_by_count() {
    let state = common::build_state(common::test_config()).await;
    state
        .trace_store()
        .insert_batch(vec![
            recommend_span(
                "recommend.from_any",
                100,
                &serde_json::json!({
                    "result_track_ids_json": "[\"t-1\",\"t-2\",\"t-3\"]"
                }),
            ),
            recommend_span(
                "recommend.from_any",
                200,
                &serde_json::json!({
                    "result_track_ids_json": "[\"t-1\",\"t-2\"]"
                }),
            ),
            recommend_span(
                "recommend.from_any",
                300,
                &serde_json::json!({
                    "result_track_ids_json": "[\"t-1\"]"
                }),
            ),
        ])
        .await
        .unwrap();
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/top_results?limit=10",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    // t-1 ×3, t-2 ×2, t-3 ×1; ordering by count desc, ties broken by id.
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["track_id"], "t-1");
    assert_eq!(items[0]["count"], 3);
    assert_eq!(items[1]["track_id"], "t-2");
    assert_eq!(items[1]["count"], 2);
    assert_eq!(items[2]["track_id"], "t-3");
    assert_eq!(items[2]["count"], 1);
}

// --- /v1/recommend/feedback (POST) + /v1/diagnostics/recommend/feedback (GET) ---
//
// Captures thumb-up / thumb-down on a recommended track. The POST UPSERTs
// per (track_id, session_id) and returns the fresh totals. The GET returns
// per-track aggregates joined with the metadata cache.

async fn post_json(app: axum::Router, uri: &str, body: serde_json::Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", AUTH)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    // 4xx responses can carry a plain-text body (`(StatusCode, &str)`);
    // we don't want a test reading the body in that case to crash, so
    // fall back to Value::Null instead of insisting on JSON.
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn recommend_feedback_post_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/recommend/feedback")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"track_id":"t","session_id":"s","vote":"up"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn recommend_feedback_post_records_upvote_and_returns_totals() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let (status, json) = post_json(
        app,
        "/v1/recommend/feedback",
        serde_json::json!({
            "track_id": "t-up",
            "session_id": "sess-A",
            "vote": "up",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["track_id"], "t-up");
    assert_eq!(json["up"], 1);
    assert_eq!(json["down"], 0);
}

#[tokio::test]
async fn recommend_feedback_post_flips_existing_vote_within_session() {
    // Same session voting up then down must replace, not double-count.
    let state = common::build_state(common::test_config()).await;
    let app1 = build_router(state.clone());
    let (status_up, _) = post_json(
        app1,
        "/v1/recommend/feedback",
        serde_json::json!({"track_id": "t-flip", "session_id": "s", "vote": "up"}),
    )
    .await;
    assert_eq!(status_up, StatusCode::OK);

    let app2 = build_router(state.clone());
    let (status_down, json) = post_json(
        app2,
        "/v1/recommend/feedback",
        serde_json::json!({"track_id": "t-flip", "session_id": "s", "vote": "down"}),
    )
    .await;
    assert_eq!(status_down, StatusCode::OK);
    assert_eq!(json["up"], 0);
    assert_eq!(json["down"], 1);
}

#[tokio::test]
async fn recommend_feedback_post_null_vote_clears_row() {
    let state = common::build_state(common::test_config()).await;
    let app1 = build_router(state.clone());
    post_json(
        app1,
        "/v1/recommend/feedback",
        serde_json::json!({"track_id": "t-clear", "session_id": "s", "vote": "up"}),
    )
    .await;
    let app2 = build_router(state.clone());
    let (status, json) = post_json(
        app2,
        "/v1/recommend/feedback",
        serde_json::json!({"track_id": "t-clear", "session_id": "s", "vote": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["up"], 0);
    assert_eq!(json["down"], 0);
}

#[tokio::test]
async fn recommend_feedback_post_rejects_empty_track_id() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let (status, _) = post_json(
        app,
        "/v1/recommend/feedback",
        serde_json::json!({"track_id": "", "session_id": "s", "vote": "up"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn recommend_feedback_diagnostics_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/recommend/feedback")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn recommend_feedback_diagnostics_returns_empty_when_no_votes() {
    let state = common::build_state(common::test_config()).await;
    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recommend/feedback").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn recommend_feedback_diagnostics_returns_aggregates_newest_first() {
    let state = common::build_state(common::test_config()).await;
    // Three sessions vote on two tracks. t-a gets two ups, t-b gets one down
    // and its most-recent vote is later → must appear first.
    for (track, session, vote) in [
        ("t-a", "s1", "up"),
        ("t-a", "s2", "up"),
        ("t-b", "s1", "down"),
    ] {
        let app = build_router(state.clone());
        post_json(
            app,
            "/v1/recommend/feedback",
            serde_json::json!({
                "track_id": track,
                "session_id": session,
                "vote": vote,
            }),
        )
        .await;
    }
    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recommend/feedback").await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    // Newest received_ms is t-b (the third write). The two t-a votes are
    // older; both rows under t-a get aggregated, so t-a appears once.
    assert_eq!(items[0]["track_id"], "t-b");
    assert_eq!(items[0]["down"], 1);
    assert_eq!(items[1]["track_id"], "t-a");
    assert_eq!(items[1]["up"], 2);
}

// --- /v1/diagnostics/recommend/latent_space -------------------------------

async fn insert_projection_point(
    state: &music_gateway::AppState,
    track_id: &str,
    model_version: &str,
    proj_version: &str,
    x: f64,
    y: f64,
    created_at_ms: i64,
) {
    insert_projection_point_with_pcs(
        state,
        track_id,
        model_version,
        proj_version,
        x,
        y,
        created_at_ms,
        [None, None, None, None],
    )
    .await;
}

// Test fixture mirroring the projection-row column set; the wide arg list
// matches the table columns rather than a domain abstraction.
#[allow(clippy::too_many_arguments)]
async fn insert_projection_point_with_pcs(
    state: &music_gateway::AppState,
    track_id: &str,
    model_version: &str,
    proj_version: &str,
    x: f64,
    y: f64,
    created_at_ms: i64,
    pcs: [Option<f64>; 4],
) {
    sqlx::query(
        "INSERT INTO embedding_projection_2d
             (track_id, model_version, proj_version, x, y, created_at_ms,
              pc1, pc2, pc3, pc4)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(track_id)
    .bind(model_version)
    .bind(proj_version)
    .bind(x)
    .bind(y)
    .bind(created_at_ms)
    .bind(pcs[0])
    .bind(pcs[1])
    .bind(pcs[2])
    .bind(pcs[3])
    .execute(state.embedding_store().pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn latent_space_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/recommend/latent_space")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn latent_space_returns_empty_when_no_projection_written() {
    let state = common::build_state(common::test_config()).await;
    let (status, json) = fetch_json(
        build_router(state.clone()),
        "/v1/diagnostics/recommend/latent_space",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["points"].as_array().unwrap().len(), 0);
    assert_eq!(json["versions"].as_array().unwrap().len(), 0);
    // proj_version is null when no projection exists; model_version
    // still echoes back the active recommender model.
    assert!(json["proj_version"].is_null());
    assert_eq!(
        json["model_version"].as_str().unwrap(),
        state.recommend_model_version().as_str()
    );
}

#[tokio::test]
async fn latent_space_picks_latest_proj_version_by_default() {
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    // Two projections under the active model: pv-old (earlier) and
    // pv-new (later). Without an explicit `proj_version` query param
    // the handler must pick pv-new.
    insert_projection_point(&state, "t1", &model, "pv-old", 0.0, 0.0, 100).await;
    insert_projection_point(&state, "t1", &model, "pv-new", 5.0, 6.0, 500).await;

    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["proj_version"], "pv-new");
    let points = json["points"].as_array().unwrap();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0]["track_id"], "t1");
    assert_eq!(points[0]["x"], 5.0);
    assert_eq!(points[0]["y"], 6.0);
    // The versions catalogue is sorted newest first.
    let versions = json["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["proj_version"], "pv-new");
    assert_eq!(versions[1]["proj_version"], "pv-old");
}

#[tokio::test]
async fn latent_space_explicit_proj_version_query_param_wins() {
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    insert_projection_point(&state, "t1", &model, "pv-old", 0.0, 0.0, 100).await;
    insert_projection_point(&state, "t1", &model, "pv-new", 5.0, 6.0, 500).await;

    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space?proj_version=pv-old",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["proj_version"], "pv-old");
    let points = json["points"].as_array().unwrap();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0]["x"], 0.0);
}

#[tokio::test]
async fn latent_space_filters_by_model_version() {
    let state = common::build_state(common::test_config()).await;
    let active = state.recommend_model_version().as_str().to_string();
    // Active model gets one point; a hypothetical other model gets a
    // different point under the same proj_version.
    insert_projection_point(&state, "t1", &active, "pv1", 1.0, 1.0, 100).await;
    insert_projection_point(&state, "t2", "other-model", "pv1", 9.0, 9.0, 200).await;

    // Default query: must only see the active-model row.
    let (_, json) = fetch_json(
        build_router(state.clone()),
        "/v1/diagnostics/recommend/latent_space",
    )
    .await;
    let points = json["points"].as_array().unwrap();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0]["track_id"], "t1");

    // Explicit `model_version` override flips the view to the other model.
    let (_, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space?model_version=other-model",
    )
    .await;
    let points = json["points"].as_array().unwrap();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0]["track_id"], "t2");
    assert_eq!(json["model_version"], "other-model");
}

#[tokio::test]
async fn latent_space_joins_metadata_when_available() {
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    // Seed metadata for t1 (but not t2) so we exercise both the
    // "metadata present" and "metadata missing → nulls" branches.
    state
        .metadata_store()
        .upsert(&music_recommend::TrackMetadata {
            track_id: TrackId::from("t1"),
            artist_id: None,
            artist: "Artist One".into(),
            album_id: None,
            album: Some("Album One".into()),
            title: "Title One".into(),
            title_normalized: music_recommend::normalize_title("Title One"),
            duration_seconds: None,
            genre: Some("Ambient".into()),
            year: None,
            track_number: None,
            disc_number: None,
            bpm: None,
            musical_key: None,
        })
        .await
        .unwrap();

    insert_projection_point(&state, "t1", &model, "pv1", 1.0, 1.0, 100).await;
    insert_projection_point(&state, "t2", &model, "pv1", 2.0, 2.0, 100).await;

    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let points = json["points"].as_array().unwrap();
    assert_eq!(points.len(), 2);
    // ProjectionStore orders by track_id, so t1 is first.
    assert_eq!(points[0]["track_id"], "t1");
    assert_eq!(points[0]["title"], "Title One");
    assert_eq!(points[0]["artist"], "Artist One");
    assert_eq!(points[0]["album"], "Album One");
    // Genre travels with the metadata join — the web scatter uses it to
    // colour clusters as a validation signal for the embedding.
    assert_eq!(points[0]["genre"], "Ambient");
    // t2 has no metadata row — fields surface as JSON null.
    assert_eq!(points[1]["track_id"], "t2");
    assert!(points[1]["title"].is_null());
    assert!(points[1]["artist"].is_null());
    assert!(points[1]["album"].is_null());
    assert!(points[1]["genre"].is_null());
}

#[tokio::test]
async fn latent_space_surfaces_pca_components_when_present() {
    // PCA components are nullable, but when the reducer has written
    // them they must flow through the endpoint as `pc1..pc4` numbers
    // — the frontend's colour-by-PC mode reads them by name. A track
    // with partial PCs (e.g. small dataset, only 2 PCs viable) keeps
    // the rest null.
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    insert_projection_point_with_pcs(
        &state,
        "t1",
        &model,
        "pv1",
        0.0,
        0.0,
        100,
        [Some(0.5), Some(-0.5), Some(0.25), Some(-0.25)],
    )
    .await;
    insert_projection_point_with_pcs(
        &state,
        "t2",
        &model,
        "pv1",
        1.0,
        1.0,
        100,
        [Some(1.5), Some(0.0), None, None],
    )
    .await;

    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let points = json["points"].as_array().unwrap();
    assert_eq!(points.len(), 2);
    // SELECT orders by track_id, so t1 first.
    assert_eq!(points[0]["pc1"], 0.5);
    assert_eq!(points[0]["pc2"], -0.5);
    assert_eq!(points[0]["pc3"], 0.25);
    assert_eq!(points[0]["pc4"], -0.25);
    assert_eq!(points[1]["pc1"], 1.5);
    assert_eq!(points[1]["pc2"], 0.0);
    assert!(points[1]["pc3"].is_null());
    assert!(points[1]["pc4"].is_null());
}

#[tokio::test]
async fn latent_space_pcs_are_null_when_reducer_did_not_write_them() {
    // Backward-compat: rows from a pre-migration-0009 reducer still
    // render correctly — the PC columns are simply null and the
    // frontend's colour-by-PC dropdown grays out those options.
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    insert_projection_point(&state, "t1", &model, "pv-legacy", 0.0, 0.0, 100).await;

    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pt = &json["points"].as_array().unwrap()[0];
    for k in ["pc1", "pc2", "pc3", "pc4"] {
        assert!(pt[k].is_null(), "{k} should be null on legacy row");
    }
}

// Test fixture mirroring the projection-row column set; the wide arg list
// matches the table columns rather than a domain abstraction.
#[allow(clippy::too_many_arguments)]
async fn insert_projection_point_with_z(
    state: &music_gateway::AppState,
    track_id: &str,
    model_version: &str,
    proj_version: &str,
    x: f64,
    y: f64,
    created_at_ms: i64,
    z: Option<f64>,
) {
    sqlx::query(
        "INSERT INTO embedding_projection_2d
             (track_id, model_version, proj_version, x, y, created_at_ms, z)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(track_id)
    .bind(model_version)
    .bind(proj_version)
    .bind(x)
    .bind(y)
    .bind(created_at_ms)
    .bind(z)
    .execute(state.embedding_store().pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn latent_space_3d_projection_serves_its_own_xyz() {
    // 2D and 3D UMAP runs are independent layouts; nothing joins
    // across them. Selecting a `-d3` projection must return its own
    // (x, y, z) — the canvas geometry comes from the 3D run's first
    // two dims, z from the third.
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    insert_projection_point_with_z(&state, "t1", &model, "pv1-d3", 7.0, 8.0, 200, Some(1.5)).await;
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space?proj_version=pv1-d3",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["proj_version"], "pv1-d3");
    let points = json["points"].as_array().unwrap();
    assert_eq!(points[0]["x"], 7.0);
    assert_eq!(points[0]["y"], 8.0);
    assert_eq!(points[0]["z"], 1.5);
}

#[tokio::test]
async fn latent_space_2d_projection_z_is_null() {
    // A 2D-UMAP row has no z column populated. The colour-by dropdown
    // disables "UMAP z" when this is the active projection — the
    // backend just surfaces the null so the UI can decide.
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    insert_projection_point(&state, "t1", &model, "pv1", 1.0, 2.0, 100).await;
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["proj_version"], "pv1");
    let pt = &json["points"].as_array().unwrap()[0];
    assert_eq!(pt["x"], 1.0);
    assert_eq!(pt["y"], 2.0);
    assert!(pt["z"].is_null());
}

#[tokio::test]
async fn latent_space_prefer_2d_picks_newest_non_d3_projection() {
    // The web UI uses `?prefer=2d` for non-UMAP-z colour modes so the
    // canvas always lands on the 2D-UMAP layout regardless of how many
    // -d3 companions exist or how recently they were written.
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    insert_projection_point(&state, "t1", &model, "pv-old-2d", 0.0, 0.0, 100).await;
    insert_projection_point(&state, "t1", &model, "pv-new-2d", 1.0, 1.0, 500).await;
    // -d3 companion written latest — must NOT win for prefer=2d.
    insert_projection_point_with_z(
        &state,
        "t1",
        &model,
        "pv-new-2d-d3",
        9.0,
        9.0,
        900,
        Some(0.5),
    )
    .await;
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space?prefer=2d",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["proj_version"], "pv-new-2d");
    assert_eq!(json["points"].as_array().unwrap()[0]["x"], 1.0);
}

#[tokio::test]
async fn latent_space_prefer_3d_picks_newest_d3_projection() {
    // UMAP-z mode in the web UI uses `?prefer=3d` to land on the 3D
    // run's layout (x,y from first two dims, z as the colour channel).
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    insert_projection_point(&state, "t1", &model, "pv-2d", 1.0, 1.0, 100).await;
    insert_projection_point_with_z(&state, "t1", &model, "pv-old-d3", 5.0, 5.0, 200, Some(0.3))
        .await;
    insert_projection_point_with_z(&state, "t1", &model, "pv-new-d3", 7.0, 7.0, 500, Some(0.7))
        .await;
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space?prefer=3d",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["proj_version"], "pv-new-d3");
    let pt = &json["points"].as_array().unwrap()[0];
    assert_eq!(pt["x"], 7.0);
    assert_eq!(pt["z"], 0.7);
}

#[tokio::test]
async fn latent_space_prefer_2d_returns_empty_when_only_d3_exists() {
    // Edge: only a 3D run exists. prefer=2d resolves to no projection
    // — the UI shows the "no 2D projection yet" hint rather than
    // accidentally falling back to the 3D layout.
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    insert_projection_point_with_z(&state, "t1", &model, "pv-d3", 1.0, 1.0, 100, Some(0.5)).await;
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space?prefer=2d",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["proj_version"].is_null());
    assert!(json["points"].as_array().unwrap().is_empty());
    // The full versions catalogue still includes the -d3 entry — the
    // UI uses this to know that UMAP-z is available even if 2D isn't.
    assert_eq!(json["versions"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn latent_space_prefer_3d_returns_empty_when_only_2d_exists() {
    // Symmetric case: prefer=3d but no -d3 projection exists. UMAP-z
    // dropdown will stay disabled.
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    insert_projection_point(&state, "t1", &model, "pv-2d", 1.0, 1.0, 100).await;
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space?prefer=3d",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["proj_version"].is_null());
    assert!(json["points"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn latent_space_explicit_proj_version_overrides_prefer() {
    // `?proj_version=…` is the power-user debug pathway and must win
    // even when `prefer` is also passed. Documents the precedence so
    // a future frontend doesn't accidentally lose explicit selection
    // by also sending `prefer`.
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    insert_projection_point(&state, "t1", &model, "pv-2d", 1.0, 2.0, 100).await;
    insert_projection_point_with_z(&state, "t1", &model, "pv-d3", 7.0, 8.0, 200, Some(0.5)).await;
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space?proj_version=pv-2d&prefer=3d",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["proj_version"], "pv-2d");
}

#[tokio::test]
async fn latent_space_dropdown_lists_both_2d_and_3d_projections() {
    // The 2D and 3D runs coexist in the dropdown — the user picks
    // which layout to view. The two layouts are independent UMAP
    // results, not a primary/companion pair.
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().as_str().to_string();
    insert_projection_point(&state, "t1", &model, "pv1", 0.0, 0.0, 100).await;
    insert_projection_point_with_z(&state, "t1", &model, "pv1-d3", 9.0, 9.0, 200, Some(0.5)).await;
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_space",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let versions = json["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    // Sorted by MAX(created_at_ms) DESC: 3D (200) before 2D (100).
    assert_eq!(versions[0]["proj_version"], "pv1-d3");
    assert_eq!(versions[1]["proj_version"], "pv1");
}

// --- /v1/diagnostics/recommend/sessions ----------------------------------

#[tokio::test]
async fn recommend_sessions_returns_empty_when_no_sessions() {
    let state = common::build_state(common::test_config()).await;
    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recommend/sessions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn recommend_sessions_returns_newest_first_with_event_counts() {
    let state = common::build_state(common::test_config()).await;
    // Three sessions, increasing started_ms. s1 and s2 closed; s3 active.
    state
        .sync()
        .apply(music_gateway::principal::OWNER_USER_ID, &music_sync::SyncOp::StartSession {
            items: vec![music_core::QueueItem {
                item_id: music_core::QueueItemId::from("qi-1".to_string()),
                track_id: music_core::TrackId::from("t-1".to_string()),
            }],
            anchor_index: 0,
            session_id: music_core::SessionId::from("s1".to_string()),
        })
        .await
        .unwrap();
    // Stamp two events under s1 by going through the scrobble interceptor.
    state
        .event_store()
        .append_batch(&[music_recommend::EventInput {
            event_type: music_recommend::EventType::Scrobble,
            track_id: music_core::TrackId::from("t-1"),
            occurred_at: 100,
            metadata: None,
            session_id: Some(music_core::SessionId::from("s1")),
        }])
        .await
        .unwrap();
    state
        .event_store()
        .append_batch(&[music_recommend::EventInput {
            event_type: music_recommend::EventType::Skip,
            track_id: music_core::TrackId::from("t-1"),
            occurred_at: 200,
            metadata: None,
            session_id: Some(music_core::SessionId::from("s1")),
        }])
        .await
        .unwrap();
    // Open s2 (auto-closes s1), then s3 (auto-closes s2).
    state
        .sync()
        .apply(music_gateway::principal::OWNER_USER_ID, &music_sync::SyncOp::StartSession {
            items: vec![music_core::QueueItem {
                item_id: music_core::QueueItemId::from("qi-2".to_string()),
                track_id: music_core::TrackId::from("t-2".to_string()),
            }],
            anchor_index: 0,
            session_id: music_core::SessionId::from("s2".to_string()),
        })
        .await
        .unwrap();
    state
        .sync()
        .apply(music_gateway::principal::OWNER_USER_ID, &music_sync::SyncOp::StartSession {
            items: vec![music_core::QueueItem {
                item_id: music_core::QueueItemId::from("qi-3".to_string()),
                track_id: music_core::TrackId::from("t-3".to_string()),
            }],
            anchor_index: 0,
            session_id: music_core::SessionId::from("s3".to_string()),
        })
        .await
        .unwrap();

    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recommend/sessions").await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    // Newest started_ms first: s3, s2, s1.
    assert_eq!(items[0]["session_id"], "s3");
    assert!(
        items[0]["ended_ms"].is_null(),
        "active session has null ended_ms"
    );
    assert_eq!(items[0]["event_count"], 0);
    assert_eq!(items[1]["session_id"], "s2");
    assert!(!items[1]["ended_ms"].is_null(), "s2 closed by s3 start");
    assert_eq!(items[1]["event_count"], 0);
    assert_eq!(items[2]["session_id"], "s1");
    assert_eq!(items[2]["anchor_track_id"], "t-1");
    assert_eq!(items[2]["items_count"], 1);
    assert_eq!(items[2]["event_count"], 2);
}

#[tokio::test]
async fn recommend_sessions_respects_limit_query() {
    let state = common::build_state(common::test_config()).await;
    for i in 0..5 {
        state
            .sync()
            .apply(music_gateway::principal::OWNER_USER_ID, &music_sync::SyncOp::StartSession {
                items: vec![music_core::QueueItem {
                    item_id: music_core::QueueItemId::from(format!("qi-{i}")),
                    track_id: music_core::TrackId::from(format!("t-{i}")),
                }],
                anchor_index: 0,
                session_id: music_core::SessionId::from(format!("s{i}")),
            })
            .await
            .unwrap();
    }
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/sessions?limit=2",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    // Newest two: s4, s3.
    assert_eq!(items[0]["session_id"], "s4");
    assert_eq!(items[1]["session_id"], "s3");
}

// --- include_events=1 ------------------------------------------------------
//
// Verifies that when callers ask for the full session trace, each item
// gains `events` (oldest-first) plus `segments` (length = events-1).
// Each segment carries the cosine distance between the two adjacent
// tracks' embeddings — `None` when either embedding is missing under
// the active model_version.

/// Store a 'done' embedding for `(track, model)`. Goes through
/// `enqueue` → `claim_next` → `mark_done` so we use the same code path
/// as the worker; that gives us a more honest end-to-end test than
/// raw-SQL injection.
async fn seed_embedding(
    state: &music_gateway::AppState,
    track: &str,
    model: &ModelVersion,
    vector: Vec<f32>,
) {
    let key = EmbeddingKey::new(track.to_string(), model.clone());
    state.embedding_store().enqueue(&key).await.unwrap();
    // claim_next picks the oldest enqueued row, but tests run isolated
    // so the FIFO works out. mark_done writes via the key on the
    // returned Embedding, not the claim result.
    state
        .embedding_store()
        .claim_next(model)
        .await
        .unwrap()
        .expect("queue not empty");
    state
        .embedding_store()
        .mark_done(&music_recommend::Embedding::new(key, vector))
        .await
        .unwrap();
}

#[tokio::test]
async fn recommend_sessions_include_events_attaches_events_and_segments() {
    let state = common::build_state(common::test_config()).await;
    let model = state.recommend_model_version().clone();

    // t-a and t-b have embeddings; t-c does not. Path: a → b → c → b.
    seed_embedding(&state, "t-a", &model, vec![1.0, 0.0, 0.0]).await;
    seed_embedding(&state, "t-b", &model, vec![0.0, 1.0, 0.0]).await;

    state
        .sync()
        .apply(music_gateway::principal::OWNER_USER_ID, &music_sync::SyncOp::StartSession {
            items: vec![music_core::QueueItem {
                item_id: music_core::QueueItemId::from("qi-a".to_string()),
                track_id: music_core::TrackId::from("t-a".to_string()),
            }],
            anchor_index: 0,
            session_id: music_core::SessionId::from("s-evt".to_string()),
        })
        .await
        .unwrap();
    for (track, occurred_at, ev_type) in [
        ("t-a", 100_i64, music_recommend::EventType::Scrobble),
        ("t-b", 200, music_recommend::EventType::Scrobble),
        ("t-c", 300, music_recommend::EventType::Scrobble),
        ("t-b", 400, music_recommend::EventType::Scrobble),
    ] {
        state
            .event_store()
            .append_batch(&[music_recommend::EventInput {
                event_type: ev_type,
                track_id: music_core::TrackId::from(track),
                occurred_at,
                metadata: None,
                session_id: Some(music_core::SessionId::from("s-evt")),
            }])
            .await
            .unwrap();
    }

    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/sessions?include_events=1",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];

    let events = item["events"]
        .as_array()
        .expect("events present when include_events=1");
    assert_eq!(events.len(), 4);
    // Oldest first by occurred_at.
    assert_eq!(events[0]["track_id"], "t-a");
    assert_eq!(events[0]["occurred_at_ms"], 100);
    assert_eq!(events[1]["track_id"], "t-b");
    assert_eq!(events[2]["track_id"], "t-c");
    assert_eq!(events[3]["track_id"], "t-b");

    let segments = item["segments"].as_array().expect("segments present");
    assert_eq!(segments.len(), 3, "events.len() - 1");

    // a (1,0,0) and b (0,1,0) are orthogonal → cosine = 0 → distance = 1.
    let d_ab = segments[0]["cosine_distance"]
        .as_f64()
        .expect("ab distance");
    assert!((d_ab - 1.0).abs() < 1e-6, "got {d_ab}");

    // b → c: c has no embedding → null.
    assert!(
        segments[1]["cosine_distance"].is_null(),
        "missing embedding => null"
    );
    // c → b: same reason, null.
    assert!(segments[2]["cosine_distance"].is_null());
}

#[tokio::test]
async fn recommend_sessions_omits_events_by_default() {
    let state = common::build_state(common::test_config()).await;
    state
        .sync()
        .apply(music_gateway::principal::OWNER_USER_ID, &music_sync::SyncOp::StartSession {
            items: vec![music_core::QueueItem {
                item_id: music_core::QueueItemId::from("qi-1".to_string()),
                track_id: music_core::TrackId::from("t-1".to_string()),
            }],
            anchor_index: 0,
            session_id: music_core::SessionId::from("s-bare".to_string()),
        })
        .await
        .unwrap();

    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recommend/sessions").await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    // Backward-compatible: bare endpoint never returns events/segments.
    assert!(items[0].get("events").is_none() || items[0]["events"].is_null());
    assert!(items[0].get("segments").is_none() || items[0]["segments"].is_null());
}

#[tokio::test]
async fn recommend_sessions_include_events_handles_session_with_no_events() {
    let state = common::build_state(common::test_config()).await;
    state
        .sync()
        .apply(music_gateway::principal::OWNER_USER_ID, &music_sync::SyncOp::StartSession {
            items: vec![music_core::QueueItem {
                item_id: music_core::QueueItemId::from("qi-1".to_string()),
                track_id: music_core::TrackId::from("t-1".to_string()),
            }],
            anchor_index: 0,
            session_id: music_core::SessionId::from("s-empty".to_string()),
        })
        .await
        .unwrap();

    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/sessions?include_events=1",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["events"].as_array().unwrap().len(), 0);
    assert_eq!(items[0]["segments"].as_array().unwrap().len(), 0);
}

// --- /v1/diagnostics/recommend/latent_neighbours -------------------------
//
// On-hover overlay for the latent-space scatter: returns the k nearest
// neighbours of a track in the original 512-D CLAP space. UMAP doesn't
// preserve global distances, so the response is the "ground truth" the
// 2D layout glosses over.

/// Build a tiny family of unit vectors. `unit_at(i)` is the canonical
/// basis vector e_i. Pairwise cosine distance for distinct i, j is
/// exactly 1.0 (orthogonal); cosine distance to itself is 0.0. That
/// gives the test deterministic distances without depending on usearch
/// internals.
fn unit_at_dim(i: usize, dim: usize) -> Vec<f32> {
    let mut v = vec![0.0_f32; dim];
    v[i] = 1.0;
    v
}

#[tokio::test]
async fn latent_neighbours_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/recommend/latent_neighbours?track_id=t0&k=5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn latent_neighbours_404_when_seed_not_embedded() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/recommend/latent_neighbours?track_id=does-not-exist")
                .header("authorization", AUTH)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn latent_neighbours_returns_top_k_excluding_seed() {
    let state = common::build_state(common::test_config()).await;
    let dim = 8;
    let ann = state.ann();
    for i in 0..dim {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at_dim(i, dim))
            .unwrap();
    }
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_neighbours?track_id=t0&k=3",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["track_id"], "t0");
    let n = json["neighbours"].as_array().unwrap();
    assert!(n.len() <= 3, "respect k cap");
    assert!(!n.is_empty(), "should return at least one neighbour");
    for entry in n {
        assert_ne!(
            entry["track_id"], "t0",
            "seed must not appear in own neighbours"
        );
        // Distances are non-negative and bounded by ~2 (cosine).
        let d = entry["cosine_distance"].as_f64().unwrap();
        assert!((0.0..=2.0).contains(&d), "distance in cosine range: {d}");
    }
}

#[tokio::test]
async fn latent_neighbours_default_k_when_param_missing() {
    let state = common::build_state(common::test_config()).await;
    let dim = 8;
    let ann = state.ann();
    for i in 0..dim {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at_dim(i, dim))
            .unwrap();
    }
    // No `k` query param: handler uses its built-in default (>= 1).
    // We don't pin the exact default; we only require "returns something
    // sensible without an explicit k."
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_neighbours?track_id=t0",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let n = json["neighbours"].as_array().unwrap();
    assert!(!n.is_empty(), "default k should return neighbours");
}

#[tokio::test]
async fn latent_neighbours_clamps_oversized_k() {
    let state = common::build_state(common::test_config()).await;
    let dim = 8;
    let ann = state.ann();
    for i in 0..dim {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at_dim(i, dim))
            .unwrap();
    }
    // k=1000 is far above any sensible cap; the handler must clamp
    // rather than crash or fan out to a giant search.
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_neighbours?track_id=t0&k=1000",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let n = json["neighbours"].as_array().unwrap();
    // Index has dim=8 tracks total; after excluding the seed, at most 7.
    assert!(
        n.len() <= 7,
        "clamped to index size minus seed, got {}",
        n.len()
    );
}

#[tokio::test]
async fn latent_neighbours_400_when_k_is_zero() {
    let state = common::build_state(common::test_config()).await;
    let dim = 8;
    let ann = state.ann();
    ann.upsert(&TrackId::from("t0"), &unit_at_dim(0, dim))
        .unwrap();
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/recommend/latent_neighbours?track_id=t0&k=0")
                .header("authorization", AUTH)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn latent_neighbours_orders_by_ascending_cosine_distance() {
    let state = common::build_state(common::test_config()).await;
    let dim = 8;
    let ann = state.ann();
    // Seed lives at e_0. Three crafted neighbours at known distances:
    //   - "close":  vector aligned with e_0 plus a tiny perpendicular
    //               component (smallest cosine distance after t_0).
    //   - "mid":    a 45° vector in (e_0, e_1) plane.
    //   - "far":    pure e_1 (orthogonal: cosine distance ≈ 1.0).
    let seed = vec![1.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let mut close = vec![0.0_f32; dim];
    close[0] = 0.99;
    close[1] = (1.0_f32 - 0.99 * 0.99).sqrt();
    let mid = {
        let v = 1.0_f32 / (2.0_f32).sqrt();
        let mut m = vec![0.0_f32; dim];
        m[0] = v;
        m[1] = v;
        m
    };
    let far = unit_at_dim(1, dim);
    ann.upsert(&TrackId::from("t0"), &seed).unwrap();
    ann.upsert(&TrackId::from("close"), &close).unwrap();
    ann.upsert(&TrackId::from("mid"), &mid).unwrap();
    ann.upsert(&TrackId::from("far"), &far).unwrap();

    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/latent_neighbours?track_id=t0&k=3",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let n = json["neighbours"].as_array().unwrap();
    assert_eq!(n.len(), 3);
    // Distances must be monotonically non-decreasing.
    let dists: Vec<f64> = n
        .iter()
        .map(|e| e["cosine_distance"].as_f64().unwrap())
        .collect();
    for w in dists.windows(2) {
        assert!(w[0] <= w[1] + 1e-6, "not monotone: {dists:?}");
    }
    // The closest neighbour must be "close" (smallest cosine distance).
    assert_eq!(n[0]["track_id"], "close", "closest should rank first");
}
