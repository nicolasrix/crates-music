//! Tests for the embedder HTTP client.
//!
//! Driven by `wiremock` so we can pin every wire-shape contract: the
//! exact request bytes, the JSON envelope, the 503 → degraded-mode
//! signal, transport errors. None of these tests touch the real
//! Python sidecar.

use std::time::Duration;

use bytes::Bytes;
use music_recommend::embedder::{EmbedderClient, EmbedderConfig, EmbedderError};
use serde_json::json;
use wiremock::matchers::{body_bytes, body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client_for(server: &MockServer) -> EmbedderClient {
    EmbedderClient::new(EmbedderConfig {
        url: server.uri().parse().expect("server uri"),
        timeout: Duration::from_secs(2),
    })
    .expect("client builds")
}

#[tokio::test]
async fn healthz_loaded_returns_health() {
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

    let client = client_for(&server);
    let h = client.healthz().await.expect("healthz");
    assert!(h.reachable);
    assert!(h.model_loaded);
    assert_eq!(h.model_version.as_str(), "stub-v1");
    assert_eq!(h.dim, 512);
}

#[tokio::test]
async fn healthz_503_reports_not_loaded_but_reachable() {
    // The sidecar returns 503 with a JSON body when the model is still
    // warming up. From the gateway's POV that's "reachable but not
    // yet usable" — it should not be confused with a transport error.
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

    let client = client_for(&server);
    let h = client.healthz().await.expect("healthz");
    assert!(h.reachable);
    assert!(!h.model_loaded);
}

#[tokio::test]
async fn healthz_unreachable_returns_transport_error() {
    // Bind a fresh client to a port we know isn't open. The right
    // failure mode here is `Transport`, not `Server` — degraded-mode
    // logic depends on this distinction.
    let client = EmbedderClient::new(EmbedderConfig {
        url: "http://127.0.0.1:1".parse().unwrap(),
        timeout: Duration::from_millis(200),
    })
    .unwrap();
    let err = client.healthz().await.unwrap_err();
    assert!(matches!(err, EmbedderError::Transport(_)), "got {err:?}");
}

#[tokio::test]
async fn embed_audio_sends_raw_bytes_and_returns_vector() {
    let server = MockServer::start().await;
    let payload = Bytes::from_static(b"\x01\x02\x03audio");
    let payload_clone = payload.clone();
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .and(header("content-type", "application/octet-stream"))
        .and(body_bytes(payload_clone.to_vec()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "vector": [0.1, 0.2, 0.3],
            "dim": 3,
            "model_version": "stub-v1"
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = client.embed_audio(payload).await.expect("embed");
    assert_eq!(out.vector, vec![0.1_f32, 0.2, 0.3]);
    assert_eq!(out.model_version.as_str(), "stub-v1");
    assert_eq!(out.dim, 3);
}

#[tokio::test]
async fn embed_audio_503_maps_to_model_not_loaded() {
    // Distinct error variant from a generic 5xx so callers can decide:
    // 503 means "retry later, sidecar is alive"; 500 means "this is broken."
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "detail": "model not loaded"
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = client
        .embed_audio(Bytes::from_static(b"x"))
        .await
        .unwrap_err();
    assert!(matches!(err, EmbedderError::ModelNotLoaded), "got {err:?}");
}

#[tokio::test]
async fn embed_audio_500_maps_to_server_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = client
        .embed_audio(Bytes::from_static(b"x"))
        .await
        .unwrap_err();
    match err {
        EmbedderError::Server { status, .. } => assert_eq!(status, 500),
        other => panic!("expected Server, got {other:?}"),
    }
}

#[tokio::test]
async fn embed_text_sends_json_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/text"))
        .and(body_json(json!({"text": "rainy sunday"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "vector": [1.0, 0.0],
            "dim": 2,
            "model_version": "stub-v1"
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = client.embed_text("rainy sunday").await.expect("embed");
    assert_eq!(out.vector, vec![1.0_f32, 0.0]);
}

#[tokio::test]
async fn embed_text_503_maps_to_model_not_loaded() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/text"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "detail": "model not loaded"
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = client.embed_text("anything").await.unwrap_err();
    assert!(matches!(err, EmbedderError::ModelNotLoaded));
}

#[tokio::test]
async fn timeout_short_circuits() {
    // wiremock's delay > client timeout exercises the timeout path
    // without flakiness.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/healthz"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
        .mount(&server)
        .await;

    let client = EmbedderClient::new(EmbedderConfig {
        url: server.uri().parse().unwrap(),
        timeout: Duration::from_millis(100),
    })
    .unwrap();
    let err = client.healthz().await.unwrap_err();
    assert!(matches!(err, EmbedderError::Transport(_)), "got {err:?}");
}

#[tokio::test]
async fn invalid_json_response_is_invalid_response_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/healthz"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = client.healthz().await.unwrap_err();
    assert!(
        matches!(err, EmbedderError::InvalidResponse(_)),
        "got {err:?}"
    );
}

#[tokio::test]
async fn embed_audio_handles_empty_vector_response() {
    // Server happy-path with dim=0 is a degenerate response we treat
    // as an InvalidResponse — embeddings with no dims are useless.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "vector": [],
            "dim": 0,
            "model_version": "stub-v1"
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = client
        .embed_audio(Bytes::from_static(b"x"))
        .await
        .unwrap_err();
    assert!(matches!(err, EmbedderError::InvalidResponse(_)));
}

#[tokio::test]
async fn dim_mismatch_in_response_is_invalid_response() {
    // Server says dim=512 but only sends 3 floats. Defensive check.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "vector": [0.1, 0.2, 0.3],
            "dim": 512,
            "model_version": "stub-v1"
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = client
        .embed_audio(Bytes::from_static(b"x"))
        .await
        .unwrap_err();
    assert!(matches!(err, EmbedderError::InvalidResponse(_)));
}
