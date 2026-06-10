//! Gateway-owned playlist CRUD + visibility + guest sandboxing (PR F).
//!
//! Drives the real router over `oneshot` with real tokens minted for real
//! `users` rows. Covers the authorization matrix the plan calls out in §8
//! and §12: owner CRUD, per-user privacy (private invisible to others,
//! shared readable-not-writable), and guests blocked from every mutation.

use std::time::Duration;

use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{Request, StatusCode};
use music_gateway::build_router;
use music_gateway::oauth::{NewClient, NewUser, OauthStore, SetupToken};
use music_gateway::state::AppState;
use serde_json::{Value, json};
use tower::ServiceExt;

mod common;

/// Far-future expiry so the guest principal stays valid for the test.
const GUEST_EXPIRES_MS: i64 = 32_503_680_000_000; // ~year 3000

struct Harness {
    state: AppState,
    owner_token: String,
    alice_token: String,
    guest_token: String,
}

async fn harness() -> Harness {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    // Owner = id 1, admin — seeded the way /oauth/setup would.
    oauth.set_master_password_hash("$argon2id$dummy").await.unwrap();
    oauth
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web".to_string(),
            redirect_uris: vec!["http://localhost:5173/cb".to_string()],
        })
        .await
        .unwrap();
    let alice_id = oauth
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
    let guest_id = oauth
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

    let owner_token = oauth
        .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(1))
        .await
        .unwrap()
        .token;
    let alice_token = oauth
        .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(alice_id))
        .await
        .unwrap()
        .token;
    let guest_token = oauth
        .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(guest_id))
        .await
        .unwrap()
        .token;

    let state = common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    Harness {
        state,
        owner_token,
        alice_token,
        guest_token,
    }
}

async fn send(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header(AUTHORIZATION, format!("Bearer {token}"));
    let req = match body {
        Some(b) => builder
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let resp = build_router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let val = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, val)
}

#[tokio::test]
async fn create_list_get_track_roundtrip() {
    let h = harness().await;

    // Create.
    let (status, body) = send(
        &h.state,
        "POST",
        "/v1/playlists",
        &h.owner_token,
        Some(json!({ "name": "  Roadtrip  " })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["name"], "Roadtrip", "name is trimmed");
    assert_eq!(body["owned"], true);
    assert_eq!(body["visibility"], "private");
    let id = body["id"].as_str().unwrap().to_string();

    // List shows it.
    let (status, body) = send(&h.state, "GET", "/v1/playlists", &h.owner_token, None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["playlists"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], id);
    assert_eq!(items[0]["song_count"], 0);

    // Replace membership.
    let (status, _) = send(
        &h.state,
        "PUT",
        &format!("/v1/playlists/{id}/tracks"),
        &h.owner_token,
        Some(json!({ "track_ids": ["t1", "t2"] })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Append one.
    let (status, _) = send(
        &h.state,
        "PUT",
        &format!("/v1/playlists/{id}/tracks"),
        &h.owner_token,
        Some(json!({ "track_ids": ["t3"], "mode": "append" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = send(
        &h.state,
        "GET",
        &format!("/v1/playlists/{id}"),
        &h.owner_token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["track_ids"], json!(["t1", "t2", "t3"]));
    assert_eq!(body["playlist"]["song_count"], 3);

    // Reorder via full replace.
    let (status, _) = send(
        &h.state,
        "PUT",
        &format!("/v1/playlists/{id}/tracks"),
        &h.owner_token,
        Some(json!({ "track_ids": ["t3", "t1", "t2"] })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(
        &h.state,
        "GET",
        &format!("/v1/playlists/{id}"),
        &h.owner_token,
        None,
    )
    .await;
    assert_eq!(body["track_ids"], json!(["t3", "t1", "t2"]));
}

#[tokio::test]
async fn guest_cannot_create_or_modify() {
    let h = harness().await;

    // Owner makes a playlist for the guest to poke at.
    let (_, body) = send(
        &h.state,
        "POST",
        "/v1/playlists",
        &h.owner_token,
        Some(json!({ "name": "Owner list" })),
    )
    .await;
    let id = body["id"].as_str().unwrap().to_string();

    // Guest create → 403 (WritePlaylist capability fires before anything else).
    let (status, _) = send(
        &h.state,
        "POST",
        "/v1/playlists",
        &h.guest_token,
        Some(json!({ "name": "Guest list" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Guest mutate someone else's playlist → 403, not 404.
    for (method, path, body) in [
        ("PATCH", format!("/v1/playlists/{id}"), Some(json!({ "name": "x" }))),
        (
            "PUT",
            format!("/v1/playlists/{id}/tracks"),
            Some(json!({ "track_ids": ["t1"] })),
        ),
        ("DELETE", format!("/v1/playlists/{id}"), None),
    ] {
        let (status, _) = send(&h.state, method, &path, &h.guest_token, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
    }
}

#[tokio::test]
async fn private_playlist_is_invisible_to_other_users() {
    let h = harness().await;

    // Alice creates a private playlist.
    let (_, body) = send(
        &h.state,
        "POST",
        "/v1/playlists",
        &h.alice_token,
        Some(json!({ "name": "Alice secret" })),
    )
    .await;
    let id = body["id"].as_str().unwrap().to_string();

    // Owner can't see it in the list...
    let (_, body) = send(&h.state, "GET", "/v1/playlists", &h.owner_token, None).await;
    assert!(body["playlists"].as_array().unwrap().is_empty());

    // ...nor fetch it directly (404, existence-hiding).
    let (status, _) = send(
        &h.state,
        "GET",
        &format!("/v1/playlists/{id}"),
        &h.owner_token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn shared_playlist_is_readable_not_writable_by_others() {
    let h = harness().await;

    // Alice creates then shares a playlist.
    let (_, body) = send(
        &h.state,
        "POST",
        "/v1/playlists",
        &h.alice_token,
        Some(json!({ "name": "Alice mixtape" })),
    )
    .await;
    let id = body["id"].as_str().unwrap().to_string();
    let (status, body) = send(
        &h.state,
        "PATCH",
        &format!("/v1/playlists/{id}"),
        &h.alice_token,
        Some(json!({ "visibility": "shared" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["visibility"], "shared");

    // Owner sees it (owned=false) and can read it.
    let (_, body) = send(&h.state, "GET", "/v1/playlists", &h.owner_token, None).await;
    let items = body["playlists"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["owned"], false);

    let (status, body) = send(
        &h.state,
        "GET",
        &format!("/v1/playlists/{id}"),
        &h.owner_token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["playlist"]["owned"], false);

    // But the owner (a real account, WritePlaylist-capable) still can't
    // mutate a playlist they don't own → 404, not 403.
    let (status, _) = send(
        &h.state,
        "DELETE",
        &format!("/v1/playlists/{id}"),
        &h.owner_token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_removes_playlist() {
    let h = harness().await;
    let (_, body) = send(
        &h.state,
        "POST",
        "/v1/playlists",
        &h.owner_token,
        Some(json!({ "name": "Throwaway" })),
    )
    .await;
    let id = body["id"].as_str().unwrap().to_string();

    let (status, _) = send(
        &h.state,
        "DELETE",
        &format!("/v1/playlists/{id}"),
        &h.owner_token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send(
        &h.state,
        "GET",
        &format!("/v1/playlists/{id}"),
        &h.owner_token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rejects_bad_input() {
    let h = harness().await;

    // Empty / whitespace name.
    let (status, _) = send(
        &h.state,
        "POST",
        "/v1/playlists",
        &h.owner_token,
        Some(json!({ "name": "   " })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Bad visibility on patch.
    let (_, body) = send(
        &h.state,
        "POST",
        "/v1/playlists",
        &h.owner_token,
        Some(json!({ "name": "Valid" })),
    )
    .await;
    let id = body["id"].as_str().unwrap().to_string();
    let (status, _) = send(
        &h.state,
        "PATCH",
        &format!("/v1/playlists/{id}"),
        &h.owner_token,
        Some(json!({ "visibility": "public" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
