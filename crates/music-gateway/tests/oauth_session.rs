//! Browser session storage. Cookie is an opaque random token — server
//! stores `sha256(token)` only; presented tokens are hashed and looked
//! up in constant time by the SQLite primary-key index.

use std::time::Duration;

use music_gateway::oauth::{OauthStore, session};

#[tokio::test]
async fn create_then_find_session_round_trips() {
    let store = OauthStore::open_in_memory().await.unwrap();
    let issued = store.create_session(Duration::from_hours(1)).await.unwrap();
    let found = store.find_session(&issued.token).await.unwrap().unwrap();
    assert_eq!(found.token_hash, session::hash_token(&issued.token));
    assert!(found.expires_at_unix_ms > found.issued_at_unix_ms);
}

#[tokio::test]
async fn find_session_returns_none_for_unknown_token() {
    let store = OauthStore::open_in_memory().await.unwrap();
    let found = store.find_session("not-a-real-token").await.unwrap();
    assert!(found.is_none());
}

#[tokio::test]
async fn find_session_returns_none_after_expiry() {
    // Negative TTL → expires immediately. Find must treat the row as
    // non-existent without requiring a sweep job to delete it.
    let store = OauthStore::open_in_memory().await.unwrap();
    let issued = store.create_session(Duration::from_secs(0)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(store.find_session(&issued.token).await.unwrap().is_none());
}

#[tokio::test]
async fn revoke_session_makes_find_return_none() {
    let store = OauthStore::open_in_memory().await.unwrap();
    let issued = store.create_session(Duration::from_hours(1)).await.unwrap();
    assert!(store.find_session(&issued.token).await.unwrap().is_some());
    store.revoke_session(&issued.token).await.unwrap();
    assert!(store.find_session(&issued.token).await.unwrap().is_none());
}

#[tokio::test]
async fn revoke_unknown_session_is_a_no_op() {
    let store = OauthStore::open_in_memory().await.unwrap();
    // Must not error; revoking a non-existent token is a benign call
    // (e.g. user clicked "log out" twice).
    store.revoke_session("nonexistent").await.unwrap();
}

#[tokio::test]
async fn session_tokens_are_unique_per_creation() {
    let store = OauthStore::open_in_memory().await.unwrap();
    let a = store.create_session(Duration::from_mins(1)).await.unwrap();
    let b = store.create_session(Duration::from_mins(1)).await.unwrap();
    assert_ne!(a.token, b.token, "every session must mint a fresh token");
}
