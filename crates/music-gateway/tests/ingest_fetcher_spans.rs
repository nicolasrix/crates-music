//! Isolated trace-capture test for `SubsonicAudioFetcher::fetch_clip`.
//!
//! Why its own test binary: the assertion below relies on a `TraceLayer`
//! installed via `tracing::dispatcher::set_default` (thread-local). When
//! this test runs in the same binary as `ingest_fetcher.rs`'s other
//! tests under `cargo test`'s default parallelism, sibling tests'
//! reqwest/hyper internals interact with `tracing-core`'s process-wide
//! callsite registry in a way that drops some inner spans before they
//! reach our layer — `[fetch_clip.stream_body, test.root]` instead of
//! the full four-span trace, deterministically. Running solo in its own
//! binary is the cheapest fix; the binary boundary forces serial
//! execution against itself (only one test inside) and gives each
//! `cargo test` invocation a clean tracing process state.

use music_core::TrackId;
use music_gateway::config::UpstreamConfig;
use music_gateway::diagnostics::{SpanRecord, TraceLayer};
use music_gateway::ingest::SubsonicAudioFetcher;
use music_recommend::ingest::AudioFetcher;
use tokio::sync::mpsc;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::Registry;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn upstream(url: &str) -> UpstreamConfig {
    UpstreamConfig {
        navidrome_url: url.to_string(),
        username: "alice".into(),
        password: "sesame".into(),
    }
}

fn get_song_body(track_id: &str, duration: u32) -> serde_json::Value {
    serde_json::json!({
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "song": { "id": track_id, "title": "Test Track", "duration": duration },
        }
    })
}

fn drain_all(rx: &mut mpsc::Receiver<SpanRecord>) -> Vec<SpanRecord> {
    let mut out = Vec::new();
    while let Ok(r) = rx.try_recv() {
        out.push(r);
    }
    out
}

#[tokio::test]
async fn fetch_clip_emits_subspans_for_get_song_request_and_body() {
    // The point of this test: ingest.fetch_clip should be observable as
    // a parent of three children (get_song, stream_request, stream_body)
    // so the diagnostics UI can attribute the ~12 s observed wall time
    // to the right phase.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getSong"))
        .respond_with(ResponseTemplate::new(200).set_body_json(get_song_body("t-spans", 60)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/stream"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(b"audio".to_vec()))
        .mount(&server)
        .await;

    let (layer, mut rx) = TraceLayer::new(64);
    let dispatch: tracing::Dispatch = Registry::default().with(layer).into();

    let fetcher = SubsonicAudioFetcher::new(&upstream(&server.uri())).unwrap();
    let trace_id = TrackId::from("t-spans");
    // set_default returns a guard usable inside an async runtime; the
    // synchronous `with_default` would have to block_on, which is
    // illegal nested inside #[tokio::test].
    {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        let root = tracing::info_span!("test.root");
        use tracing::Instrument;
        fetcher
            .fetch_clip(&trace_id)
            .instrument(root)
            .await
            .unwrap();
    }

    let records = drain_all(&mut rx);
    let names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();

    let parent = records
        .iter()
        .find(|r| r.name == "ingest.fetch_clip")
        .unwrap_or_else(|| panic!("expected ingest.fetch_clip; got {names:?}"));

    let children: Vec<&SpanRecord> = records
        .iter()
        .filter(|r| r.parent_span_id == Some(parent.span_id))
        .collect();
    let child_names: Vec<&str> = children.iter().map(|r| r.name.as_str()).collect();

    assert!(
        child_names.contains(&"fetch_clip.get_song"),
        "expected child fetch_clip.get_song; got {child_names:?}"
    );
    assert!(
        child_names.contains(&"fetch_clip.stream_request"),
        "expected child fetch_clip.stream_request; got {child_names:?}"
    );
    assert!(
        child_names.contains(&"fetch_clip.stream_body"),
        "expected child fetch_clip.stream_body; got {child_names:?}"
    );
}
