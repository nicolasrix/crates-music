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
            "dim": 512,
            "device": "cpu"
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let h = client.healthz().await.expect("healthz");
    assert!(h.reachable);
    assert!(h.model_loaded);
    assert_eq!(h.model_version.as_str(), "stub-v1");
    assert_eq!(h.dim, 512);
    assert_eq!(h.device.as_deref(), Some("cpu"));
}

#[tokio::test]
async fn healthz_reports_cuda_device() {
    // When the sidecar runs on ROCm-built torch, HIP devices identify
    // as "cuda" — that's the contract this assertion pins.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/healthz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "ok",
            "model_loaded": true,
            "model_version": "clap-v1",
            "dim": 512,
            "device": "cuda"
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let h = client.healthz().await.expect("healthz");
    assert_eq!(h.device.as_deref(), Some("cuda"));
}

#[tokio::test]
async fn healthz_missing_device_field_parses_as_none() {
    // Backwards compat: an older sidecar that doesn't yet emit `device`
    // should still produce a valid EmbedderHealth — gateway will log
    // "device=unknown" rather than crash-loop.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/healthz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "ok",
            "model_loaded": true,
            "model_version": "old-v0",
            "dim": 512
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let h = client.healthz().await.expect("healthz");
    assert!(h.reachable);
    assert_eq!(h.device, None);
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

// --- Server-Timing parser ------------------------------------------------
//
// The parser is exposed at the crate root so the gateway (and these
// tests) can verify it independently of the HTTP path. The record-on-
// span behavior is exercised in the layered test below.

mod server_timing_parser {
    use music_recommend::embedder::parse_server_timing;

    #[test]
    fn single_entry() {
        let out = parse_server_timing("decode;dur=42");
        assert_eq!(out, vec![("decode".into(), 42.0_f64)]);
    }

    #[test]
    fn multi_entry_with_whitespace() {
        let out = parse_server_timing("decode;dur=42, gpu_forward;dur=520");
        assert_eq!(
            out,
            vec![
                ("decode".into(), 42.0_f64),
                ("gpu_forward".into(), 520.0_f64),
            ]
        );
    }

    #[test]
    fn fractional_durations_round_trip() {
        let out = parse_server_timing("hash;dur=0.1, wrap;dur=1.8");
        assert_eq!(
            out,
            vec![("hash".into(), 0.1_f64), ("wrap".into(), 1.8_f64)]
        );
    }

    #[test]
    fn entries_without_dur_are_skipped() {
        // Per the RFC, `metric;desc="..."` (no dur) is a valid entry —
        // we just have no useful number to extract, so we drop it.
        let out = parse_server_timing("missing, decode;dur=42");
        assert_eq!(out, vec![("decode".into(), 42.0_f64)]);
    }

    #[test]
    fn extra_params_after_dur_are_ignored() {
        // `decode;dur=42;desc="audio decode"` — keep dur, drop desc.
        let out = parse_server_timing("decode;dur=42;desc=\"audio decode\"");
        assert_eq!(out, vec![("decode".into(), 42.0_f64)]);
    }

    #[test]
    fn empty_string_returns_empty_vec() {
        assert!(parse_server_timing("").is_empty());
        assert!(parse_server_timing("   ").is_empty());
    }

    #[test]
    fn malformed_entries_dont_panic() {
        // Each input is something a buggy backend might emit. None of
        // them should panic; we either extract what we can or drop.
        for header in [
            ";dur=10",          // empty name
            "decode;dur=",      // missing value
            "decode;dur=NaN",   // unparseable
            ",,, ,",            // pure separators
            "decode;dur=42;",   // trailing semicolon
        ] {
            let _ = parse_server_timing(header);
        }
    }
}

// --- record-on-span integration -----------------------------------------

#[tokio::test]
async fn embed_audio_works_when_server_timing_header_absent() {
    // Backwards compat: an embedder that doesn't emit Server-Timing
    // should still produce a valid embedding; the gateway just won't
    // get the stage breakdown. (The "field is actually written onto
    // the embed_audio span" path is too racy to test with parallel
    // `#[tokio::test]`s — `#[tracing::instrument]` creates the span at
    // call time using whatever dispatcher is active on the test
    // thread, and that's contested under parallel cargo test.
    // Instead, the parser is exhaustively unit-tested above and the
    // span-field plumbing is verified end-to-end by the M0 TraceLayer
    // round-trip tests in `crates/music-gateway/tests/diagnostics_layer.rs`.)
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "vector": [0.1, 0.2, 0.3],
            "dim": 3,
            "model_version": "stub-v1"
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = client
        .embed_audio(Bytes::from_static(b"audio"))
        .await
        .expect("embed");
    assert_eq!(out.vector.len(), 3);
}

#[tokio::test]
async fn embed_audio_succeeds_when_server_timing_header_is_present() {
    // The presence of Server-Timing must not change the embed result —
    // we extract it as a side effect for tracing only.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Server-Timing", "decode;dur=42, gpu_forward;dur=520")
                .set_body_json(json!({
                    "vector": [0.1, 0.2, 0.3],
                    "dim": 3,
                    "model_version": "stub-v1"
                })),
        )
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = client
        .embed_audio(Bytes::from_static(b"audio"))
        .await
        .expect("embed");
    assert_eq!(out.vector.len(), 3);
    assert_eq!(out.dim, 3);
}

// --- /reduce -----------------------------------------------------------------

use music_recommend::embedder::{ReduceParams, ReduceResult};

fn default_reduce_params() -> ReduceParams {
    ReduceParams {
        db_path: "/tmp/rec.sqlite".into(),
        model_version: "stub-v1".into(),
        proj_version: None,
        n_neighbors: 15,
        min_dist: 0.1,
        random_state: 42,
        n_components: 2,
    }
}

#[tokio::test]
async fn reduce_serializes_full_body_and_parses_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/reduce"))
        .and(body_json(json!({
            "db_path": "/tmp/rec.sqlite",
            "model_version": "stub-v1",
            "proj_version": "auto-1",
            "n_neighbors": 15,
            "min_dist": 0.1,
            "random_state": 42,
            "n_components": 2,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "proj_version": "auto-1",
            "written": 42,
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let mut params = default_reduce_params();
    params.proj_version = Some("auto-1".into());
    let out: ReduceResult = client.reduce(&params).await.expect("reduce");
    assert_eq!(out.proj_version, "auto-1");
    assert_eq!(out.written, 42);
}

#[tokio::test]
async fn reduce_omits_proj_version_when_none() {
    // When proj_version is None, the field must serialise as JSON null
    // (or be absent). The embedder's pydantic model accepts either —
    // null is the simpler wire shape and avoids "default missing" foot-
    // guns when both sides change at once.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/reduce"))
        .and(body_json(json!({
            "db_path": "/tmp/rec.sqlite",
            "model_version": "stub-v1",
            "proj_version": null,
            "n_neighbors": 15,
            "min_dist": 0.1,
            "random_state": 42,
            "n_components": 2,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "proj_version": "derived-from-defaults",
            "written": 0,
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let out = client.reduce(&default_reduce_params()).await.expect("reduce");
    assert_eq!(out.proj_version, "derived-from-defaults");
    assert_eq!(out.written, 0);
}

#[tokio::test]
async fn reduce_400_maps_to_server_error() {
    // The embedder returns 400 when the SQLite path doesn't exist;
    // surface that as a Server error so the gateway can log a clear
    // diagnostic and pause auto-trigger retries.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/reduce"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "detail": "db_path does not exist: /tmp/nope.sqlite",
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = client.reduce(&default_reduce_params()).await.unwrap_err();
    match err {
        EmbedderError::Server { status, body } => {
            assert_eq!(status, 400);
            assert!(body.contains("db_path"));
        }
        other => panic!("expected Server, got {other:?}"),
    }
}

#[tokio::test]
async fn reduce_503_maps_to_model_not_loaded() {
    // `reduce` returns 503 when umap-learn isn't installed — same
    // shape as embed_audio's 503, so the existing degraded-mode error
    // variant reuses cleanly.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/reduce"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "detail": "reduce extra not installed",
        })))
        .mount(&server)
        .await;

    let client = client_for(&server);
    let err = client.reduce(&default_reduce_params()).await.unwrap_err();
    assert!(matches!(err, EmbedderError::ModelNotLoaded), "got {err:?}");
}
