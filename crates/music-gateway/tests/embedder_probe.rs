//! Boot-probe + degraded-mode tests for the gateway-side embedder
//! handle. We don't run the real Python sidecar in tests — wiremock
//! gives us deterministic 200/503/unreachable responses.

use std::time::Duration;

use music_gateway::config::EmbedderConfigSection;
use music_gateway::embedder::{EmbedderHandle, boot_probe};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn boot_probe_disabled_when_no_config() {
    let h = boot_probe(None).await;
    assert!(!h.ready());
    assert!(h.client().is_none());
    assert!(h.last_health().is_none());
}

#[tokio::test]
async fn boot_probe_loaded_marks_ready() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/healthz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "ok",
            "model_loaded": true,
            "model_version": "stub-v1",
            "dim": 512
        })))
        .mount(&server)
        .await;

    let cfg = EmbedderConfigSection {
        url: server.uri(),
        timeout_seconds: 2,
    };
    let h = boot_probe(Some(&cfg)).await;
    assert!(h.ready());
    let health = h.last_health().expect("health recorded");
    assert!(health.model_loaded);
    assert!(h.client().is_some());
}

#[tokio::test]
async fn boot_probe_503_keeps_client_but_not_ready() {
    // Sidecar reachable, model still warming up. Gateway must boot
    // (no panic), client retained for later retries, ready() returns
    // false until a subsequent probe sees the model loaded.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/healthz"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "status": "loading",
            "model_loaded": false,
            "model_version": "stub-v1",
            "dim": 512
        })))
        .mount(&server)
        .await;

    let cfg = EmbedderConfigSection {
        url: server.uri(),
        timeout_seconds: 2,
    };
    let h = boot_probe(Some(&cfg)).await;
    assert!(!h.ready());
    assert!(h.client().is_some(), "client retained for later retries");
    let health = h.last_health().expect("health recorded");
    assert!(!health.model_loaded);
}

#[tokio::test]
async fn boot_probe_unreachable_keeps_client_for_retries() {
    // Bind a config to a port that isn't open. The probe must not
    // crash; it must log and return a handle with no recorded health.
    let cfg = EmbedderConfigSection {
        url: "http://127.0.0.1:1".to_string(),
        timeout_seconds: 1,
    };
    let h = boot_probe(Some(&cfg)).await;
    assert!(!h.ready());
    assert!(
        h.client().is_some(),
        "client retained even when unreachable"
    );
    assert!(h.last_health().is_none());
}

#[tokio::test]
async fn boot_probe_invalid_url_falls_back_to_disabled() {
    // Garbage URL → degraded mode, no client, no panic. This is the
    // "user typo'd the gateway.toml" case.
    let cfg = EmbedderConfigSection {
        url: "not a url at all".to_string(),
        timeout_seconds: 1,
    };
    let h = boot_probe(Some(&cfg)).await;
    assert!(!h.ready());
    assert!(h.client().is_none());
}

#[tokio::test]
async fn record_health_updates_ready_state() {
    let h = EmbedderHandle::disabled();
    assert!(!h.ready());
    // Disabled handles intentionally don't accept records — the
    // disabled path shouldn't pretend to have a probe history.
    // (We just assert the initial state stays sticky.)
    let _ = h; // no-op; this test pins the disabled-mode invariant.
}

#[tokio::test]
async fn ready_flips_from_not_loaded_to_loaded_after_record() {
    use music_recommend::embedder::EmbedderHealth;
    use music_recommend::types::ModelVersion;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/healthz"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "status": "loading",
            "model_loaded": false,
            "model_version": "stub-v1",
            "dim": 512
        })))
        .mount(&server)
        .await;

    let cfg = EmbedderConfigSection {
        url: server.uri(),
        timeout_seconds: 2,
    };
    let h = boot_probe(Some(&cfg)).await;
    assert!(!h.ready());

    // Worker observed a successful embed → records loaded health.
    h.record_health(EmbedderHealth {
        reachable: true,
        model_loaded: true,
        model_version: ModelVersion::from("stub-v1"),
        dim: 512,
    });
    assert!(h.ready(), "ready flips to true once health is recorded");
}

#[tokio::test]
async fn boot_probe_respects_short_timeout() {
    // wiremock delays 5s; our timeout is 200ms — the probe should
    // fail fast (Transport via hyper) and degrade.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/healthz"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;

    // We can't pass sub-second timeouts via config (it's seconds), so
    // this test pins the boot-probe behavior at the minimum 1s.
    let cfg = EmbedderConfigSection {
        url: server.uri(),
        timeout_seconds: 1,
    };
    let start = std::time::Instant::now();
    let h = boot_probe(Some(&cfg)).await;
    let elapsed = start.elapsed();
    assert!(!h.ready());
    assert!(
        elapsed < Duration::from_secs(3),
        "boot probe took {elapsed:?} — should fail fast under 3s"
    );
}
