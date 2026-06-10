//! Sync rooms (PR C of the user-system plan).
//!
//! Each principal reads/writes the queue for `principal.room_id()` — a
//! User's own id. These tests prove the partition at two levels:
//!
//!   * **HTTP boundary** — two Users driving `/v1/sync/{ops,snapshot}`
//!     with their own tokens get independent queues, and two *devices*
//!     of the **same** User share one queue (the cross-device point).
//!   * **Store** — a room's broadcast bus delivers an applied op only to
//!     that room's subscribers, never another room's.

use std::time::Duration;

use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{Request, StatusCode};
use music_gateway::build_router;
use music_gateway::oauth::{NewClient, NewUser, OauthStore, SetupToken};
use music_gateway::state::AppState;
use music_gateway::sync::SyncStore;
use music_sync::{SyncOp, SyncState};
use serde_json::json;
use tower::ServiceExt;

mod common;

// ---- HTTP-boundary isolation -------------------------------------------

/// Build an oauth store seeded with the owner + a registered client, and
/// return it ready for minting tokens.
async fn store_with_owner_and_client() -> OauthStore {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    oauth.set_master_password_hash("$argon2id$dummy").await.unwrap();
    oauth
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web".to_string(),
            redirect_uris: vec!["http://localhost:5173/cb".to_string()],
        })
        .await
        .unwrap();
    oauth
}

/// Mint an access token attributed to `user_id` against the shared store.
async fn token_for(oauth: &OauthStore, user_id: i64) -> String {
    oauth
        .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(user_id))
        .await
        .unwrap()
        .token
}

/// POST a sync op as `token`; assert 200.
async fn push(state: &AppState, token: &str, item_id: &str, track_id: &str) {
    let body = json!({
        "type": "push",
        "item_id": item_id,
        "track_id": track_id,
    });
    let resp = build_router(state.clone())
        .oneshot(
            Request::post("/v1/sync/ops")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "push op should be accepted");
}

/// GET the snapshot as `token` and return the queue's track ids in order.
async fn snapshot_track_ids(state: &AppState, token: &str) -> Vec<String> {
    let resp = build_router(state.clone())
        .oneshot(
            Request::get("/v1/sync/snapshot")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    let state: SyncState = serde_json::from_slice(&bytes).unwrap();
    state
        .playback
        .queue
        .items
        .iter()
        .map(|i| i.track_id.as_str().to_string())
        .collect()
}

#[tokio::test]
async fn two_users_have_independent_queues() {
    let oauth = store_with_owner_and_client().await;
    let alice = oauth
        .insert_user(NewUser {
            username: Some("alice".to_string()),
            display_name: Some("Alice".to_string()),
            role: "user".to_string(),
            password_hash: Some("$argon2id$dummy".to_string()),
            host_user_id: None,
            expires_at_unix_ms: None,
        })
        .await
        .unwrap();
    let owner_token = token_for(&oauth, 1).await;
    let alice_token = token_for(&oauth, alice).await;

    // One shared AppState so the SyncStore (and its rooms) persist across
    // the per-request routers.
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;

    push(&state, &owner_token, "qi-owner", "t-owner").await;
    push(&state, &alice_token, "qi-alice", "t-alice").await;

    // Each sees only their own push — the rooms never bleed.
    assert_eq!(snapshot_track_ids(&state, &owner_token).await, vec!["t-owner"]);
    assert_eq!(snapshot_track_ids(&state, &alice_token).await, vec!["t-alice"]);
}

#[tokio::test]
async fn two_devices_of_one_user_share_a_queue() {
    let oauth = store_with_owner_and_client().await;
    // Two *separate* tokens for the same user id — model two devices.
    let phone = token_for(&oauth, 1).await;
    let laptop = token_for(&oauth, 1).await;
    assert_ne!(phone, laptop, "distinct tokens");

    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;

    push(&state, &phone, "qi-1", "t-shared").await;
    // The laptop, a different device/token but the same user, sees it.
    assert_eq!(snapshot_track_ids(&state, &laptop).await, vec!["t-shared"]);
}

// ---- Store-level bus isolation -----------------------------------------

fn push_op(item_id: &str, track_id: &str) -> SyncOp {
    SyncOp::Push {
        item_id: music_core::QueueItemId::from(item_id),
        track_id: music_core::TrackId::from(track_id),
    }
}

#[tokio::test]
async fn broadcast_bus_is_per_room() {
    let store = SyncStore::new();
    let (_snap_a, mut rx_a) = store.subscribe(1).await;
    let (_snap_b, mut rx_b) = store.subscribe(2).await;

    // Apply only to room 1.
    store.apply(1, &push_op("qi-1", "t-1")).await.unwrap();

    // Room 1's subscriber receives it…
    let got = rx_a.try_recv().expect("room 1 subscriber sees room 1 op");
    assert_eq!(got.op, push_op("qi-1", "t-1"));
    assert_eq!(got.version, 1);

    // …room 2's subscriber sees nothing.
    assert!(
        matches!(rx_b.try_recv(), Err(tokio::sync::broadcast::error::TryRecvError::Empty)),
        "room 2 subscriber must not see room 1 ops",
    );
}

#[tokio::test]
async fn snapshots_are_per_room() {
    let store = SyncStore::new();
    store.apply(1, &push_op("qi-1", "t-1")).await.unwrap();
    store.apply(2, &push_op("qi-2", "t-2")).await.unwrap();

    let snap1 = store.snapshot(1).await;
    let snap2 = store.snapshot(2).await;
    assert_eq!(snap1.playback.queue.items.len(), 1);
    assert_eq!(snap1.playback.queue.items[0].track_id.as_str(), "t-1");
    assert_eq!(snap2.playback.queue.items.len(), 1);
    assert_eq!(snap2.playback.queue.items[0].track_id.as_str(), "t-2");

    // A never-touched room is empty, not a panic.
    assert_eq!(store.snapshot(999).await.playback.queue.items.len(), 0);
}
