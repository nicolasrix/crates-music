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

async fn snapshot_json(
    app: axum::Router,
    auth: (&'static str, String),
) -> serde_json::Value {
    let resp = app
        .oneshot(
            Request::get("/v1/sync/snapshot")
                .header(auth.0, auth.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    body_json(resp).await
}

#[tokio::test]
async fn snapshot_omits_session_anchor_field_when_none() {
    // Default state: no session, no anchor. The wire payload must not
    // carry a `session_anchor` key at all — clients should only see
    // it when there's something to see.
    let app = build_router(common::build_state(common::test_config()).await);
    let (k, v) = auth_header();
    let snap = snapshot_json(app, (k, v)).await;
    assert!(
        snap["playback"].get("session_anchor").is_none(),
        "session_anchor must be absent from default snapshot, got: {snap}"
    );
}

#[tokio::test]
async fn post_start_session_sets_anchor_in_snapshot() {
    let app = build_router(common::build_state(common::test_config()).await);
    let (k, v) = auth_header();

    let before_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();

    let body = json!({
        "type": "start_session",
        "items": [
            {"item_id": "qi-1", "track_id": "t-1"},
            {"item_id": "qi-2", "track_id": "t-2"},
        ],
        "anchor_index": 0,
        "session_id": "sess-1",
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/v1/sync/ops")
                .header(k, v.clone())
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ack = body_json(resp).await;
    assert_eq!(ack["version"], 1);

    let after_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();

    let snap = snapshot_json(app, (k, v)).await;
    assert_eq!(snap["version"], 1);
    assert_eq!(snap["playback"]["queue"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(snap["playback"]["now_playing_index"], 0);
    assert_eq!(snap["playback"]["is_playing"], true);

    let anchor = &snap["playback"]["session_anchor"];
    assert_eq!(anchor["session_id"], "sess-1");
    assert_eq!(anchor["track_id"], "t-1");
    let started_ms = anchor["started_ms"].as_i64().expect("started_ms is integer");
    assert!(
        started_ms >= before_ms && started_ms <= after_ms,
        "server-stamped started_ms ({started_ms}) must fall within [{before_ms}, {after_ms}]"
    );
}

#[tokio::test]
async fn post_stop_session_clears_anchor_only_not_queue() {
    let app = build_router(common::build_state(common::test_config()).await);
    let (k, v) = auth_header();

    // Start a session, then stop it.
    let start_body = json!({
        "type": "start_session",
        "items": [{"item_id": "qi-1", "track_id": "t-1"}],
        "anchor_index": 0,
        "session_id": "sess-1",
    });
    app.clone()
        .oneshot(
            Request::post("/v1/sync/ops")
                .header(k, v.clone())
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(start_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    app.clone()
        .oneshot(
            Request::post("/v1/sync/ops")
                .header(k, v.clone())
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"type": "stop_session"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let snap = snapshot_json(app, (k, v)).await;
    assert_eq!(snap["version"], 2);
    assert!(
        snap["playback"].get("session_anchor").is_none(),
        "anchor must be absent after stop_session, got: {snap}"
    );
    // Queue and cursor survive — stop_session is intent-only.
    assert_eq!(
        snap["playback"]["queue"]["items"].as_array().unwrap().len(),
        1
    );
    assert_eq!(snap["playback"]["now_playing_index"], 0);
}

#[tokio::test]
async fn post_start_session_empty_items_returns_422() {
    let app = build_router(common::build_state(common::test_config()).await);
    let (k, v) = auth_header();
    let body = json!({
        "type": "start_session",
        "items": [],
        "anchor_index": 0,
        "session_id": "sess-1",
    });
    let resp = app
        .oneshot(
            Request::post("/v1/sync/ops")
                .header(k, v)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let json = body_json(resp).await;
    assert!(json["error"].is_string());
}

#[tokio::test]
async fn post_start_session_out_of_range_anchor_returns_422() {
    let app = build_router(common::build_state(common::test_config()).await);
    let (k, v) = auth_header();
    let body = json!({
        "type": "start_session",
        "items": [{"item_id": "qi-1", "track_id": "t-1"}],
        "anchor_index": 5,
        "session_id": "sess-1",
    });
    let resp = app
        .oneshot(
            Request::post("/v1/sync/ops")
                .header(k, v)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
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

// --- session lifecycle persistence (migration 0008) -----------------

async fn post_op(
    app: &axum::Router,
    auth: &(&str, String),
    body: serde_json::Value,
) -> StatusCode {
    let resp = app
        .clone()
        .oneshot(
            Request::post("/v1/sync/ops")
                .header(auth.0, auth.1.clone())
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
}

#[tokio::test]
async fn start_session_persists_a_recommend_sessions_row() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state.clone());
    let auth = auth_header();
    let body = json!({
        "type": "start_session",
        "items": [
            {"item_id": "qi-1", "track_id": "t-1"},
            {"item_id": "qi-2", "track_id": "t-2"},
            {"item_id": "qi-3", "track_id": "t-3"},
        ],
        "anchor_index": 1,
        "session_id": "sess-persist-1",
    });
    assert_eq!(post_op(&app, &auth, body).await, StatusCode::OK);

    let sid = music_core::SessionId::from("sess-persist-1".to_string());
    let row = state.sessions().get(&sid).await.unwrap().expect("row persisted");
    assert_eq!(row.anchor_track_id.as_str(), "t-2");
    assert_eq!(row.items_count, 3);
    assert!(row.ended_ms.is_none());
}

#[tokio::test]
async fn stop_session_closes_persisted_row() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state.clone());
    let auth = auth_header();
    let start = json!({
        "type": "start_session",
        "items": [{"item_id": "qi-1", "track_id": "t-1"}],
        "anchor_index": 0,
        "session_id": "sess-stop-1",
    });
    assert_eq!(post_op(&app, &auth, start).await, StatusCode::OK);
    let stop = json!({"type": "stop_session"});
    assert_eq!(post_op(&app, &auth, stop).await, StatusCode::OK);

    let sid = music_core::SessionId::from("sess-stop-1".to_string());
    let row = state.sessions().get(&sid).await.unwrap().expect("row persisted");
    assert!(
        row.ended_ms.is_some(),
        "stop_session must stamp ended_ms; got {row:?}"
    );
    // active() must reflect the close.
    assert!(state.sessions().active().await.unwrap().is_none());
}

#[tokio::test]
async fn starting_new_session_closes_the_previous_persisted_row() {
    // Mirrors the single-active invariant: the in-memory anchor flips,
    // and the persisted row for the old session gets stamped with the
    // new session's start time.
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state.clone());
    let auth = auth_header();
    let s1 = json!({
        "type": "start_session",
        "items": [{"item_id": "qi-1", "track_id": "t-1"}],
        "anchor_index": 0,
        "session_id": "sess-prev",
    });
    let s2 = json!({
        "type": "start_session",
        "items": [{"item_id": "qi-2", "track_id": "t-2"}],
        "anchor_index": 0,
        "session_id": "sess-next",
    });
    assert_eq!(post_op(&app, &auth, s1).await, StatusCode::OK);
    assert_eq!(post_op(&app, &auth, s2).await, StatusCode::OK);

    let prev = state
        .sessions()
        .get(&music_core::SessionId::from("sess-prev".to_string()))
        .await
        .unwrap()
        .unwrap();
    assert!(prev.ended_ms.is_some(), "previous session must be closed");

    let active = state.sessions().active().await.unwrap().unwrap();
    assert_eq!(active.session_id.as_str(), "sess-next");
}

#[tokio::test]
async fn clear_op_implicitly_stops_the_active_persisted_session() {
    // Clear nulls session_anchor in-memory; the persisted mirror must
    // match — otherwise an orphan "active" row would linger forever
    // after a queue clear.
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state.clone());
    let auth = auth_header();
    let start = json!({
        "type": "start_session",
        "items": [{"item_id": "qi-1", "track_id": "t-1"}],
        "anchor_index": 0,
        "session_id": "sess-clear",
    });
    assert_eq!(post_op(&app, &auth, start).await, StatusCode::OK);
    let clear = json!({"type": "clear"});
    assert_eq!(post_op(&app, &auth, clear).await, StatusCode::OK);

    let row = state
        .sessions()
        .get(&music_core::SessionId::from("sess-clear".to_string()))
        .await
        .unwrap()
        .unwrap();
    assert!(row.ended_ms.is_some(), "clear must close the active session");
    assert!(state.sessions().active().await.unwrap().is_none());
}

#[tokio::test]
async fn sync_active_session_id_reads_from_in_memory_anchor() {
    let state = common::build_state(common::test_config()).await;
    let app = build_router(state.clone());
    let auth = auth_header();
    assert!(state.sync().active_session_id().await.is_none());
    let start = json!({
        "type": "start_session",
        "items": [{"item_id": "qi-1", "track_id": "t-1"}],
        "anchor_index": 0,
        "session_id": "sess-active",
    });
    assert_eq!(post_op(&app, &auth, start).await, StatusCode::OK);
    let active = state.sync().active_session_id().await.unwrap();
    assert_eq!(active.as_str(), "sess-active");
}
