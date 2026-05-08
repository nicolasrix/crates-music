//! Integration tests for `POST /v1/events`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_gateway::build_router;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::{TEST_BEARER, build_state, test_config};

async fn read_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("response body is JSON")
}

fn auth_post(body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/events")
        .header("authorization", format!("Bearer {TEST_BEARER}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn events_accepts_well_formed_batch() {
    let state = build_state(test_config()).await;
    let app = build_router(state.clone());

    let body = json!({
        "events": [
            {"event_type": "scrobble", "track_id": "t1", "occurred_at": 1000, "metadata": {"played_ms": 180_000}},
            {"event_type": "skip", "track_id": "t2", "occurred_at": 1500},
            {"event_type": "like", "track_id": "t3", "occurred_at": 2000}
        ]
    });
    let resp = app.oneshot(auth_post(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body = read_json(resp).await;
    assert_eq!(body["accepted"], 3);

    assert_eq!(state.event_store().count().await.unwrap(), 3);
}

#[tokio::test]
async fn events_rejects_empty_array() {
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let resp = app
        .oneshot(auth_post(&json!({"events": []})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn events_rejects_invalid_json() {
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/events")
        .header("authorization", format!("Bearer {TEST_BEARER}"))
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn events_requires_auth() {
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let req = Request::builder()
        .method("POST")
        .uri("/v1/events")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"events": [{"event_type": "scrobble", "track_id": "t1", "occurred_at": 0}]})
                .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn events_caps_oversized_batches() {
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let mut events = Vec::new();
    for i in 0..1_001 {
        events.push(json!({
            "event_type": "scrobble",
            "track_id": format!("t{i}"),
            "occurred_at": i,
        }));
    }
    let resp = app
        .oneshot(auth_post(&json!({"events": events})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn events_accepts_unknown_event_type() {
    let state = build_state(test_config()).await;
    let app = build_router(state.clone());

    // Forward-compatibility: the server doesn't reject unknown event
    // types, it just stores them. The recommender will decide what to
    // do with them later.
    let body = json!({
        "events": [
            {"event_type": "hover", "track_id": "t1", "occurred_at": 1000}
        ]
    });
    let resp = app.oneshot(auth_post(&body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert_eq!(state.event_store().count().await.unwrap(), 1);
}

#[tokio::test]
async fn events_persists_metadata_intact() {
    let state = build_state(test_config()).await;
    let app = build_router(state.clone());

    let payload = json!({
        "events": [{
            "event_type": "seek",
            "track_id": "t1",
            "occurred_at": 5000,
            "metadata": {"from_ms": 0, "to_ms": 30500}
        }]
    });
    let resp = app.oneshot(auth_post(&payload)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let recent = state.event_store().recent(1).await.unwrap();
    assert_eq!(recent.len(), 1);
    let meta = recent[0].metadata.as_ref().expect("metadata present");
    assert_eq!(meta["to_ms"], 30500);
}
