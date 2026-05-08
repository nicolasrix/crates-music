//! /healthz is unauthenticated and returns a small JSON status body.
//! Drives the router directly via `tower::ServiceExt::oneshot` — no port bound.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_gateway::build_router;
use tower::ServiceExt;

mod common;

#[tokio::test]
async fn healthz_returns_200_without_auth() {
    let app = build_router(common::build_state(common::test_config()).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn healthz_body_reports_ok_status_and_service_name() {
    let app = build_router(common::build_state(common::test_config()).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["service"], "music-gateway");
    assert!(json["version"].is_string());
}

/// Regression: the protected sub-router has a `require_bearer` layer.
/// Without an explicit fallback on the merged router, that layer wraps the
/// fallback and returns 401 for ALL unmatched paths — which conflates
/// "this route doesn't exist" with "your token is invalid". We hit a
/// nonsense path with no auth and expect 404.
#[tokio::test]
async fn unmatched_route_returns_404_not_401() {
    let app = build_router(common::build_state(common::test_config()).await);
    for path in ["/totally-unknown", "/oauth/callback", "/v1/", "/rest"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "unmatched path {path} must return 404, got {}",
            resp.status()
        );
    }
}
