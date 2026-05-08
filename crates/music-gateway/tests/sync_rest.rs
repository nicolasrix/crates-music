//! REST surface of the sync subsystem (P5.2): a snapshot read and a
//! single-op write endpoint, both behind the bearer guard.
//!
//! These exercise the in-memory `SyncStore` end-to-end through the axum
//! router. WebSocket fan-out is exercised separately in P5.3.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use music_gateway::build_router;
use serde_json::json;
use tower::ServiceExt;

mod common;

fn auth_header() -> (&'static str, String) {
    (
        header::AUTHORIZATION.as_str(),
        format!("Bearer {}", common::TEST_BEARER),
    )
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("response body must be valid JSON")
}

#[tokio::test]
async fn snapshot_requires_auth() {
    let app = build_router(common::build_state(common::test_config()).await);
    let resp = app
        .oneshot(
            Request::get("/v1/sync/snapshot")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn snapshot_returns_empty_initial_state() {
    let app = build_router(common::build_state(common::test_config()).await);
    let (k, v) = auth_header();
    let resp = app
        .oneshot(
            Request::get("/v1/sync/snapshot")
                .header(k, v)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["version"], 0);
    assert_eq!(json["playback"]["queue"]["items"], serde_json::json!([]));
    assert!(json["playback"]["now_playing_index"].is_null());
    assert_eq!(json["playback"]["position_ms"], 0);
    assert_eq!(json["playback"]["is_playing"], false);
}

#[tokio::test]
async fn post_op_requires_auth() {
    let app = build_router(common::build_state(common::test_config()).await);
    let resp = app
        .oneshot(
            Request::post("/v1/sync/ops")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"type":"set_position","position_ms":1}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn post_push_op_appends_and_returns_new_version() {
    let app = build_router(common::build_state(common::test_config()).await);
    let (k, v) = auth_header();
    let resp = app
        .clone()
        .oneshot(
            Request::post("/v1/sync/ops")
                .header(k, v.clone())
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"type":"push","item_id":"qi-1","track_id":"t-1"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["version"], 1);

    // Snapshot now reflects the push.
    let snap = app
        .oneshot(
            Request::get("/v1/sync/snapshot")
                .header(k, v)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let snap_json = body_json(snap).await;
    assert_eq!(snap_json["version"], 1);
    assert_eq!(
        snap_json["playback"]["queue"]["items"][0]["item_id"],
        "qi-1"
    );
    assert_eq!(
        snap_json["playback"]["queue"]["items"][0]["track_id"],
        "t-1"
    );
}

#[tokio::test]
async fn post_op_rejects_out_of_bounds_set_now_playing_with_422() {
    let app = build_router(common::build_state(common::test_config()).await);
    let (k, v) = auth_header();
    let resp = app
        .oneshot(
            Request::post("/v1/sync/ops")
                .header(k, v)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"type":"set_now_playing","index":99}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let json = body_json(resp).await;
    assert!(
        json["error"].is_string(),
        "rejection body must carry a human-readable error: {json:?}"
    );
}

#[tokio::test]
async fn post_op_rejects_malformed_json_with_400() {
    let app = build_router(common::build_state(common::test_config()).await);
    let (k, v) = auth_header();
    let resp = app
        .oneshot(
            Request::post("/v1/sync/ops")
                .header(k, v)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("not valid json".to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_op_rejects_unknown_op_type_with_400() {
    let app = build_router(common::build_state(common::test_config()).await);
    let (k, v) = auth_header();
    let resp = app
        .oneshot(
            Request::post("/v1/sync/ops")
                .header(k, v)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"type":"teleport"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ops_are_linearized_across_concurrent_posts() {
    // Two concurrent Push ops must both apply, end up at version 2,
    // and produce a queue of length 2 — never lost or duplicated.
    let app = build_router(common::build_state(common::test_config()).await);
    let (k, v) = auth_header();
    let post = |item_id: &str, track_id: &str| {
        let app = app.clone();
        let v = v.clone();
        let body = json!({"type":"push","item_id":item_id,"track_id":track_id}).to_string();
        async move {
            app.oneshot(
                Request::post("/v1/sync/ops")
                    .header(k, v)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };
    let (a, b) = tokio::join!(post("qi-1", "t-1"), post("qi-2", "t-2"));
    assert_eq!(a.status(), StatusCode::OK);
    assert_eq!(b.status(), StatusCode::OK);

    let snap = app
        .oneshot(
            Request::get("/v1/sync/snapshot")
                .header(k, v)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json = body_json(snap).await;
    assert_eq!(json["version"], 2);
    assert_eq!(
        json["playback"]["queue"]["items"].as_array().unwrap().len(),
        2
    );
}
