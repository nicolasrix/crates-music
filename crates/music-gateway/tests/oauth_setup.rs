//! Master-password storage + bootstrap setup token.
//!
//! On first start, the gateway has no `users` row — it generates a
//! one-time setup token and prints it to stdout. The setup endpoint
//! accepts that token plus a chosen master password, hashes the
//! password (Argon2id), stores the hash, and burns the token. Once
//! `users` is populated, the setup endpoint refuses everything.

use axum::body::Body;
use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
use http_body_util::BodyExt;
use music_gateway::build_router;
use music_gateway::oauth::{OauthStore, SetupToken, password};
use tower::ServiceExt;

mod common;

#[tokio::test]
async fn fresh_store_has_no_master_password() {
    let store = OauthStore::open_in_memory().await.unwrap();
    assert!(store.master_password_hash().await.unwrap().is_none());
}

#[tokio::test]
async fn set_then_fetch_master_password_hash() {
    let store = OauthStore::open_in_memory().await.unwrap();
    let phc = password::hash("hunter2").unwrap();
    store.set_master_password_hash(&phc).await.unwrap();
    let stored = store.master_password_hash().await.unwrap().unwrap();
    assert_eq!(stored, phc);
}

#[tokio::test]
async fn setting_master_password_twice_fails() {
    // We don't support "change password" yet — that's an admin flow for
    // later. Today, two `set_master_password_hash` calls is a programming
    // error.
    let store = OauthStore::open_in_memory().await.unwrap();
    let phc = password::hash("first").unwrap();
    store.set_master_password_hash(&phc).await.unwrap();
    let phc2 = password::hash("second").unwrap();
    assert!(store.set_master_password_hash(&phc2).await.is_err());
}

#[tokio::test]
async fn setup_token_active_when_unset() {
    let token = SetupToken::generate();
    assert!(token.is_active());
}

#[tokio::test]
async fn setup_token_matches_constant_time() {
    let token = SetupToken::generate();
    let value = token.value().expect("just-generated token has a value");
    assert!(token.matches(&value));
    assert!(!token.matches("not-the-token"));
}

#[tokio::test]
async fn setup_token_consume_returns_value_then_clears() {
    let token = SetupToken::generate();
    let v = token.value().unwrap();
    assert_eq!(token.consume().as_deref(), Some(v.as_str()));
    assert!(!token.is_active());
    assert!(token.consume().is_none(), "consume is one-shot");
}

#[tokio::test]
async fn setup_token_inactive_when_none() {
    let token = SetupToken::none();
    assert!(!token.is_active());
    assert!(!token.matches("anything"));
}

// ------------------------------------------------------------------
// HTTP-level: /oauth/setup endpoint.
// ------------------------------------------------------------------

#[tokio::test]
async fn setup_endpoint_sets_password_and_burns_token() {
    let setup_token = SetupToken::generate();
    let token_value = setup_token.value().unwrap();
    let oauth = OauthStore::open_in_memory().await.unwrap();
    let state =
        common::build_state_with_oauth(common::test_config(), oauth.clone(), setup_token).await;
    let app = build_router(state);

    let body = format!("token={token_value}&password=hunter2-very-long");
    let resp = app
        .clone()
        .oneshot(
            Request::post("/oauth/setup")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Master password is now stored…
    let stored = oauth.master_password_hash().await.unwrap();
    assert!(stored.is_some(), "master password must be persisted");
    let phc = stored.unwrap();
    assert!(password::verify("hunter2-very-long", &phc).unwrap());

    // …and a second setup attempt is refused (token consumed).
    let resp2 = app
        .oneshot(
            Request::post("/oauth/setup")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "token={token_value}&password=hunter2-very-long"
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp2.status(),
        StatusCode::GONE,
        "setup is one-shot; subsequent calls must return 410"
    );
}

#[tokio::test]
async fn setup_endpoint_rejects_wrong_token() {
    let setup_token = SetupToken::generate();
    let oauth = OauthStore::open_in_memory().await.unwrap();
    let state =
        common::build_state_with_oauth(common::test_config(), oauth.clone(), setup_token).await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::post("/oauth/setup")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("token=wrong&password=hunter2-very-long"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(oauth.master_password_hash().await.unwrap().is_none());
}

#[tokio::test]
async fn setup_endpoint_410_when_already_configured() {
    // Pre-existing master password → setup endpoint is permanently disabled.
    let oauth = OauthStore::open_in_memory().await.unwrap();
    let phc = password::hash("preexisting").unwrap();
    oauth.set_master_password_hash(&phc).await.unwrap();
    let state =
        common::build_state_with_oauth(common::test_config(), oauth.clone(), SetupToken::none())
            .await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::post("/oauth/setup")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("token=anything&password=anything-very-long"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::GONE);
}

#[tokio::test]
async fn setup_endpoint_rejects_short_password() {
    let setup_token = SetupToken::generate();
    let token_value = setup_token.value().unwrap();
    let oauth = OauthStore::open_in_memory().await.unwrap();
    let state =
        common::build_state_with_oauth(common::test_config(), oauth.clone(), setup_token).await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::post("/oauth/setup")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("token={token_value}&password=short")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        std::str::from_utf8(&body).unwrap().contains("password"),
        "error body should mention password"
    );
    assert!(oauth.master_password_hash().await.unwrap().is_none());
}
