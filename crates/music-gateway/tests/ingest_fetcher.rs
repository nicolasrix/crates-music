//! End-to-end behavior of the gateway's `SubsonicAudioFetcher`.
//!
//! These tests assert the URL/header shape of the fetch_clip call against
//! a wiremock server. They're the safety net that the
//! middle-window-via-timeOffset optimization actually fires.

use bytes::Bytes;
use music_core::TrackId;
use music_gateway::config::UpstreamConfig;
use music_gateway::diagnostics::{SpanRecord, TraceLayer};
use music_gateway::ingest::SubsonicAudioFetcher;
use music_recommend::ingest::AudioFetcher;
use tokio::sync::mpsc;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::Registry;
use wiremock::matchers::{header, header_regex, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn upstream(url: &str) -> UpstreamConfig {
    UpstreamConfig {
        navidrome_url: url.to_string(),
        username: "alice".into(),
        password: "sesame".into(),
    }
}

/// Subsonic getSong response wrapper for a track of the given duration.
fn get_song_body(track_id: &str, duration: Option<u32>) -> serde_json::Value {
    get_song_body_with_suffix(track_id, duration, None)
}

fn get_song_body_with_suffix(
    track_id: &str,
    duration: Option<u32>,
    suffix: Option<&str>,
) -> serde_json::Value {
    let mut song = serde_json::json!({
        "id": track_id,
        "title": "Test Track",
    });
    if let Some(d) = duration {
        song["duration"] = serde_json::json!(d);
    }
    if let Some(s) = suffix {
        song["suffix"] = serde_json::json!(s);
    }
    serde_json::json!({
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "song": song,
        }
    })
}

#[tokio::test]
async fn long_track_fetches_with_centered_time_offset() {
    let server = MockServer::start().await;

    // 1042 s track → centered 12 s window starts at offset (1042-12)/2 = 515.
    Mock::given(method("GET"))
        .and(path("/rest/getSong"))
        .and(query_param("id", "t-long"))
        .respond_with(ResponseTemplate::new(200).set_body_json(get_song_body("t-long", Some(1042))))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/stream"))
        .and(query_param("id", "t-long"))
        .and(query_param("format", "mp3"))
        .and(query_param("maxBitRate", "192"))
        .and(query_param("timeOffset", "515"))
        // The byte range cap is the small one: ~384 KiB. Match a leading
        // "bytes=0-" then a 6-digit number — anything in [100000, 999999]
        // qualifies as "the new small cap, not the old 8 MiB one".
        .and(header_regex("range", r"^bytes=0-\d{6}$"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(b"audio-bytes".to_vec()))
        .mount(&server)
        .await;

    let fetcher = SubsonicAudioFetcher::new(&upstream(&server.uri())).unwrap();
    let bytes: Bytes = fetcher.fetch_clip(&TrackId::from("t-long")).await.unwrap();
    assert_eq!(bytes.as_ref(), b"audio-bytes");
}

#[tokio::test]
async fn short_track_omits_time_offset() {
    let server = MockServer::start().await;

    // 8 s track is shorter than the 12 s window — embed the whole thing,
    // no timeOffset.
    Mock::given(method("GET"))
        .and(path("/rest/getSong"))
        .and(query_param("id", "t-short"))
        .respond_with(ResponseTemplate::new(200).set_body_json(get_song_body("t-short", Some(8))))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/stream"))
        .and(query_param("id", "t-short"))
        .and(query_param("format", "mp3"))
        .and(query_param_is_missing("timeOffset"))
        // (192_000 / 8) * 12 + 96 KiB - 1 = 288000 + 98304 - 1 = 386303
        .and(header("range", "bytes=0-386303"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(b"short".to_vec()))
        .mount(&server)
        .await;

    let fetcher = SubsonicAudioFetcher::new(&upstream(&server.uri())).unwrap();
    let bytes = fetcher.fetch_clip(&TrackId::from("t-short")).await.unwrap();
    assert_eq!(bytes.as_ref(), b"short");
}

#[tokio::test]
async fn missing_duration_falls_back_to_offset_zero() {
    // If Navidrome returns getSong without a duration field (older
    // servers, edge-case files), we still fetch the clip — just from the
    // start of the track. Quietly degrade rather than fail.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/getSong"))
        .and(query_param("id", "t-nodur"))
        .respond_with(ResponseTemplate::new(200).set_body_json(get_song_body("t-nodur", None)))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/stream"))
        .and(query_param("id", "t-nodur"))
        .and(query_param_is_missing("timeOffset"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(b"x".to_vec()))
        .mount(&server)
        .await;

    let fetcher = SubsonicAudioFetcher::new(&upstream(&server.uri())).unwrap();
    fetcher.fetch_clip(&TrackId::from("t-nodur")).await.unwrap();
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
        .respond_with(ResponseTemplate::new(200).set_body_json(get_song_body("t-spans", Some(60))))
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

#[tokio::test]
async fn mp3_source_skips_transcode_and_keeps_offset() {
    // When the source codec is already MP3, the fetcher must skip the
    // ?format=mp3&maxBitRate=192 params — those force Navidrome to run
    // ffmpeg at realtime rate, which dominates wall-clock per fetch.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/getSong"))
        .and(query_param("id", "t-mp3"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(get_song_body_with_suffix("t-mp3", Some(1042), Some("mp3"))),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/stream"))
        .and(query_param("id", "t-mp3"))
        .and(query_param_is_missing("format"))
        .and(query_param_is_missing("maxBitRate"))
        .and(query_param("timeOffset", "515"))
        .and(header_regex("range", r"^bytes=0-\d{6}$"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(b"mp3-bytes".to_vec()))
        .mount(&server)
        .await;

    let fetcher = SubsonicAudioFetcher::new(&upstream(&server.uri())).unwrap();
    let bytes = fetcher.fetch_clip(&TrackId::from("t-mp3")).await.unwrap();
    assert_eq!(bytes.as_ref(), b"mp3-bytes");
}

#[tokio::test]
async fn flac_source_keeps_transcode_params() {
    // Conservatively, anything not MP3 still goes through the transcode
    // path — FLAC truncated mid-frame causes decoder desync.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/getSong"))
        .and(query_param("id", "t-flac"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(get_song_body_with_suffix("t-flac", Some(1042), Some("flac"))),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/stream"))
        .and(query_param("id", "t-flac"))
        .and(query_param("format", "mp3"))
        .and(query_param("maxBitRate", "192"))
        .and(query_param("timeOffset", "515"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(b"flac-bytes".to_vec()))
        .mount(&server)
        .await;

    let fetcher = SubsonicAudioFetcher::new(&upstream(&server.uri())).unwrap();
    let bytes = fetcher.fetch_clip(&TrackId::from("t-flac")).await.unwrap();
    assert_eq!(bytes.as_ref(), b"flac-bytes");
}

#[tokio::test]
async fn get_song_failure_falls_back_to_offset_zero() {
    // Transient failures of getSong shouldn't take down the worker —
    // we degrade to "fetch from the start" with a debug log.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/getSong"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/stream"))
        .and(query_param_is_missing("timeOffset"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(b"y".to_vec()))
        .mount(&server)
        .await;

    let fetcher = SubsonicAudioFetcher::new(&upstream(&server.uri())).unwrap();
    fetcher.fetch_clip(&TrackId::from("t-fail")).await.unwrap();
}
