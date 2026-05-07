//! Integration tests for the HTTP client against a wiremock server.
//! Validates URL construction, auth params, response parsing end-to-end.

use music_core::{AlbumId, TrackId};
use music_subsonic::{AlbumListType, Client, Credentials};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn creds() -> Credentials {
    Credentials {
        username: "alice".into(),
        password: "sesame".into(),
    }
}

fn ok_envelope(payload: &serde_json::Value) -> serde_json::Value {
    let mut inner = serde_json::json!({
        "status": "ok",
        "version": "1.16.1",
        "type": "navidrome"
    });
    if let serde_json::Value::Object(map) = payload {
        for (k, v) in map {
            inner[k] = v.clone();
        }
    }
    serde_json::json!({ "subsonic-response": inner })
}

#[tokio::test]
async fn ping_succeeds_against_ok_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/ping"))
        .and(query_param("u", "alice"))
        .and(query_param("c", "crates-music"))
        .and(query_param("f", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_envelope(&serde_json::json!({}))))
        .mount(&server)
        .await;

    let client = Client::new(&server.uri(), creds()).unwrap();
    client.ping().await.unwrap();
}

#[tokio::test]
async fn ping_returns_subsonic_error_on_failed_status() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "subsonic-response": {
            "status": "failed",
            "version": "1.16.1",
            "error": { "code": 40, "message": "Wrong username or password." }
        }
    });
    Mock::given(method("GET"))
        .and(path("/rest/ping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let client = Client::new(&server.uri(), creds()).unwrap();
    let err = client.ping().await.unwrap_err();
    let (code, msg) = err.subsonic_error().expect("subsonic error");
    assert_eq!(code, 40);
    assert!(msg.contains("Wrong username"));
}

#[tokio::test]
async fn get_album_list2_returns_typed_albums() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "albumList2": {
                "album": [{
                    "id": "al-1", "name": "Music for Airports",
                    "artist": "Brian Eno", "artistId": "ar-1",
                    "songCount": 4, "duration": 2880, "year": 1978
                }]
            }
        }
    });
    Mock::given(method("GET"))
        .and(path("/rest/getAlbumList2"))
        .and(query_param("type", "newest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let client = Client::new(&server.uri(), creds()).unwrap();
    let albums = client
        .get_album_list2(AlbumListType::Newest, Some(20), None)
        .await
        .unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].id, AlbumId::from("al-1"));
}

#[tokio::test]
async fn get_album_returns_album_with_tracks() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "album": {
                "id": "al-1", "name": "Music for Airports",
                "artist": "Brian Eno", "artistId": "ar-1",
                "songCount": 1, "duration": 1042,
                "song": [{
                    "id": "t-1", "title": "1/1", "albumId": "al-1",
                    "artistId": "ar-1", "duration": 1042, "track": 1
                }]
            }
        }
    });
    Mock::given(method("GET"))
        .and(path("/rest/getAlbum"))
        .and(query_param("id", "al-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let client = Client::new(&server.uri(), creds()).unwrap();
    let result = client.get_album(&AlbumId::from("al-1")).await.unwrap();
    assert_eq!(result.album.id, AlbumId::from("al-1"));
    assert_eq!(result.tracks.len(), 1);
    assert_eq!(result.tracks[0].id, TrackId::from("t-1"));
}

#[tokio::test]
async fn http_500_surfaces_as_transport_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/ping"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let client = Client::new(&server.uri(), creds()).unwrap();
    let err = client.ping().await.unwrap_err();
    assert!(
        err.subsonic_error().is_none(),
        "should be transport error, not subsonic error"
    );
}

#[tokio::test]
async fn stream_url_targets_rest_stream_with_track_id() {
    // We don't pull bytes here — just confirm the URL builder is correct.
    let client = Client::new("https://nav.example.com", creds()).unwrap();
    let url = client.stream_url(&TrackId::from("t-99")).unwrap();
    assert_eq!(url.path(), "/rest/stream");
    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q.get("id").map(String::as_str), Some("t-99"));
    assert_eq!(q.get("u").map(String::as_str), Some("alice"));
    assert_eq!(q.get("c").map(String::as_str), Some("crates-music"));
    assert!(q.contains_key("t"));
    assert!(q.contains_key("s"));
}
