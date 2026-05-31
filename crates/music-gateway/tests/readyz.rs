//! `/readyz` — strict readiness probe.
//!
//! Distinct from `/healthz`. `/healthz` is liveness ("the process is up,
//! restart-or-not?") and answers 200 unconditionally. `/readyz` is the
//! one Docker `HEALTHCHECK` and `depends_on: service_healthy` consume:
//! it returns 503 the moment a dependency the gateway *opted into* is
//! unreachable, so the container reports `unhealthy` instead of lying.

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_gateway::build_router;
use music_gateway::config::EmbedderConfigSection;
use music_gateway::embedder::boot_probe;
use serde_json::Value;
use tower::ServiceExt;
use wiremock::matchers::{method as m_method, path as m_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

async fn readyz(app: axum::Router) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn mount_navidrome_ping_ok(server: &MockServer) {
    Mock::given(m_method("GET"))
        .and(m_path("/rest/ping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "subsonic-response": { "status": "ok", "version": "1.16.1" }
        })))
        .mount(server)
        .await;
}

async fn mount_embedder_loaded(server: &MockServer) {
    Mock::given(m_method("GET"))
        .and(m_path("/healthz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "model_loaded": true,
            "model_version": "stub-v1",
            "dim": 512
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn readyz_does_not_require_auth() {
    // No bearer header attached — the route must be public like /healthz.
    let upstream = MockServer::start().await;
    mount_navidrome_ping_ok(&upstream).await;
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state(cfg).await);
    let (status, _) = readyz(app).await;
    assert_ne!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn readyz_ok_when_navidrome_reachable_and_no_embedder() {
    let upstream = MockServer::start().await;
    mount_navidrome_ping_ok(&upstream).await;
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state(cfg).await);
    let (status, body) = readyz(app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ready");
    assert_eq!(body["checks"]["navidrome"]["status"], "ok");
    // No [embedder] block configured ⇒ "disabled" is acceptable, not a failure.
    assert_eq!(body["checks"]["embedder"]["status"], "disabled");
}

#[tokio::test]
async fn readyz_ok_when_navidrome_and_embedder_both_ready() {
    let upstream = MockServer::start().await;
    mount_navidrome_ping_ok(&upstream).await;
    let embedder_server = MockServer::start().await;
    mount_embedder_loaded(&embedder_server).await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let embedder_cfg = EmbedderConfigSection {
        url: embedder_server.uri(),
        timeout_seconds: 2,
        bearer_token: None,
    };
    let handle = boot_probe(Some(&embedder_cfg)).await;
    assert!(handle.ready(), "embedder boot probe should mark ready");

    let app = build_router(common::build_state_with_embedder(cfg, handle).await);
    let (status, body) = readyz(app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["checks"]["embedder"]["status"], "ok");
}

#[tokio::test]
async fn readyz_503_when_navidrome_unreachable() {
    // Point at a closed port — connection refused, no MockServer mounted.
    let cfg = common::test_config_with_upstream("http://127.0.0.1:1", "alice", "sesame");
    let app = build_router(common::build_state(cfg).await);
    let (status, body) = readyz(app).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "degraded");
    assert_eq!(body["checks"]["navidrome"]["status"], "error");
    assert!(
        body["checks"]["navidrome"]["error"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "navidrome failure must surface an error message, got {}",
        body["checks"]["navidrome"]
    );
}

#[tokio::test]
async fn readyz_503_when_navidrome_returns_5xx() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/ping"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&upstream)
        .await;
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state(cfg).await);
    let (status, body) = readyz(app).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["checks"]["navidrome"]["status"], "error");
}

#[tokio::test]
async fn readyz_503_when_embedder_configured_but_not_loaded() {
    // Operator opted into the embedder by adding it to config. If it's
    // reachable but the model never loaded, requests will fail with
    // degraded behaviour for the whole recommend surface — that's not
    // "ready". Embedder-disabled is fine; embedder-configured-but-down
    // is a 503.
    let upstream = MockServer::start().await;
    mount_navidrome_ping_ok(&upstream).await;
    let embedder_server = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/healthz"))
        .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
            "status": "loading",
            "model_loaded": false,
            "model_version": "stub-v1",
            "dim": 512
        })))
        .mount(&embedder_server)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let embedder_cfg = EmbedderConfigSection {
        url: embedder_server.uri(),
        timeout_seconds: 2,
        bearer_token: None,
    };
    let handle = boot_probe(Some(&embedder_cfg)).await;
    assert!(!handle.ready());

    let app = build_router(common::build_state_with_embedder(cfg, handle).await);
    let (status, body) = readyz(app).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["checks"]["embedder"]["status"], "error");
}

#[tokio::test]
async fn readyz_503_when_embedder_configured_but_unreachable() {
    let upstream = MockServer::start().await;
    mount_navidrome_ping_ok(&upstream).await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let embedder_cfg = EmbedderConfigSection {
        url: "http://127.0.0.1:1".to_string(),
        timeout_seconds: 1,
        bearer_token: None,
    };
    let handle = boot_probe(Some(&embedder_cfg)).await;
    assert!(!handle.ready());

    let app = build_router(common::build_state_with_embedder(cfg, handle).await);
    let (status, _body) = readyz(app).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn readyz_fails_fast_when_navidrome_hangs() {
    // wiremock delays response 5s; the handler must short-circuit well
    // before that. Docker hits the HEALTHCHECK every 30s — a slow probe
    // blocks the healthchecker, not us, but a slow probe is still a bug:
    // the operator wants the unhealthy signal *now*, not after 30s.
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/ping"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&upstream)
        .await;
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state(cfg).await);
    let start = Instant::now();
    let (status, _body) = readyz(app).await;
    let elapsed = start.elapsed();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        elapsed < Duration::from_secs(4),
        "readyz hung for {elapsed:?} — must fail fast under ~3s"
    );
}
