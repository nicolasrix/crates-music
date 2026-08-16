//! `GET /v1/search` — typo-tolerant catalog search.
//!
//! Two paths:
//!   - index present → fuzzy match ("led zeplin" recovers Led Zeppelin),
//!     returned in a Subsonic `searchResult3` envelope;
//!   - index absent (boot window) → falls back to proxying Navidrome
//!     `search3`, mapped into the same envelope.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header::AUTHORIZATION};
use http_body_util::BodyExt;
use music_gateway::build_router;
use music_gateway::search::{Kind, Record, SearchIndex};
use serde_json::Value;
use tower::ServiceExt;
use wiremock::matchers::{method as m_method, path as m_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

fn auth(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {}", common::TEST_BEARER))
        .body(Body::empty())
        .unwrap()
}

fn records() -> Vec<Record> {
    vec![
        Record {
            kind: Kind::Artist,
            id: "ar-1".into(),
            name: "Led Zeppelin".into(),
            artist: None,
            artist_id: None,
            album: None,
            album_id: None,
            cover_art: None,
            duration_seconds: None,
        },
        Record {
            kind: Kind::Track,
            id: "tr-1".into(),
            name: "Stairway to Heaven".into(),
            artist: Some("Led Zeppelin".into()),
            artist_id: Some("ar-1".into()),
            album: Some("Led Zeppelin IV".into()),
            album_id: Some("al-1".into()),
            cover_art: None,
            // "Stairway to Heaven" — the value the song row must surface.
            duration_seconds: Some(482),
        },
    ]
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn indexed_search_recovers_a_typo() {
    let state = common::build_state(common::test_config()).await;
    // Inject a built index through the shared handle (what the boot task
    // would fill).
    *state.search_handle().write().unwrap() = Some(Arc::new(SearchIndex::build(records()).unwrap()));
    let app = build_router(state);

    // "led zeplin" — the exact typo Navidrome's prefix match fails on.
    let resp = app.oneshot(auth("/v1/search?q=led%20zeplin")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let result = &json["subsonic-response"]["searchResult3"];
    assert_eq!(
        result["artist"][0]["name"], "Led Zeppelin",
        "typo'd query should still surface the artist"
    );
    // The track is surfaced via its secondary (artist) field, and carries
    // an album-id-derived cover so the row isn't a bare placeholder.
    assert_eq!(result["song"][0]["id"], "tr-1");
    assert_eq!(result["song"][0]["coverArt"], "al-1");
    // Regression: the song row carried no `duration`, so every search
    // result rendered "0:00" in the web track table (`fmtDuration`
    // returns "0:00" for undefined).
    assert_eq!(
        result["song"][0]["duration"], 482,
        "indexed song rows must carry duration"
    );
}

/// The fallback path builds the same `SongDto`, so it lost `duration`
/// too — cover it separately since the two mappings are hand-written.
#[tokio::test]
async fn fallback_song_rows_carry_duration() {
    let upstream = MockServer::start().await;
    let search3_body = serde_json::json!({
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "searchResult3": {
                "artist": [],
                "album": [],
                "song": [{
                    "id": "tr-9",
                    "title": "Weird Fishes",
                    "artist": "Radiohead",
                    "album": "In Rainbows",
                    "albumId": "al-9",
                    "duration": 321
                }]
            }
        }
    });
    Mock::given(m_method("GET"))
        .and(m_path("/rest/search3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search3_body))
        .expect(1)
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state(cfg).await;
    let app = build_router(state);

    let resp = app.oneshot(auth("/v1/search?q=weird%20fishes")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let song = &json["subsonic-response"]["searchResult3"]["song"][0];
    assert_eq!(song["id"], "tr-9");
    assert_eq!(song["duration"], 321, "fallback song rows must carry duration");
}

#[tokio::test]
async fn falls_back_to_navidrome_when_index_absent() {
    // A Subsonic search3 envelope the mock Navidrome will return.
    let upstream = MockServer::start().await;
    let search3_body = serde_json::json!({
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "searchResult3": {
                "artist": [{"id": "ar-9", "name": "Radiohead"}],
                "album": [],
                "song": []
            }
        }
    });
    Mock::given(m_method("GET"))
        .and(m_path("/rest/search3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(search3_body))
        .expect(1)
        .mount(&upstream)
        .await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    // No index injected → handler must fall back to Navidrome.
    let state = common::build_state(cfg).await;
    let app = build_router(state);

    let resp = app.oneshot(auth("/v1/search?q=radiohead")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    assert_eq!(
        json["subsonic-response"]["searchResult3"]["artist"][0]["name"],
        "Radiohead",
        "fallback should map Navidrome's search3 into the same envelope"
    );
}
