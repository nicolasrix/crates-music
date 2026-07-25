//! Subsonic /rest/* pass-through proxy.
//!
//! Verifies the gateway:
//!   - injects upstream Subsonic auth params (u/t/s/v/c/f),
//!   - strips client-supplied auth params (defence: clients never set them),
//!   - forwards remaining query params verbatim,
//!   - propagates upstream status & body,
//!   - surfaces upstream connection failures as 502.

use axum::body::Body;
use axum::http::{Request, StatusCode, header::AUTHORIZATION};
use http_body_util::BodyExt;
use music_gateway::build_router;
use tower::ServiceExt;
use wiremock::matchers::{method as m_method, path as m_path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

fn auth_header() -> (&'static str, String) {
    (
        AUTHORIZATION.as_str(),
        format!("Bearer {}", common::TEST_BEARER),
    )
}

#[tokio::test]
async fn proxy_forwards_ping_with_injected_auth_params() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/ping"))
        .and(query_param("u", "alice"))
        .and(query_param("v", "1.16.1"))
        .and(query_param("f", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "subsonic-response": { "status": "ok", "version": "1.16.1" }
        })))
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state(cfg).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rest/ping")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn proxy_strips_client_supplied_auth_params() {
    // The client tries to override u/p/t/s — the gateway must ignore them
    // and use its own configured Navidrome credentials.
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/ping"))
        .and(query_param("u", "alice"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "subsonic-response": { "status": "ok", "version": "1.16.1" }
        })))
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state(cfg).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rest/ping?u=evil&p=plaintext&t=spoofed&s=spoofed")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn proxy_forwards_non_auth_query_params_unchanged() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .and(query_param("type", "newest"))
        .and(query_param("size", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "subsonic-response": {
                "status": "ok",
                "version": "1.16.1",
                "albumList2": { "album": [] }
            }
        })))
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state(cfg).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rest/getAlbumList2?type=newest&size=20")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn proxy_preserves_upstream_body_bytes() {
    let upstream = MockServer::start().await;
    let payload = serde_json::json!({
        "subsonic-response": {
            "status": "failed",
            "version": "1.16.1",
            "error": { "code": 70, "message": "Album not found." }
        }
    });
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbum"))
        .respond_with(ResponseTemplate::new(200).set_body_json(payload.clone()))
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state(cfg).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rest/getAlbum?id=al-missing")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let received: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(received, payload);
}

#[tokio::test]
async fn proxy_returns_502_when_upstream_unreachable() {
    // Point at an address nothing's listening on. reqwest should fail to connect.
    let cfg = common::test_config_with_upstream(
        "http://127.0.0.1:1", // RFC 6335: well-known port, nothing here
        "alice",
        "sesame",
    );
    let app = build_router(common::build_state(cfg).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rest/ping")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn proxy_propagates_upstream_5xx() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/ping"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state(cfg).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rest/ping")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // We forward upstream's status — 503 in, 503 out.
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn proxy_rejects_subsonic_write_methods() {
    // The /rest proxy is read-only: mutating Subsonic methods (star,
    // setRating, createPlaylist, createUser, …) are gateway-owned via
    // /v1/* or would expose Navidrome-admin operations under the shared
    // credential. They must be 403'd *before* any upstream call — proven
    // by the `expect(0)` catch-all below (any proxied request would 200).
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "subsonic-response": { "status": "ok", "version": "1.16.1" }
        })))
        .expect(0)
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state(cfg).await);

    // Even the owner/admin static bearer is blocked — read-only is
    // universal, so a guest (strictly less capable) is denied a fortiori.
    for method in ["star", "setRating", "createPlaylist", "deleteUser", "star.view", "STAR"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/rest/{method}?id=al-1"))
                    .header(auth_header().0, auth_header().1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{method} must be blocked read-only"
        );
    }
}

#[tokio::test]
async fn proxy_does_not_follow_upstream_redirects() {
    // A redirect from the upstream must NOT be chased by the gateway's
    // HTTP client: the request carries the gateway's Navidrome
    // credentials in its query string, and following a 3xx would replay
    // them to the (attacker-chosen) Location. We forward the 3xx to the
    // client instead.
    let upstream = MockServer::start().await;
    // `/rest/ping` answers with a redirect to `/leaked`.
    Mock::given(m_method("GET"))
        .and(m_path("/rest/ping"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "/leaked"),
        )
        .mount(&upstream)
        .await;
    // The redirect target must never be hit. `expect(0)` is verified when
    // the MockServer is dropped at end of test.
    Mock::given(m_method("GET"))
        .and(m_path("/leaked"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state(cfg).await);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/rest/ping")
                .header(auth_header().0, auth_header().1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // The 302 is forwarded verbatim — not followed.
    assert_eq!(response.status(), StatusCode::FOUND);
}
