//! End-to-end behavior of the gateway's `SubsonicAudioFetcher`.
//!
//! These tests assert the URL/header shape of the fetch_clip call against
//! a wiremock server. They're the safety net that the
//! middle-window-via-timeOffset optimization actually fires.

use bytes::Bytes;
use music_core::TrackId;
use music_gateway::config::UpstreamConfig;
use music_gateway::ingest::SubsonicAudioFetcher;
use music_recommend::ingest::AudioFetcher;
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

#[tokio::test]
async fn mp3_source_also_transcodes() {
    // MP3 sources transcode too. The old raw-MP3 fast path (no
    // ?format=mp3) mis-decoded VBR-header MP3s when the byte Range
    // truncated them: the file's Xing header claims the full track
    // length, so a truncated clip decodes to ~0.1 s — below MERT's 6 s
    // floor. Forcing a transcode yields a fresh header-less stream that
    // survives the Range cap. Navidrome honours the Range for MP3→MP3,
    // so the clip stays bounded.
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/rest/getSong"))
        .and(query_param("id", "t-mp3"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(get_song_body_with_suffix(
                "t-mp3",
                Some(1042),
                Some("mp3"),
            )),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/rest/stream"))
        .and(query_param("id", "t-mp3"))
        .and(query_param("format", "mp3"))
        .and(query_param("maxBitRate", "192"))
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
            ResponseTemplate::new(200).set_body_json(get_song_body_with_suffix(
                "t-flac",
                Some(1042),
                Some("flac"),
            )),
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
