//! Per-user taste isolation (PR E of the user-system plan).
//!
//! Proves the HTTP-boundary contract that gateway-owned taste state
//! partitions by the calling principal's `user_id`:
//!
//!   * one User's like/dislike is invisible to another User,
//!   * a guest cannot write ratings (403) and their events are dropped
//!     from training (accepted, but persisted nowhere),
//!   * a guest's own ratings list is empty — they read their own
//!     (empty) partition, not the host's library.
//!
//! Uses a real router and real tokens minted for real `users` rows. A
//! single `AppState` is shared across requests (cloned per `oneshot`) so
//! every call hits the same in-memory recommend DB.

use std::time::Duration;

use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{Request, StatusCode};
use music_gateway::oauth::{NewClient, NewUser, OauthStore, SetupToken};
use music_gateway::state::AppState;
use music_gateway::build_router;
use tower::ServiceExt;

mod common;

/// Far-future guest expiry so `resolve_principal`'s expiry check passes.
const GUEST_EXPIRES_MS: i64 = 32_503_680_000_000; // year 3000

struct Harness {
    state: AppState,
    owner_token: String,
    alice_token: String,
    guest_token: String,
}

async fn harness() -> Harness {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    // Owner: id=1, admin (the way /oauth/setup seeds it).
    oauth
        .set_master_password_hash("$argon2id$dummy")
        .await
        .unwrap();
    oauth
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web".to_string(),
            redirect_uris: vec!["http://localhost:5173/cb".to_string()],
        })
        .await
        .unwrap();
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
    // Guest attached to the owner's room (host_user_id = 1), expiring.
    let guest = oauth
        .insert_user(NewUser {
            username: None,
            display_name: Some("Guest".to_string()),
            role: "guest".to_string(),
            password_hash: None,
            host_user_id: Some(1),
            expires_at_unix_ms: Some(GUEST_EXPIRES_MS),
        })
        .await
        .unwrap();

    let mint = |uid: i64| {
        let oauth = oauth.clone();
        async move {
            oauth
                .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(uid))
                .await
                .unwrap()
                .token
        }
    };
    let owner_token = mint(1).await;
    let alice_token = mint(alice).await;
    let guest_token = mint(guest).await;

    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    Harness {
        state,
        owner_token,
        alice_token,
        guest_token,
    }
}

async fn put_rating(state: &AppState, token: &str, id: &str, rating: &str) -> StatusCode {
    let body = serde_json::json!({ "kind": "track", "id": id, "rating": rating });
    build_router(state.clone())
        .oneshot(
            Request::put("/v1/library/rating")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// Returns the set of rated track ids the token's principal can see.
async fn list_rated_ids(state: &AppState, token: &str) -> Vec<String> {
    let resp = build_router(state.clone())
        .oneshot(
            Request::get("/v1/library/ratings")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    json["ratings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn ratings_are_isolated_between_users() {
    let h = harness().await;

    // Each user likes a distinct track.
    assert_eq!(
        put_rating(&h.state, &h.owner_token, "t-owner", "like").await,
        StatusCode::OK
    );
    assert_eq!(
        put_rating(&h.state, &h.alice_token, "t-alice", "like").await,
        StatusCode::OK
    );

    // Neither sees the other's verdict.
    let owner_ids = list_rated_ids(&h.state, &h.owner_token).await;
    let alice_ids = list_rated_ids(&h.state, &h.alice_token).await;
    assert_eq!(owner_ids, vec!["t-owner".to_string()]);
    assert_eq!(alice_ids, vec!["t-alice".to_string()]);
}

#[tokio::test]
async fn same_track_rated_independently_by_two_users() {
    let h = harness().await;
    // Owner dislikes a track; alice likes the *same* track id. The
    // verdicts are separate rows under separate user_ids.
    assert_eq!(
        put_rating(&h.state, &h.owner_token, "shared", "dislike").await,
        StatusCode::OK
    );
    assert_eq!(
        put_rating(&h.state, &h.alice_token, "shared", "like").await,
        StatusCode::OK
    );
    // Both still see exactly one rating for "shared" — their own.
    assert_eq!(
        list_rated_ids(&h.state, &h.owner_token).await,
        vec!["shared".to_string()]
    );
    assert_eq!(
        list_rated_ids(&h.state, &h.alice_token).await,
        vec!["shared".to_string()]
    );
}

#[tokio::test]
async fn guest_cannot_rate() {
    let h = harness().await;
    assert_eq!(
        put_rating(&h.state, &h.guest_token, "t-x", "like").await,
        StatusCode::FORBIDDEN,
        "a guest must be 403'd from writing taste"
    );
}

#[tokio::test]
async fn guest_ratings_list_is_their_own_empty_partition() {
    let h = harness().await;
    // Host has likes…
    assert_eq!(
        put_rating(&h.state, &h.owner_token, "t-host", "like").await,
        StatusCode::OK
    );
    // …but the guest reads their *own* user_id partition (empty), not the
    // host's library. (Guests get the host's *recommendations*, but never
    // a personal library of their own.)
    assert!(
        list_rated_ids(&h.state, &h.guest_token).await.is_empty(),
        "guest must not see the host's likes in their ratings list"
    );
}

#[tokio::test]
async fn guest_events_are_accepted_but_dropped() {
    let h = harness().await;
    let body = serde_json::json!({
        "events": [
            { "event_type": "scrobble", "track_id": "t-guest", "occurred_at": 1_000 }
        ]
    });
    let resp = build_router(h.state.clone())
        .oneshot(
            Request::post("/v1/events")
                .header(AUTHORIZATION, format!("Bearer {}", h.guest_token))
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        json["accepted"], 0,
        "guest events must be dropped from training (accepted, persisted nowhere)"
    );
}

#[tokio::test]
async fn user_events_are_accepted_and_counted() {
    let h = harness().await;
    let body = serde_json::json!({
        "events": [
            { "event_type": "scrobble", "track_id": "t-a", "occurred_at": 1_000 },
            { "event_type": "skip", "track_id": "t-b", "occurred_at": 2_000 }
        ]
    });
    let resp = build_router(h.state.clone())
        .oneshot(
            Request::post("/v1/events")
                .header(AUTHORIZATION, format!("Bearer {}", h.alice_token))
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["accepted"], 2, "a real user's events feed training");
}
