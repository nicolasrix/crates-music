//! Integration tests for `POST /v1/diagnostics/client_events` and
//! `GET /v1/diagnostics/client_events`. The POST handler stamps
//! `received_ms` and `user_agent` server-side; the GET handler returns
//! recent rows newest-first with optional name filtering.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_gateway::build_router;
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;

const AUTH: &str = "Bearer test-bearer-token";

async fn post_json(
    app: axum::Router,
    uri: &str,
    body: Value,
    user_agent: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", AUTH)
        .header("content-type", "application/json");
    if let Some(ua) = user_agent {
        req = req.header("user-agent", ua);
    }
    let resp = app
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    // 4xx error bodies from axum extractors are text/plain, not JSON;
    // tolerate that so a 400 doesn't panic the helper before assertion.
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
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
        serde_json::from_slice(&bytes).expect("JSON body")
    };
    (status, json)
}

// --- POST /v1/diagnostics/client_events -----------------------------------

#[tokio::test]
async fn client_events_post_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/diagnostics/client_events")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"events":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn client_events_post_empty_batch_is_accepted() {
    // An empty batch is a no-op, not an error — the client may flush
    // every N seconds whether or not anything fired.
    let state = common::build_state(common::test_config()).await;
    let (status, json) = post_json(
        build_router(state),
        "/v1/diagnostics/client_events",
        json!({"events": []}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["accepted"], 0);
}

#[tokio::test]
async fn client_events_post_persists_batch_and_get_returns_them() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state.clone());

    let body = json!({
        "events": [
            {
                "session_id": "sess-1",
                "occurred_ms": 1_700_000_000_000_i64,
                "name": "web-vital.LCP",
                "value_ms": 1234.5,
                "rating": "good",
                "page_path": "/albums",
                "fields": {"id": "v3-snapshot"}
            },
            {
                "session_id": "sess-1",
                "occurred_ms": 1_700_000_000_500_i64,
                "name": "playback.start",
                "value_ms": 187.0,
                "page_path": "/albums/abc"
            }
        ]
    });
    let (status, posted) = post_json(
        app,
        "/v1/diagnostics/client_events",
        body,
        Some("Mozilla/5.0 (Test)"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(posted["accepted"], 2);

    let (status, got) = get_json(
        build_router(state),
        "/v1/diagnostics/client_events?limit=10",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = got["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    // Newest received first; second event in batch lands at index 0.
    assert_eq!(events[0]["name"], "playback.start");
    assert_eq!(events[1]["name"], "web-vital.LCP");
    // Server-stamped received_ms is present and >= occurred_ms of either.
    let received_ms = events[0]["received_ms"].as_i64().unwrap();
    assert!(received_ms > 0, "server stamps received_ms");
    // user_agent recorded from header.
    assert_eq!(events[0]["user_agent"], "Mozilla/5.0 (Test)");
    // rating only present where the client sent one.
    assert_eq!(events[1]["rating"], "good");
    assert!(events[0]["rating"].is_null());
}

#[tokio::test]
async fn client_events_post_truncates_long_user_agent() {
    // We don't trust the UA header — clamp to something sane (256 chars
    // is plenty for any browser UA ever shipped). Prevents a malicious
    // client from filling the table with multi-MB strings.
    let state = common::build_state(common::test_config()).await;
    let huge_ua = "A".repeat(10_000);
    let (status, _) = post_json(
        build_router(state.clone()),
        "/v1/diagnostics/client_events",
        json!({
            "events": [{
                "session_id": "s",
                "occurred_ms": 1,
                "name": "playback.start",
                "page_path": "/"
            }]
        }),
        Some(huge_ua.as_str()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, got) = get_json(
        build_router(state),
        "/v1/diagnostics/client_events?limit=10",
    )
    .await;
    let ua = got["events"][0]["user_agent"].as_str().unwrap();
    assert!(ua.len() <= 256, "user_agent truncated, got {}", ua.len());
}

#[tokio::test]
async fn client_events_post_oversized_batch_returns_413() {
    let state = common::build_state(common::test_config()).await;
    // 51 events > MAX_BATCH (50) — server should refuse the whole batch.
    let events: Vec<Value> = (0..51)
        .map(|i| {
            json!({
                "session_id": "s",
                "occurred_ms": i,
                "name": "noise",
                "page_path": "/"
            })
        })
        .collect();
    let (status, _) = post_json(
        build_router(state),
        "/v1/diagnostics/client_events",
        json!({"events": events}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn client_events_post_rejects_event_without_required_fields() {
    let state = common::build_state(common::test_config()).await;
    // Missing `name` — axum's Json extractor rejects with 422
    // Unprocessable Entity (the standard for body that's syntactically
    // JSON but semantically invalid). We just need to assert it's NOT a
    // 200 / 500 / silent accept.
    let (status, _) = post_json(
        build_router(state),
        "/v1/diagnostics/client_events",
        json!({"events": [{"session_id": "s", "occurred_ms": 1, "page_path": "/"}]}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

// --- GET /v1/diagnostics/client_events ------------------------------------

#[tokio::test]
async fn client_events_get_requires_auth() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/v1/diagnostics/client_events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn client_events_get_empty_returns_empty_array() {
    let state = common::build_state(common::test_config()).await;
    let (status, json) =
        get_json(build_router(state), "/v1/diagnostics/client_events").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["events"], json!([]));
}

#[tokio::test]
async fn client_events_get_filters_by_name() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state.clone());

    let body = json!({
        "events": [
            {"session_id":"s","occurred_ms":1,"name":"web-vital.LCP","page_path":"/"},
            {"session_id":"s","occurred_ms":2,"name":"web-vital.INP","page_path":"/"},
            {"session_id":"s","occurred_ms":3,"name":"web-vital.LCP","page_path":"/"}
        ]
    });
    let (status, _) =
        post_json(app, "/v1/diagnostics/client_events", body, None).await;
    assert_eq!(status, StatusCode::OK);

    let (status, json) = get_json(
        build_router(state),
        "/v1/diagnostics/client_events?name=web-vital.LCP",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = json["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e["name"] == "web-vital.LCP"));
}
