//! Device Authorization Grant (RFC 8628): `/oauth/device_authorization`,
//! the `device_code` token grant, and the browser approval page
//! (`GET/POST /oauth/device`).

use std::time::Duration;

use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, LOCATION};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_gateway::build_router;
use music_gateway::oauth::{DevicePollState, NewClient, NewDeviceCode, OauthStore, SetupToken, password};
use serde_json::Value;
use tower::ServiceExt;

mod common;

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

async fn store_with_cli_client() -> OauthStore {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    let phc = password::hash("master-password-here").unwrap();
    oauth.set_master_password_hash(&phc).await.unwrap();
    oauth
        .register_client(NewClient {
            client_id: "cli".to_string(),
            name: "CLI".to_string(),
            redirect_uris: vec![],
        })
        .await
        .unwrap();
    oauth
}

async fn build_app(oauth: OauthStore) -> axum::Router {
    build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    )
}

async fn post_form(app: axum::Router, path: &str, body: String) -> axum::http::Response<Body> {
    app.oneshot(
        Request::post(path)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn body_json(resp: axum::http::Response<Body>) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

// ---------------------------------------------------------------------
// device_authorization
// ---------------------------------------------------------------------

#[tokio::test]
async fn device_authorization_issues_codes() {
    let oauth = store_with_cli_client().await;
    let app = build_app(oauth).await;

    let (status, json) =
        body_json(post_form(app, "/oauth/device_authorization", "client_id=cli".into()).await)
            .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["device_code"].is_string());
    let user_code = json["user_code"].as_str().unwrap();
    assert!(user_code.contains('-'), "user code is formatted XXXX-XXXX");
    assert!(json["verification_uri"].as_str().unwrap().ends_with("/oauth/device"));
    assert!(
        json["verification_uri_complete"]
            .as_str()
            .unwrap()
            .contains("user_code=")
    );
    assert!(json["expires_in"].as_u64().unwrap() > 0);
    assert!(json["interval"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn device_authorization_rejects_unknown_client() {
    let oauth = store_with_cli_client().await;
    let app = build_app(oauth).await;

    let (status, json) = body_json(
        post_form(app, "/oauth/device_authorization", "client_id=ghost".into()).await,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"].as_str(), Some("invalid_client"));
}

// ---------------------------------------------------------------------
// device_code token grant (polling)
// ---------------------------------------------------------------------

async fn issued_device_code(oauth: &OauthStore, ttl: Duration) -> String {
    oauth
        .create_device_code(NewDeviceCode {
            client_id: "cli".to_string(),
            ttl,
            interval: Duration::from_secs(5),
        })
        .await
        .unwrap()
        .device_code
}

fn poll_body(device_code: &str) -> String {
    format!("grant_type={DEVICE_GRANT}&client_id=cli&device_code={device_code}")
}

#[tokio::test]
async fn poll_is_pending_before_approval() {
    let oauth = store_with_cli_client().await;
    let device_code = issued_device_code(&oauth, Duration::from_mins(10)).await;
    let app = build_app(oauth).await;

    let (status, json) =
        body_json(post_form(app, "/oauth/token", poll_body(&device_code)).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"].as_str(), Some("authorization_pending"));
}

#[tokio::test]
async fn poll_returns_tokens_after_approval() {
    let oauth = store_with_cli_client().await;
    let issued = oauth
        .create_device_code(NewDeviceCode {
            client_id: "cli".to_string(),
            ttl: Duration::from_mins(10),
            interval: Duration::from_secs(5),
        })
        .await
        .unwrap();
    assert!(
        oauth
            .set_device_decision(&issued.user_code, true)
            .await
            .unwrap()
    );
    let app = build_app(oauth.clone()).await;

    let (status, json) =
        body_json(post_form(app, "/oauth/token", poll_body(&issued.device_code)).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["access_token"].is_string());
    let rt = json["refresh_token"].as_str().unwrap();
    assert_eq!(json["token_type"].as_str(), Some("Bearer"));
    // The minted refresh token is real and scoped to the CLI client.
    let found = oauth.find_refresh_token(rt).await.unwrap().unwrap();
    assert_eq!(found.client_id, "cli");
}

#[tokio::test]
async fn poll_is_denied_after_deny() {
    let oauth = store_with_cli_client().await;
    let issued = oauth
        .create_device_code(NewDeviceCode {
            client_id: "cli".to_string(),
            ttl: Duration::from_mins(10),
            interval: Duration::from_secs(5),
        })
        .await
        .unwrap();
    assert!(
        oauth
            .set_device_decision(&issued.user_code, false)
            .await
            .unwrap()
    );
    let app = build_app(oauth).await;

    let (status, json) =
        body_json(post_form(app, "/oauth/token", poll_body(&issued.device_code)).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"].as_str(), Some("access_denied"));
}

#[tokio::test]
async fn poll_expired_code_is_expired_token() {
    let oauth = store_with_cli_client().await;
    let device_code = issued_device_code(&oauth, Duration::ZERO).await;
    let app = build_app(oauth).await;

    let (status, json) =
        body_json(post_form(app, "/oauth/token", poll_body(&device_code)).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"].as_str(), Some("expired_token"));
}

#[tokio::test]
async fn device_code_is_single_use() {
    let oauth = store_with_cli_client().await;
    let issued = oauth
        .create_device_code(NewDeviceCode {
            client_id: "cli".to_string(),
            ttl: Duration::from_mins(10),
            interval: Duration::from_secs(5),
        })
        .await
        .unwrap();
    oauth
        .set_device_decision(&issued.user_code, true)
        .await
        .unwrap();
    let app = build_app(oauth).await;

    let first = post_form(app.clone(), "/oauth/token", poll_body(&issued.device_code)).await;
    assert_eq!(first.status(), StatusCode::OK);
    let (status, json) =
        body_json(post_form(app, "/oauth/token", poll_body(&issued.device_code)).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        json["error"].as_str(),
        Some("expired_token"),
        "a consumed device code can't mint a second pair"
    );
}

// ---------------------------------------------------------------------
// Browser approval page (session-gated)
// ---------------------------------------------------------------------

#[tokio::test]
async fn device_page_requires_login() {
    let oauth = store_with_cli_client().await;
    let app = build_app(oauth).await;

    let resp = app
        .oneshot(
            Request::get("/oauth/device?user_code=BCDF-GHJK")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let loc = resp.headers().get(LOCATION).unwrap().to_str().unwrap();
    assert!(loc.starts_with("/oauth/login?next="), "bounced to login: {loc}");
}

#[tokio::test]
async fn device_page_approve_with_session_then_poll_succeeds() {
    let oauth = store_with_cli_client().await;
    let issued = oauth
        .create_device_code(NewDeviceCode {
            client_id: "cli".to_string(),
            ttl: Duration::from_mins(10),
            interval: Duration::from_secs(5),
        })
        .await
        .unwrap();
    // Mint a browser session directly and present its cookie.
    let session = oauth.create_session(Duration::from_hours(1)).await.unwrap();
    let cookie = format!("gw_session={}", session.token);
    let app = build_app(oauth.clone()).await;

    // The logged-in user approves via the POST form.
    let approve = app
        .clone()
        .oneshot(
            Request::post("/oauth/device")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(COOKIE, &cookie)
                .body(Body::from(format!(
                    "user_code={}&action=approve",
                    issued.user_code
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(approve.status(), StatusCode::OK);

    // The CLI's next poll now succeeds.
    let (status, json) =
        body_json(post_form(app, "/oauth/token", poll_body(&issued.device_code)).await).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["access_token"].is_string());
}

#[tokio::test]
async fn device_page_renders_confirm_with_session() {
    let oauth = store_with_cli_client().await;
    let issued = oauth
        .create_device_code(NewDeviceCode {
            client_id: "cli".to_string(),
            ttl: Duration::from_mins(10),
            interval: Duration::from_secs(5),
        })
        .await
        .unwrap();
    let session = oauth.create_session(Duration::from_hours(1)).await.unwrap();
    let app = build_app(oauth).await;

    let resp = app
        .oneshot(
            Request::get(format!("/oauth/device?user_code={}", issued.user_code))
                .header(COOKIE, format!("gw_session={}", session.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains(&issued.user_code), "page shows the user code");
    assert!(html.contains("Approve this device"));
}

// ---------------------------------------------------------------------
// Storage layer
// ---------------------------------------------------------------------

#[tokio::test]
async fn consume_device_code_is_single_use_under_concurrency() {
    let oauth = store_with_cli_client().await;
    let issued = oauth
        .create_device_code(NewDeviceCode {
            client_id: "cli".to_string(),
            ttl: Duration::from_mins(10),
            interval: Duration::from_secs(5),
        })
        .await
        .unwrap();
    oauth
        .set_device_decision(&issued.user_code, true)
        .await
        .unwrap();

    let (a, b) = tokio::join!(
        oauth.consume_device_code(&issued.device_code),
        oauth.consume_device_code(&issued.device_code),
    );
    let approved = [a.unwrap(), b.unwrap()]
        .into_iter()
        .filter(|s| matches!(s, DevicePollState::Approved { .. }))
        .count();
    assert_eq!(approved, 1, "exactly one concurrent consume may mint tokens");
}

#[tokio::test]
async fn set_device_decision_is_idempotent_after_first() {
    let oauth = store_with_cli_client().await;
    let issued = oauth
        .create_device_code(NewDeviceCode {
            client_id: "cli".to_string(),
            ttl: Duration::from_mins(10),
            interval: Duration::from_secs(5),
        })
        .await
        .unwrap();
    assert!(
        oauth
            .set_device_decision(&issued.user_code, true)
            .await
            .unwrap(),
        "first decision is recorded"
    );
    assert!(
        !oauth
            .set_device_decision(&issued.user_code, false)
            .await
            .unwrap(),
        "a second decision is refused"
    );
}

#[tokio::test]
async fn set_device_decision_unknown_code_is_false() {
    let oauth = store_with_cli_client().await;
    assert!(
        !oauth
            .set_device_decision("ZZZZ-ZZZZ", true)
            .await
            .unwrap()
    );
}
