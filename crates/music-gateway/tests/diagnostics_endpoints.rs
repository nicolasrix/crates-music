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
use music_core::TrackId;
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

// --- /v1/diagnostics/recently_played ---------------------------------------

fn scrobble(track_id: &str, occurred_at: i64) -> EventInput {
    EventInput {
        event_type: EventType::Scrobble,
        track_id: TrackId::from(track_id.to_string()),
        occurred_at,
        metadata: None,
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
    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recently_played").await;
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

    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recently_played").await;
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
            },
            EventInput {
                event_type: EventType::Like,
                track_id: TrackId::from("t-c".to_string()),
                occurred_at: 3_000,
                metadata: None,
            },
        ])
        .await
        .unwrap();

    let (_, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recently_played").await;
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

    let (_, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recently_played?limit=3").await;
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
        .append_batch(&[
            scrobble("t-known", 1_000),
            scrobble("t-orphan", 2_000),
        ])
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

    let (status, json) =
        fetch_json(build_router(state), "/v1/diagnostics/recently_played").await;
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
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/queue_fill",
    )
    .await;
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
            recommend_span("recommend.from_any",
                100,
                &serde_json::json!({"requested_n": 20, "results": 20}),
            ),
            // 0/20 → 0%
            recommend_span("recommend.from_any",
                200,
                &serde_json::json!({"requested_n": 20, "results": 0}),
            ),
            // 10/20 → 50%
            recommend_span("recommend.from_any",
                300,
                &serde_json::json!({"requested_n": 20, "results": 10}),
            ),
            // 19/20 → 95% (lands in the 80–100 bucket exclusive of 100)
            recommend_span("recommend.from_any",
                400,
                &serde_json::json!({"requested_n": 20, "results": 19}),
            ),
        ])
        .await
        .unwrap();
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/queue_fill",
    )
    .await;
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
            recommend_span("recommend.from_any",
                100,
                &serde_json::json!({"shortfall_reason": "none"}),
            ),
            recommend_span("recommend.from_any",
                200,
                &serde_json::json!({"shortfall_reason": "filter_starved_artist"}),
            ),
            recommend_span("recommend.from_any",
                300,
                &serde_json::json!({"shortfall_reason": "filter_starved_artist"}),
            ),
            recommend_span("recommend.from_any",
                400,
                &serde_json::json!({"shortfall_reason": "pool_exhausted"}),
            ),
            // Old-shape row, no shortfall_reason → counts as "unknown".
            recommend_span("recommend.from_any", 500, &serde_json::json!({})),
        ])
        .await
        .unwrap();
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/shortfall",
    )
    .await;
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
        .insert_batch(vec![
            recommend_span("recommend.from_any",
                100,
                &serde_json::json!({
                    "filter_admitted_sims_json":
                        "[0.10,0.20,0.30,0.40,0.50,0.60,0.70,0.80,0.90,1.00]"
                }),
            ),
        ])
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
            recommend_span("recommend.from_any",
                100,
                &serde_json::json!({
                    "result_track_ids_json": "[\"t-1\",\"t-2\",\"t-3\"]"
                }),
            ),
            recommend_span("recommend.from_any",
                200,
                &serde_json::json!({
                    "result_track_ids_json": "[\"t-1\",\"t-2\"]"
                }),
            ),
            recommend_span("recommend.from_any",
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
                .body(Body::from(r#"{"track_id":"t","session_id":"s","vote":"up"}"#))
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
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/feedback",
    )
    .await;
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
    let (status, json) = fetch_json(
        build_router(state),
        "/v1/diagnostics/recommend/feedback",
    )
    .await;
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
    sqlx::query(
        "INSERT INTO embedding_projection_2d
             (track_id, model_version, proj_version, x, y, created_at_ms)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(track_id)
    .bind(model_version)
    .bind(proj_version)
    .bind(x)
    .bind(y)
    .bind(created_at_ms)
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
