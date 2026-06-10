//! Guest rooms (PR D of the user-system plan).
//!
//! A host (a real User) mints a shareable code; a visitor redeems it at
//! `POST /oauth/guest` and gets an ephemeral guest principal attached to
//! the host's room. These tests cover:
//!
//!   * **the grant** — redeem → guest token whose `whoami` reports
//!     `role=guest` + the host as `host_user_id`, and whose sync ops land
//!     in the *host's* room (shared jukebox, the D5 decision);
//!   * **code lifecycle** — max-uses, revocation, expiry all reject;
//!   * **host management** — `/v1/guest_codes` create/list/revoke, scoped
//!     per host, with guests forbidden;
//!   * **GC** — expired guest rows are reaped and cascade their tokens.

use std::time::Duration;

use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{Request, StatusCode};
use music_gateway::build_router;
use music_gateway::guest_codes;
use music_gateway::oauth::{NewClient, NewGuestCode, NewUser, OauthStore, RedeemOutcome, SetupToken};
use music_gateway::state::AppState;
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;

// ---- fixtures ----------------------------------------------------------

async fn seed_store() -> OauthStore {
    let oauth = OauthStore::open_in_memory().await.unwrap();
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
    oauth
}

/// Insert a real `user` account and return its id.
async fn make_user(oauth: &OauthStore, username: &str) -> i64 {
    oauth
        .insert_user(NewUser {
            username: Some(username.to_string()),
            display_name: Some(username.to_string()),
            role: "user".to_string(),
            password_hash: Some("$argon2id$dummy".to_string()),
            host_user_id: None,
            expires_at_unix_ms: None,
        })
        .await
        .unwrap()
}

async fn token_for(oauth: &OauthStore, user_id: i64) -> String {
    oauth
        .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(user_id))
        .await
        .unwrap()
        .token
}

/// POST /oauth/guest (form-encoded) and return (status, parsed-json).
async fn redeem(state: &AppState, code: &str) -> (StatusCode, Value) {
    let body = format!("code={code}&client_id=web");
    let resp = build_router(state.clone())
        .oneshot(
            Request::post("/oauth/guest")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn push(state: &AppState, token: &str, item_id: &str, track_id: &str) {
    let body = json!({ "type": "push", "item_id": item_id, "track_id": track_id });
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
    assert_eq!(resp.status(), StatusCode::OK, "push should be accepted");
}

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
    let state: music_sync::SyncState = serde_json::from_slice(&bytes).unwrap();
    state
        .playback
        .queue
        .items
        .iter()
        .map(|i| i.track_id.as_str().to_string())
        .collect()
}

async fn whoami(state: &AppState, token: &str) -> Value {
    let resp = build_router(state.clone())
        .oneshot(
            Request::get("/v1/whoami")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ---- the grant + shared room ------------------------------------------

#[tokio::test]
async fn guest_joins_host_room_and_shares_queue() {
    let oauth = seed_store().await;
    let host = make_user(&oauth, "alice").await;
    let host_token = token_for(&oauth, host).await;
    let issued = oauth
        .create_guest_code(NewGuestCode {
            host_user_id: host,
            label: Some("party".to_string()),
            expires_at_unix_ms: None,
            max_uses: None,
        })
        .await
        .unwrap();

    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;

    let (status, body) = redeem(&state, &issued.code).await;
    assert_eq!(status, StatusCode::OK, "redeem should succeed: {body:?}");
    assert_eq!(body["role"], "guest");
    assert_eq!(body["host_user_id"], host);
    assert!(body["refresh_token"].is_null(), "guest gets no refresh token");
    let guest_token = body["access_token"].as_str().unwrap().to_string();

    // whoami confirms the guest identity + host attachment.
    let who = whoami(&state, &guest_token).await;
    assert_eq!(who["role"], "guest");
    assert_eq!(who["host_user_id"], host);

    // The host pushes; the guest sees it (same room).
    push(&state, &host_token, "qi-host", "t-host").await;
    assert_eq!(snapshot_track_ids(&state, &guest_token).await, vec!["t-host"]);

    // The guest pushes; the host sees it too — a shared jukebox.
    push(&state, &guest_token, "qi-guest", "t-guest").await;
    assert_eq!(
        snapshot_track_ids(&state, &host_token).await,
        vec!["t-host", "t-guest"]
    );
}

#[tokio::test]
async fn unknown_code_is_rejected() {
    let oauth = seed_store().await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let (status, body) = redeem(&state, "BCDF-GHJK").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_grant");
}

// ---- code lifecycle (store level) -------------------------------------

#[tokio::test]
async fn max_uses_is_enforced() {
    let oauth = seed_store().await;
    let host = make_user(&oauth, "alice").await;
    let issued = oauth
        .create_guest_code(NewGuestCode {
            host_user_id: host,
            label: None,
            expires_at_unix_ms: None,
            max_uses: Some(1),
        })
        .await
        .unwrap();

    assert!(matches!(
        oauth.redeem_guest_code(&issued.code).await.unwrap(),
        RedeemOutcome::Ok { host_user_id } if host_user_id == host
    ));
    // Second redemption exceeds the cap.
    assert_eq!(
        oauth.redeem_guest_code(&issued.code).await.unwrap(),
        RedeemOutcome::Spent
    );
}

#[tokio::test]
async fn revoked_code_is_rejected() {
    let oauth = seed_store().await;
    let host = make_user(&oauth, "alice").await;
    let issued = oauth
        .create_guest_code(NewGuestCode {
            host_user_id: host,
            label: None,
            expires_at_unix_ms: None,
            max_uses: None,
        })
        .await
        .unwrap();
    assert!(oauth.revoke_guest_code(host, issued.id).await.unwrap());
    assert_eq!(
        oauth.redeem_guest_code(&issued.code).await.unwrap(),
        RedeemOutcome::Spent
    );
    // Re-revoking is a no-op (already revoked).
    assert!(!oauth.revoke_guest_code(host, issued.id).await.unwrap());
}

#[tokio::test]
async fn expired_code_is_rejected() {
    let oauth = seed_store().await;
    let host = make_user(&oauth, "alice").await;
    let issued = oauth
        .create_guest_code(NewGuestCode {
            host_user_id: host,
            label: None,
            expires_at_unix_ms: Some(1), // 1ms after epoch → long gone
            max_uses: None,
        })
        .await
        .unwrap();
    assert_eq!(
        oauth.redeem_guest_code(&issued.code).await.unwrap(),
        RedeemOutcome::Spent
    );
}

// ---- host management over HTTP ----------------------------------------

#[tokio::test]
async fn host_manages_codes_over_http() {
    let oauth = seed_store().await;
    let host = make_user(&oauth, "alice").await;
    let host_token = token_for(&oauth, host).await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;

    // Create.
    let resp = build_router(state.clone())
        .oneshot(
            Request::post("/v1/guest_codes")
                .header(AUTHORIZATION, format!("Bearer {host_token}"))
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "label": "kitchen ipad" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let code = created["code"].as_str().unwrap().to_string();
    let id = created["id"].as_i64().unwrap();

    // List shows it.
    let resp = build_router(state.clone())
        .oneshot(
            Request::get("/v1/guest_codes")
                .header(AUTHORIZATION, format!("Bearer {host_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let list: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["label"], "kitchen ipad");

    // The freshly minted code redeems.
    let (status, _) = redeem(&state, &code).await;
    assert_eq!(status, StatusCode::OK);

    // Revoke via DELETE → 204, and the code stops redeeming.
    let resp = build_router(state.clone())
        .oneshot(
            Request::delete(format!("/v1/guest_codes/{id}"))
                .header(AUTHORIZATION, format!("Bearer {host_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let (status, _) = redeem(&state, &code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn codes_are_isolated_between_hosts() {
    let oauth = seed_store().await;
    let alice = make_user(&oauth, "alice").await;
    let bob = make_user(&oauth, "bob").await;
    let alice_token = token_for(&oauth, alice).await;
    let bob_code = oauth
        .create_guest_code(NewGuestCode {
            host_user_id: bob,
            label: None,
            expires_at_unix_ms: None,
            max_uses: None,
        })
        .await
        .unwrap();
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;

    // Alice's list does not include Bob's code.
    let resp = build_router(state.clone())
        .oneshot(
            Request::get("/v1/guest_codes")
                .header(AUTHORIZATION, format!("Bearer {alice_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let list: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 0);

    // Alice cannot delete Bob's code → 404 (not her code).
    let resp = build_router(state.clone())
        .oneshot(
            Request::delete(format!("/v1/guest_codes/{}", bob_code.id))
                .header(AUTHORIZATION, format!("Bearer {alice_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn guest_cannot_manage_codes() {
    let oauth = seed_store().await;
    let host = make_user(&oauth, "alice").await;
    let issued = oauth
        .create_guest_code(NewGuestCode {
            host_user_id: host,
            label: None,
            expires_at_unix_ms: None,
            max_uses: None,
        })
        .await
        .unwrap();
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let (_, body) = redeem(&state, &issued.code).await;
    let guest_token = body["access_token"].as_str().unwrap().to_string();

    // Guests are forbidden from every management verb.
    for req in [
        Request::get("/v1/guest_codes")
            .header(AUTHORIZATION, format!("Bearer {guest_token}"))
            .body(Body::empty())
            .unwrap(),
        Request::post("/v1/guest_codes")
            .header(AUTHORIZATION, format!("Bearer {guest_token}"))
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap(),
        Request::delete("/v1/guest_codes/1")
            .header(AUTHORIZATION, format!("Bearer {guest_token}"))
            .body(Body::empty())
            .unwrap(),
    ] {
        let resp = build_router(state.clone()).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}

// ---- GC sweep ----------------------------------------------------------

#[tokio::test]
async fn expired_guests_are_reaped_with_their_tokens() {
    let oauth = seed_store().await;
    let host = make_user(&oauth, "alice").await;
    // A guest that expired in the past.
    let guest_id = oauth
        .insert_user(NewUser {
            username: None,
            display_name: Some("Guest".to_string()),
            role: "guest".to_string(),
            password_hash: None,
            host_user_id: Some(host),
            expires_at_unix_ms: Some(1),
        })
        .await
        .unwrap();
    let guest_token = token_for(&oauth, guest_id).await;
    // Sanity: the token exists pre-sweep (resolve still rejects it as a
    // lapsed guest, but the row is present until reaped).
    assert!(oauth.find_access_token(&guest_token).await.unwrap().is_some());

    let reaped = oauth.delete_expired_guests(i64::MAX).await.unwrap();
    assert_eq!(reaped, 1);

    // Cascade removed the access token; the host row is untouched.
    assert!(oauth.find_access_token(&guest_token).await.unwrap().is_none());
    assert!(oauth.user_profile(host).await.unwrap().is_some());
    // A live (non-expired) guest is not touched by the sweep.
    let live = oauth
        .insert_user(NewUser {
            username: None,
            display_name: Some("Guest".to_string()),
            role: "guest".to_string(),
            password_hash: None,
            host_user_id: Some(host),
            expires_at_unix_ms: Some(i64::MAX),
        })
        .await
        .unwrap();
    assert_eq!(oauth.delete_expired_guests(0).await.unwrap(), 0);
    assert!(oauth.user_profile(live).await.unwrap().is_some());
}

#[tokio::test]
async fn sweep_disabled_when_interval_zero() {
    let oauth = seed_store().await;
    assert!(guest_codes::spawn_guest_sweep(oauth, Duration::ZERO).is_none());
}
