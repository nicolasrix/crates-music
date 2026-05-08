//! GET /oauth/authorize — OAuth 2.1 Authorization Code + PKCE.
//!
//! Validates response_type, client_id, redirect_uri (against the
//! registered list), code_challenge presence and method (S256 only). If
//! no session cookie, 302's to /oauth/login?next=<this_url>. If signed
//! in, mints an auth code, stores it (with the code_challenge for later
//! PKCE verification), and 302's to redirect_uri?code=…&state=….

use std::time::Duration;

use axum::body::Body;
use axum::http::header::{COOKIE, LOCATION};
use axum::http::{Request, StatusCode};
use music_gateway::build_router;
use music_gateway::oauth::{
    NewAuthCode, NewClient, OauthStore, SetupToken, password, session as session_mod,
};
use tower::ServiceExt;

mod common;

const SESSION_COOKIE: &str = "gw_session";

async fn store_with_session_and_client() -> (OauthStore, String) {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    let phc = password::hash("right-password-here").unwrap();
    oauth.set_master_password_hash(&phc).await.unwrap();
    let issued = oauth.create_session(Duration::from_hours(1)).await.unwrap();
    oauth
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web".to_string(),
            redirect_uris: vec!["http://localhost:5173/callback".to_string()],
        })
        .await
        .unwrap();
    (oauth, issued.token)
}

fn cookie_header(token: &str) -> String {
    format!("{SESSION_COOKIE}={token}")
}

// ---------------------------------------------------------------------
// Storage layer
// ---------------------------------------------------------------------

#[tokio::test]
async fn create_auth_code_returns_plaintext_and_stores_hash() {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    oauth
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web".to_string(),
            redirect_uris: vec!["http://x".to_string()],
        })
        .await
        .unwrap();
    let issued = oauth
        .create_auth_code(NewAuthCode {
            client_id: "web".to_string(),
            redirect_uri: "http://x".to_string(),
            code_challenge: "challenge".to_string(),
            ttl: Duration::from_mins(10),
        })
        .await
        .unwrap();
    assert!(!issued.code.is_empty());
    assert_eq!(issued.code_hash, session_mod::hash_token(&issued.code));
}

#[tokio::test]
async fn consume_auth_code_returns_row_then_refuses_replay() {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    oauth
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web".to_string(),
            redirect_uris: vec!["http://x".to_string()],
        })
        .await
        .unwrap();
    let issued = oauth
        .create_auth_code(NewAuthCode {
            client_id: "web".to_string(),
            redirect_uri: "http://x".to_string(),
            code_challenge: "abc123".to_string(),
            ttl: Duration::from_mins(10),
        })
        .await
        .unwrap();
    let consumed = oauth
        .consume_auth_code(&issued.code)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(consumed.client_id, "web");
    assert_eq!(consumed.redirect_uri, "http://x");
    assert_eq!(consumed.code_challenge, "abc123");
    // Replay is refused — a code may be used exactly once.
    assert!(
        oauth
            .consume_auth_code(&issued.code)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn consume_unknown_auth_code_returns_none() {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    assert!(oauth.consume_auth_code("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn consume_expired_auth_code_returns_none() {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    oauth
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web".to_string(),
            redirect_uris: vec!["http://x".to_string()],
        })
        .await
        .unwrap();
    let issued = oauth
        .create_auth_code(NewAuthCode {
            client_id: "web".to_string(),
            redirect_uri: "http://x".to_string(),
            code_challenge: "c".to_string(),
            ttl: Duration::from_secs(0),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        oauth
            .consume_auth_code(&issued.code)
            .await
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------------
// HTTP layer
// ---------------------------------------------------------------------

const VALID_QS: &str = "response_type=code\
    &client_id=web\
    &redirect_uri=http://localhost:5173/callback\
    &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM\
    &code_challenge_method=S256\
    &state=xyz";

#[tokio::test]
async fn authorize_redirects_to_login_when_no_session() {
    let (oauth, _token) = store_with_session_and_client().await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get(format!("/oauth/authorize?{VALID_QS}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get(LOCATION).unwrap().to_str().unwrap();
    assert!(
        location.starts_with("/oauth/login?next="),
        "must redirect to login with next= : {location}"
    );
    assert!(location.contains("authorize"));
}

#[tokio::test]
async fn authorize_with_session_redirects_to_client_with_code_and_state() {
    let (oauth, token) = store_with_session_and_client().await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth.clone(), SetupToken::none())
            .await;
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::get(format!("/oauth/authorize?{VALID_QS}"))
                .header(COOKIE, cookie_header(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get(LOCATION).unwrap().to_str().unwrap();
    assert!(location.starts_with("http://localhost:5173/callback?"));
    assert!(location.contains("code="));
    assert!(location.contains("state=xyz"));

    // The code is stored — extract it from the redirect, look it up.
    let code = extract_query_param(location, "code").unwrap();
    let consumed = oauth.consume_auth_code(&code).await.unwrap().unwrap();
    assert_eq!(consumed.client_id, "web");
    assert_eq!(consumed.redirect_uri, "http://localhost:5173/callback");
    assert_eq!(
        consumed.code_challenge,
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[tokio::test]
async fn authorize_rejects_unknown_client_id() {
    let (oauth, token) = store_with_session_and_client().await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);
    let qs = VALID_QS.replace("client_id=web", "client_id=ghost");
    let resp = app
        .oneshot(
            Request::get(format!("/oauth/authorize?{qs}"))
                .header(COOKIE, cookie_header(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn authorize_rejects_unregistered_redirect_uri() {
    let (oauth, token) = store_with_session_and_client().await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);
    let qs = VALID_QS.replace(
        "redirect_uri=http://localhost:5173/callback",
        "redirect_uri=http://evil.example/cb",
    );
    let resp = app
        .oneshot(
            Request::get(format!("/oauth/authorize?{qs}"))
                .header(COOKIE, cookie_header(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn authorize_rejects_response_type_other_than_code() {
    let (oauth, token) = store_with_session_and_client().await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);
    let qs = VALID_QS.replace("response_type=code", "response_type=token");
    let resp = app
        .oneshot(
            Request::get(format!("/oauth/authorize?{qs}"))
                .header(COOKIE, cookie_header(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // 400 directly: malformed authorization request, not redirected
    // back (RFC 6749 says we *may* redirect with error= but for OAuth 2.1
    // with PKCE-only, refusing outright is cleaner).
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn authorize_rejects_pkce_method_other_than_s256() {
    let (oauth, token) = store_with_session_and_client().await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);
    let qs = VALID_QS.replace("code_challenge_method=S256", "code_challenge_method=plain");
    let resp = app
        .oneshot(
            Request::get(format!("/oauth/authorize?{qs}"))
                .header(COOKIE, cookie_header(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn authorize_rejects_missing_code_challenge() {
    let (oauth, token) = store_with_session_and_client().await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);
    let qs = "response_type=code\
        &client_id=web\
        &redirect_uri=http://localhost:5173/callback\
        &code_challenge_method=S256\
        &state=xyz";
    let resp = app
        .oneshot(
            Request::get(format!("/oauth/authorize?{qs}"))
                .header(COOKIE, cookie_header(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn authorize_with_invalid_session_redirects_to_login() {
    let (oauth, _token) = store_with_session_and_client().await;
    let state =
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(
            Request::get(format!("/oauth/authorize?{VALID_QS}"))
                .header(COOKIE, cookie_header("not-a-real-session"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get(LOCATION).unwrap().to_str().unwrap();
    assert!(location.starts_with("/oauth/login?next="));
}

fn extract_query_param(url: &str, param: &str) -> Option<String> {
    let qs = url.split('?').nth(1)?;
    for kv in qs.split('&') {
        let (k, v) = kv.split_once('=')?;
        if k == param {
            return Some(v.to_string());
        }
    }
    None
}
