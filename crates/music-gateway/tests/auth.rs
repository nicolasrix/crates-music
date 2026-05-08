//! Bearer-token middleware on protected routes.

use axum::body::Body;
use axum::http::{Request, StatusCode, header::AUTHORIZATION};
use music_gateway::build_router;
use tower::ServiceExt;

mod common;

fn protected_request(uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut req = Request::builder().uri(uri);
    if let Some(token) = bearer {
        req = req.header(AUTHORIZATION, format!("Bearer {token}"));
    }
    req.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn whoami_without_bearer_is_401() {
    let app = build_router(common::build_state(common::test_config()).await);
    let response = app
        .oneshot(protected_request("/v1/whoami", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn whoami_with_wrong_bearer_is_401() {
    let app = build_router(common::build_state(common::test_config()).await);
    let response = app
        .oneshot(protected_request("/v1/whoami", Some("not-the-token")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn whoami_with_correct_bearer_is_200() {
    let app = build_router(common::build_state(common::test_config()).await);
    let response = app
        .oneshot(protected_request("/v1/whoami", Some(common::TEST_BEARER)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn malformed_authorization_header_is_401() {
    // No "Bearer " prefix.
    let app = build_router(common::build_state(common::test_config()).await);
    let req = Request::builder()
        .uri("/v1/whoami")
        .header(AUTHORIZATION, common::TEST_BEARER)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn access_token_query_param_is_accepted_for_media_urls() {
    // Browser <audio> can't set headers — the access_token query param
    // is the standard fallback (RFC 6750 §2.3).
    let app = build_router(common::build_state(common::test_config()).await);
    let req = Request::builder()
        .uri(format!("/v1/whoami?access_token={}", common::TEST_BEARER))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn wrong_access_token_query_param_is_401() {
    let app = build_router(common::build_state(common::test_config()).await);
    let req = Request::builder()
        .uri("/v1/whoami?access_token=bad")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rest_subtree_is_also_protected() {
    let app = build_router(common::build_state(common::test_config()).await);
    let response = app
        .oneshot(protected_request("/rest/ping", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
