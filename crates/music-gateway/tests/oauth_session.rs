//! Browser session storage. Cookie is an opaque random token — server
//! stores `sha256(token)` only; presented tokens are hashed and looked
//! up in constant time by the SQLite primary-key index.

use std::time::Duration;

use music_gateway::oauth::{NewClient, NewRefreshToken, NewUser, OauthStore, session};

#[tokio::test]
async fn create_then_find_session_round_trips() {
    let store = OauthStore::open_in_memory().await.unwrap();
    // Sessions now FK-reference users(id); seed the owner (id=1).
    store
        .set_master_password_hash("$argon2id$dummy")
        .await
        .unwrap();
    let issued = store.create_session(1, Duration::from_hours(1)).await.unwrap();
    let found = store.find_session(&issued.token).await.unwrap().unwrap();
    assert_eq!(found.token_hash, session::hash_token(&issued.token));
    assert!(found.expires_at_unix_ms > found.issued_at_unix_ms);
}

#[tokio::test]
async fn find_session_returns_none_for_unknown_token() {
    let store = OauthStore::open_in_memory().await.unwrap();
    // Sessions now FK-reference users(id); seed the owner (id=1).
    store
        .set_master_password_hash("$argon2id$dummy")
        .await
        .unwrap();
    let found = store.find_session("not-a-real-token").await.unwrap();
    assert!(found.is_none());
}

#[tokio::test]
async fn find_session_returns_none_after_expiry() {
    // Negative TTL → expires immediately. Find must treat the row as
    // non-existent without requiring a sweep job to delete it.
    let store = OauthStore::open_in_memory().await.unwrap();
    // Sessions now FK-reference users(id); seed the owner (id=1).
    store
        .set_master_password_hash("$argon2id$dummy")
        .await
        .unwrap();
    let issued = store.create_session(1, Duration::from_secs(0)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(store.find_session(&issued.token).await.unwrap().is_none());
}

#[tokio::test]
async fn revoke_session_makes_find_return_none() {
    let store = OauthStore::open_in_memory().await.unwrap();
    // Sessions now FK-reference users(id); seed the owner (id=1).
    store
        .set_master_password_hash("$argon2id$dummy")
        .await
        .unwrap();
    let issued = store.create_session(1, Duration::from_hours(1)).await.unwrap();
    assert!(store.find_session(&issued.token).await.unwrap().is_some());
    store.revoke_session(&issued.token).await.unwrap();
    assert!(store.find_session(&issued.token).await.unwrap().is_none());
}

#[tokio::test]
async fn revoke_unknown_session_is_a_no_op() {
    let store = OauthStore::open_in_memory().await.unwrap();
    // Sessions now FK-reference users(id); seed the owner (id=1).
    store
        .set_master_password_hash("$argon2id$dummy")
        .await
        .unwrap();
    // Must not error; revoking a non-existent token is a benign call
    // (e.g. user clicked "log out" twice).
    store.revoke_session("nonexistent").await.unwrap();
}

#[tokio::test]
async fn revoke_all_sessions_for_user_kills_only_that_users_sessions() {
    let store = OauthStore::open_in_memory().await.unwrap();
    store.set_master_password_hash("$argon2id$dummy").await.unwrap();
    let other = store
        .insert_user(NewUser {
            username: Some("bob".into()),
            display_name: None,
            role: "user".into(),
            password_hash: Some("$argon2id$dummy".into()),
            host_user_id: None,
            expires_at_unix_ms: None,
        })
        .await
        .unwrap();

    // Owner (id=1) gets two sessions; bob gets one.
    let a = store.create_session(1, Duration::from_hours(1)).await.unwrap();
    let b = store.create_session(1, Duration::from_hours(1)).await.unwrap();
    let bob = store.create_session(other, Duration::from_hours(1)).await.unwrap();

    let revoked = store.revoke_all_sessions_for_user(1).await.unwrap();
    assert_eq!(revoked, 2, "both owner sessions revoked");
    assert!(store.find_session(&a.token).await.unwrap().is_none());
    assert!(store.find_session(&b.token).await.unwrap().is_none());
    // bob's session is untouched.
    assert!(store.find_session(&bob.token).await.unwrap().is_some());
}

#[tokio::test]
async fn revoke_all_tokens_for_user_kills_refresh_and_access() {
    let store = OauthStore::open_in_memory().await.unwrap();
    store.set_master_password_hash("$argon2id$dummy").await.unwrap();
    store
        .register_client(NewClient {
            client_id: "web".into(),
            name: "Web".into(),
            redirect_uris: vec!["http://localhost:5173/cb".into()],
        })
        .await
        .unwrap();

    // Owner refresh + access (access derived from the refresh).
    let refresh = store
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".into(),
            ttl: None,
            family_id: None,
        })
        .await
        .unwrap();
    let access = store
        .mint_access_token_for_user("web", Some(&refresh.token_hash), Duration::from_hours(1), Some(1))
        .await
        .unwrap();

    // A second user whose tokens must survive the owner's reset.
    let bob = store
        .insert_user(NewUser {
            username: Some("bob".into()),
            display_name: None,
            role: "user".into(),
            password_hash: Some("$argon2id$dummy".into()),
            host_user_id: None,
            expires_at_unix_ms: None,
        })
        .await
        .unwrap();
    let bob_access = store
        .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(bob))
        .await
        .unwrap();

    let revoked = store.revoke_all_tokens_for_user(1).await.unwrap();
    assert_eq!(revoked, 2, "owner refresh + access both revoked");
    assert!(store.find_refresh_token(&refresh.token).await.unwrap().is_none());
    assert!(store.find_access_token(&access.token).await.unwrap().is_none());
    // bob keeps his access token.
    assert!(store.find_access_token(&bob_access.token).await.unwrap().is_some());
}

#[tokio::test]
async fn session_tokens_are_unique_per_creation() {
    let store = OauthStore::open_in_memory().await.unwrap();
    // Sessions now FK-reference users(id); seed the owner (id=1).
    store
        .set_master_password_hash("$argon2id$dummy")
        .await
        .unwrap();
    let a = store.create_session(1, Duration::from_mins(1)).await.unwrap();
    let b = store.create_session(1, Duration::from_mins(1)).await.unwrap();
    assert_ne!(a.token, b.token, "every session must mint a fresh token");
}
