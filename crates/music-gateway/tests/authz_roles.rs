//! Route-level authorization (PR A of the user-system plan).
//!
//! `require_bearer` resolves a `Principal` and `require_admin` gates the
//! admin tier. These tests prove the matrix at the HTTP boundary: an admin
//! reaches an admin route, a non-admin User authenticates but is 403'd,
//! and an unauthenticated request is 401'd — using a real router and real
//! tokens minted for real `users` rows.

use std::time::Duration;

use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{Request, StatusCode};
use music_gateway::build_router;
use music_gateway::oauth::{NewClient, NewUser, OauthStore, SetupToken};
use tower::ServiceExt;

mod common;

/// An admin-tier route (diagnostics). Cheap GET, no body required.
const ADMIN_ROUTE: &str = "/v1/diagnostics/histogram";
/// An any-authenticated route.
const GENERAL_ROUTE: &str = "/v1/whoami";

async fn store_with_owner_and_client() -> OauthStore {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    // Seed the owner (id=1, admin) the way /oauth/setup would.
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

async fn get_with_bearer(oauth: OauthStore, route: &str, token: &str) -> StatusCode {
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );
    app.oneshot(
        Request::get(route)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

#[tokio::test]
async fn admin_token_reaches_admin_route() {
    let oauth = store_with_owner_and_client().await;
    // A token attributed to the owner (admin).
    let access = oauth
        .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(1))
        .await
        .unwrap();
    let status = get_with_bearer(oauth, ADMIN_ROUTE, &access.token).await;
    assert_ne!(status, StatusCode::FORBIDDEN, "admin must not be forbidden");
    assert_ne!(status, StatusCode::UNAUTHORIZED, "admin must be authenticated");
}

#[tokio::test]
async fn user_token_is_forbidden_on_admin_route() {
    let oauth = store_with_owner_and_client().await;
    let user_id = oauth
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
    let access = oauth
        .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(user_id))
        .await
        .unwrap();
    let status = get_with_bearer(oauth, ADMIN_ROUTE, &access.token).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "user must be 403 on admin tier");
}

#[tokio::test]
async fn user_token_reaches_general_route() {
    let oauth = store_with_owner_and_client().await;
    let user_id = oauth
        .insert_user(NewUser {
            username: Some("bob".to_string()),
            display_name: Some("Bob".to_string()),
            role: "user".to_string(),
            password_hash: Some("$argon2id$dummy".to_string()),
            host_user_id: None,
            expires_at_unix_ms: None,
        })
        .await
        .unwrap();
    let access = oauth
        .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(user_id))
        .await
        .unwrap();
    let status = get_with_bearer(oauth, GENERAL_ROUTE, &access.token).await;
    assert_eq!(status, StatusCode::OK, "user must reach any-auth tier");
}

#[tokio::test]
async fn unauthenticated_is_rejected_on_admin_route() {
    let oauth = store_with_owner_and_client().await;
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );
    let status = app
        .oneshot(Request::get(ADMIN_ROUTE).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn whoami_reports_role_and_identity() {
    let oauth = store_with_owner_and_client().await;
    let user_id = oauth
        .insert_user(NewUser {
            username: Some("carol".to_string()),
            display_name: Some("Carol".to_string()),
            role: "user".to_string(),
            password_hash: Some("$argon2id$dummy".to_string()),
            host_user_id: None,
            expires_at_unix_ms: None,
        })
        .await
        .unwrap();
    let access = oauth
        .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(user_id))
        .await
        .unwrap();
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );
    let resp = app
        .oneshot(
            Request::get(GENERAL_ROUTE)
                .header(AUTHORIZATION, format!("Bearer {}", access.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["role"], "user");
    assert_eq!(json["username"], "carol");
    assert_eq!(json["display_name"], "Carol");
    assert_eq!(json["user_id"], user_id);
}
