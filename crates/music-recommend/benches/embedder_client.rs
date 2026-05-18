//! Microbenchmarks for `EmbedderClient` against a wiremock fake.
//!
//! What this measures: HTTP roundtrip via reqwest + JSON deserialize +
//! `Server-Timing` extraction + `Span::current().record(...)`. It does
//! NOT measure inference (the wiremock returns a canned vector
//! instantly) — model timings live in `services/embedder/tests/test_benchmarks_clap.py`.
//!
//! The signal here is "did our client-side plumbing regress?" — for
//! example, a reqwest config change adding TLS overhead, a JSON parse
//! that allocates more, or a Server-Timing header parse that suddenly
//! starts copying strings. Useful but narrow.
//!
//! Setup (MockServer + canned response) lives outside `b.iter_*` so
//! every iteration measures the same hot path. Spinning up a server
//! per iteration would dominate (millisecond-scale) the actual call
//! cost (sub-millisecond on localhost).

use std::time::Duration;

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use serde_json::json;
use tokio::runtime::Runtime;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use music_recommend::embedder::{EmbedderClient, EmbedderConfig};

const DIM: usize = 512;

fn canned_vector() -> Vec<f32> {
    // Deterministic, unit-norm-ish so it survives any future client-
    // side validation we add (currently just dim/length checks).
    // i and DIM are bounded to 512, well within f32's 23-bit mantissa,
    // but the cast is explicit (`u16` → `f32` is lossless via `From`)
    // to keep clippy::pedantic quiet without an `#[allow]`.
    let dim_f32 = f32::from(u16::try_from(DIM).expect("DIM fits in u16"));
    (0..DIM)
        .map(|i| {
            let i_f32 = f32::from(u16::try_from(i).expect("index fits in u16"));
            i_f32 / dim_f32 - 0.5
        })
        .collect()
}

/// Spin up a wiremock server that handles both /embed/audio and
/// /embed/text with a stable JSON envelope and a realistic
/// Server-Timing header (three CLAP stages).
async fn build_mock_server() -> MockServer {
    let server = MockServer::start().await;
    let body = json!({
        "vector": canned_vector(),
        "dim": DIM,
        "model_version": "stub-v1",
    });
    let template = ResponseTemplate::new(200)
        .set_body_json(body.clone())
        .insert_header(
            "Server-Timing",
            "decode;dur=12.3, resample;dur=4.5, gpu_forward;dur=2480",
        );

    // Two mounts (one per path). wiremock matches mounts by predicate;
    // a single Mock with no path filter would also work but the
    // explicit form documents what we're benching.
    Mock::given(method("POST"))
        .and(path("/embed/audio"))
        .respond_with(template.clone())
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/embed/text"))
        .respond_with(template)
        .mount(&server)
        .await;

    server
}

fn build_client(server: &MockServer) -> EmbedderClient {
    EmbedderClient::new(EmbedderConfig {
        url: server.uri().parse().expect("server uri"),
        timeout: Duration::from_secs(5),
        bearer_token: None,
    })
    .expect("client builds")
}

fn bench_embed_audio(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime");
    let server = rt.block_on(build_mock_server());
    let client = build_client(&server);

    let mut group = c.benchmark_group("embedder_client_embed_audio");
    group.sample_size(30);

    // Body sizes mirror real ingest clips: ~1KB (metadata-only fake),
    // ~256KB (~3s of 192kbps mp3), ~1MB (~12s). The wiremock returns
    // the same response regardless of body size — so this isolates
    // request-upload + response-parse cost as the body grows.
    for &size in &[1024usize, 256 * 1024, 1024 * 1024] {
        let payload = Bytes::from(vec![0xABu8; size]);

        group.bench_with_input(BenchmarkId::from_parameter(size), &payload, |b, p| {
            b.iter(|| {
                rt.block_on(async {
                    let r = client
                        .embed_audio(black_box(p.clone()))
                        .await
                        .expect("embed");
                    black_box(r);
                });
            });
        });
    }
    group.finish();
}

fn bench_embed_text(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime");
    let server = rt.block_on(build_mock_server());
    let client = build_client(&server);

    let mut group = c.benchmark_group("embedder_client_embed_text");
    group.sample_size(30);

    for &len in &[16usize, 1024, 16 * 1024] {
        let text = "x".repeat(len);

        group.bench_with_input(BenchmarkId::from_parameter(len), &text, |b, t| {
            b.iter(|| {
                rt.block_on(async {
                    let r = client
                        .embed_text(black_box(t.as_str()))
                        .await
                        .expect("embed");
                    black_box(r);
                });
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_embed_audio, bench_embed_text);
criterion_main!(benches);
