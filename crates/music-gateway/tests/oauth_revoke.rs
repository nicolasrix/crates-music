//! POST /oauth/revoke (RFC 7009) + bearer-token middleware acceptance
//! of OAuth-issued access tokens.

use std::time::Duration;

use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{Request, StatusCode};
use music_gateway::build_router;
use music_gateway::oauth::{NewClient, NewRefreshToken, OauthStore, SetupToken};
use tower::ServiceExt;

mod common;

async fn store_with_client() -> OauthStore {
    let oauth = OauthStore::open_in_memory().await.unwrap();
    // Tokens now FK-reference users(id); seed the owner (id=1).
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

// ---------------------------------------------------------------------
// /oauth/revoke
// ---------------------------------------------------------------------

#[tokio::test]
async fn revoke_endpoint_revokes_refresh_token_and_cascades_to_access() {
    let oauth = store_with_client().await;
    let refresh = oauth
        .mint_refresh_token(NewRefreshToken {
            user_id: 1,
            client_id: "web".to_string(),
            ttl: None,
        })
        .await
        .unwrap();
    let access = oauth
        .mint_access_token("web", Some(&refresh.token_hash), Duration::from_hours(1))
        .await
        .unwrap();
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth.clone(), SetupToken::none())
            .await,
    );

    let resp = app
        .oneshot(
            Request::post("/oauth/revoke")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("token={}", refresh.token)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    assert!(
        oauth
            .find_refresh_token(&refresh.token)
            .await
            .unwrap()
            .is_none(),
        "refresh token must be revoked"
    );
    assert!(
        oauth
            .find_access_token(&access.token)
            .await
            .unwrap()
            .is_none(),
        "associated access token must be revoked too"
    );
}

#[tokio::test]
async fn revoke_endpoint_returns_200_for_unknown_token() {
    // RFC 7009: the server MUST respond 200 for unknown tokens to avoid
    // leaking which tokens exist.
    let oauth = store_with_client().await;
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );
    let resp = app
        .oneshot(
            Request::post("/oauth/revoke")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("token=ghost"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn revoke_endpoint_revokes_a_standalone_access_token() {
    // Access tokens not derived from a refresh (legacy/test) can still
    // be revoked individually.
    let oauth = store_with_client().await;
    let access = oauth
        .mint_access_token("web", None, Duration::from_hours(1))
        .await
        .unwrap();
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth.clone(), SetupToken::none())
            .await,
    );

    let resp = app
        .oneshot(
            Request::post("/oauth/revoke")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("token={}", access.token)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        oauth
            .find_access_token(&access.token)
            .await
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------------
// Middleware now also accepts OAuth-issued access tokens.
// ---------------------------------------------------------------------

#[tokio::test]
async fn middleware_accepts_oauth_access_token() {
    let oauth = store_with_client().await;
    let access = oauth
        .mint_access_token("web", None, Duration::from_hours(1))
        .await
        .unwrap();
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );
    let resp = app
        .oneshot(
            Request::get("/v1/whoami")
                .header(AUTHORIZATION, format!("Bearer {}", access.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn middleware_still_accepts_static_bearer_for_legacy_clients() {
    // The CLI still uses the static bearer through P3; OAuth migration
    // happens later. Belt + braces: regression test.
    let oauth = store_with_client().await;
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );
    let resp = app
        .oneshot(
            Request::get("/v1/whoami")
                .header(AUTHORIZATION, format!("Bearer {}", common::TEST_BEARER))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn middleware_rejects_revoked_access_token() {
    let oauth = store_with_client().await;
    let access = oauth
        .mint_access_token("web", None, Duration::from_hours(1))
        .await
        .unwrap();
    oauth.revoke_access_token(&access.token).await.unwrap();
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );
    let resp = app
        .oneshot(
            Request::get("/v1/whoami")
                .header(AUTHORIZATION, format!("Bearer {}", access.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn middleware_rejects_unknown_bearer() {
    let oauth = store_with_client().await;
    let app = build_router(
        common::build_state_with_oauth(common::test_config(), oauth, SetupToken::none()).await,
    );
    let resp = app
        .oneshot(
            Request::get("/v1/whoami")
                .header(AUTHORIZATION, "Bearer not-a-token-anywhere")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
