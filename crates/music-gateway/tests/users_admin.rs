//! Admin user provisioning + multi-user login (PR B of the user-system
//! plan). Two concerns:
//!   1. `/v1/admin/users` CRUD is admin-gated and behaves (create / list /
//!      delete / reset-password, with the owner undeletable and duplicate
//!      usernames 409'd).
//!   2. A provisioned account can log in by username and the resulting
//!      access token resolves to *that* user — proving `user_id` threads
//!      the whole login → session → auth-code → token → whoami chain.

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_gateway::build_router;
use music_gateway::oauth::{NewClient, NewUser, OauthStore, SetupToken};
use serde_json::Value;
use tower::ServiceExt;

mod common;

// RFC 7636 §4.6 example PKCE pair.
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const REDIRECT_URI: &str = "http://localhost:5173/cb";
const ALICE_PASSWORD: &str = "alice-password-1234";

/// Owner (id=1, admin) + a registered `web` client.
async fn store_with_owner_and_client() -> OauthStore {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    oauth
        .set_master_password_hash("$argon2id$dummy")
        .await
        .unwrap();
    oauth
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web".to_string(),
            redirect_uris: vec![REDIRECT_URI.to_string()],
        })
        .await
        .unwrap();
    oauth
}

async fn app_for(oauth: OauthStore) -> Router {
    build_router(common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await)
}

/// An access token attributed to `user_id`.
async fn token_for(oauth: &OauthStore, user_id: i64) -> String {
    oauth
        .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(user_id))
        .await
        .unwrap()
        .token
}

async fn post_json(app: &Router, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::post(path)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

// ---------------------------------------------------------------------
// Admin CRUD
// ---------------------------------------------------------------------

#[tokio::test]
async fn admin_can_create_list_and_delete_a_user() {
    let oauth = store_with_owner_and_client().await;
    let admin = token_for(&oauth, 1).await;
    let app = app_for(oauth).await;

    // Create.
    let (status, body) = post_json(
        &app,
        "/v1/admin/users",
        &admin,
        serde_json::json!({ "username": "alice", "password": ALICE_PASSWORD, "role": "user" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create: {body}");
    let new_id = body["id"].as_i64().expect("create returns the new id");
    assert!(new_id > 1);

    // List includes the owner + alice.
    let resp = app
        .clone()
        .oneshot(
            Request::get("/v1/admin/users")
                .header(AUTHORIZATION, format!("Bearer {admin}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let list: Value = serde_json::from_slice(&bytes).unwrap();
    let users = list["users"].as_array().unwrap();
    assert!(users.iter().any(|u| u["username"] == "owner"));
    assert!(
        users.iter().any(|u| u["username"] == "alice" && u["role"] == "user"),
        "alice must appear with role=user: {list}"
    );
    // No credential material leaks.
    assert!(users.iter().all(|u| u.get("password_hash").is_none()));

    // Delete.
    let resp = app
        .clone()
        .oneshot(
            Request::delete(format!("/v1/admin/users/{new_id}"))
                .header(AUTHORIZATION, format!("Bearer {admin}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn non_admin_cannot_provision_users() {
    let oauth = store_with_owner_and_client().await;
    let user_id = oauth
        .insert_user(NewUser {
            username: Some("bob".to_string()),
            display_name: None,
            role: "user".to_string(),
            password_hash: Some("$argon2id$dummy".to_string()),
            host_user_id: None,
            expires_at_unix_ms: None,
        })
        .await
        .unwrap();
    let user = token_for(&oauth, user_id).await;
    let app = app_for(oauth).await;

    let (status, _) = post_json(
        &app,
        "/v1/admin/users",
        &user,
        serde_json::json!({ "username": "mallory", "password": ALICE_PASSWORD, "role": "user" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a non-admin must be 403 on provisioning");
}

#[tokio::test]
async fn cannot_delete_the_owner() {
    let oauth = store_with_owner_and_client().await;
    let admin = token_for(&oauth, 1).await;
    let app = app_for(oauth).await;

    let resp = app
        .oneshot(
            Request::delete("/v1/admin/users/1")
                .header(AUTHORIZATION, format!("Bearer {admin}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "the owner is undeletable");
}

#[tokio::test]
async fn cannot_reset_the_owner_password() {
    // The owner's credential is only resettable via the offline CLI
    // `reset-master-password` path. Allowing it over HTTP would let ANY
    // admin (id != 1) `POST /v1/admin/users/1/password` and take over the
    // owner account — so the guard is unconditional, mirroring delete.
    let oauth = store_with_owner_and_client().await;
    let admin = token_for(&oauth, 1).await;
    let app = app_for(oauth).await;

    let (status, _) = post_json(
        &app,
        "/v1/admin/users/1/password",
        &admin,
        serde_json::json!({ "password": ALICE_PASSWORD }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the owner password is not resettable over HTTP"
    );
}

#[tokio::test]
async fn duplicate_username_is_409() {
    let oauth = store_with_owner_and_client().await;
    let admin = token_for(&oauth, 1).await;
    let app = app_for(oauth).await;

    let body = serde_json::json!({ "username": "dup", "password": ALICE_PASSWORD, "role": "user" });
    let (s1, _) = post_json(&app, "/v1/admin/users", &admin, body.clone()).await;
    assert_eq!(s1, StatusCode::CREATED);
    let (s2, j2) = post_json(&app, "/v1/admin/users", &admin, body).await;
    assert_eq!(s2, StatusCode::CONFLICT);
    assert_eq!(j2["error"], "username_taken");
}

#[tokio::test]
async fn create_rejects_guest_role_and_short_password() {
    let oauth = store_with_owner_and_client().await;
    let admin = token_for(&oauth, 1).await;
    let app = app_for(oauth).await;

    let (guest, _) = post_json(
        &app,
        "/v1/admin/users",
        &admin,
        serde_json::json!({ "username": "g", "password": ALICE_PASSWORD, "role": "guest" }),
    )
    .await;
    assert_eq!(guest, StatusCode::BAD_REQUEST, "guest role is not provisionable here");

    let (short, _) = post_json(
        &app,
        "/v1/admin/users",
        &admin,
        serde_json::json!({ "username": "h", "password": "short", "role": "user" }),
    )
    .await;
    assert_eq!(short, StatusCode::BAD_REQUEST, "password below the minimum is rejected");
}

// ---------------------------------------------------------------------
// Multi-user login → token → whoami (the user_id threading proof)
// ---------------------------------------------------------------------

#[tokio::test]
async fn provisioned_user_logs_in_and_token_resolves_to_them() {
    let oauth = store_with_owner_and_client().await;
    let admin = token_for(&oauth, 1).await;
    let app = app_for(oauth).await;

    // Provision alice through the admin endpoint (so the password is hashed
    // exactly as production would).
    let (status, body) = post_json(
        &app,
        "/v1/admin/users",
        &admin,
        serde_json::json!({ "username": "alice", "display_name": "Alice", "password": ALICE_PASSWORD, "role": "user" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let alice_id = body["id"].as_i64().unwrap();

    // 1. Login as alice → session cookie bound to her user_id.
    let resp = app
        .clone()
        .oneshot(
            Request::post("/oauth/login")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("username=alice&password={ALICE_PASSWORD}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "alice must be able to log in");
    let cookie = resp
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .find_map(|v| {
            let s = v.to_str().ok()?;
            s.starts_with("gw_session=").then(|| s.split(';').next().unwrap().to_string())
        })
        .expect("login sets a session cookie");

    // 2. Authorize with the cookie → redirect carrying the auth code.
    let authorize_uri = format!(
        "/oauth/authorize?response_type=code&client_id=web&redirect_uri={}&code_challenge={PKCE_CHALLENGE}&code_challenge_method=S256",
        urlencoding(REDIRECT_URI),
    );
    let resp = app
        .clone()
        .oneshot(
            Request::get(&authorize_uri)
                .header(COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "authorize must redirect back to the client");
    let location = resp.headers().get(LOCATION).unwrap().to_str().unwrap();
    let code = url::Url::parse(location)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .expect("authorize redirect carries a code");

    // 3. Exchange the code for tokens.
    let token_body = format!(
        "grant_type=authorization_code&code={code}&client_id=web&redirect_uri={}&code_verifier={PKCE_VERIFIER}",
        urlencoding(REDIRECT_URI),
    );
    let resp = app
        .clone()
        .oneshot(
            Request::post("/oauth/token")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(token_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "token exchange must succeed");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let tokens: Value = serde_json::from_slice(&bytes).unwrap();
    let access = tokens["access_token"].as_str().unwrap().to_string();

    // 4. whoami resolves the token to alice — not the owner.
    let resp = app
        .oneshot(
            Request::get("/v1/whoami")
                .header(AUTHORIZATION, format!("Bearer {access}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let me: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(me["user_id"], alice_id, "token must resolve to alice's id");
    assert_eq!(me["role"], "user");
    assert_eq!(me["username"], "alice");
    assert_eq!(me["display_name"], "Alice");
}

/// Minimal percent-encoder for the redirect_uri in query strings.
fn urlencoding(s: &str) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => write!(&mut out, "%{b:02X}").unwrap(),
        }
    }
    out
}
