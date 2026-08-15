//! `/v1/lyrics/*` end to end: the resolution ladder, what is cacheable,
//! and the authorization tier.
//!
//! Both upstreams are wiremocked — a Navidrome for the `songLyrics`
//! extension and a stand-in for the external provider — so the tests
//! assert not just the response but *how many* upstream calls it took.
//! That is the only way to prove the cache and the negative cache work.

use std::time::Duration;

use axum::body::Body;
use axum::http::header::{AUTHORIZATION, IF_NONE_MATCH};
use axum::http::{Request, StatusCode};
use music_gateway::build_router;
use music_gateway::config::{Config, LyricsConfig};
use music_gateway::oauth::{NewClient, NewUser, OauthStore, SetupToken};
use music_gateway::state::AppState;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

const GUEST_EXPIRES_MS: i64 = 32_503_680_000_000; // ~year 3000

fn subsonic_ok(payload: &Value) -> Value {
    let mut body = json!({ "status": "ok", "version": "1.16.1" });
    if let (Some(obj), Some(extra)) = (body.as_object_mut(), payload.as_object()) {
        for (k, v) in extra {
            obj.insert(k.clone(), v.clone());
        }
    }
    json!({ "subsonic-response": body })
}

/// A Navidrome that knows the track but has no lyrics for it — the
/// measured norm for an untagged library.
async fn navidrome_without_lyrics(server: &MockServer, track: &str, duration: u32) {
    Mock::given(method("GET"))
        .and(path("/rest/getLyricsBySongId"))
        .and(query_param("id", track))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(subsonic_ok(&json!({ "lyricsList": {} }))),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/rest/getSong"))
        .and(query_param("id", track))
        .respond_with(ResponseTemplate::new(200).set_body_json(subsonic_ok(&json!({
            "song": {
                "id": track,
                "title": "Roygbiv",
                "artist": "Boards of Canada",
                "album": "Music Has the Right to Children",
                "duration": duration,
            }
        }))))
        .mount(server)
        .await;
}

fn config_for(navidrome: &MockServer, provider: Option<&MockServer>) -> Config {
    let mut config = common::test_config_with_upstream(&navidrome.uri(), "u", "p");
    config.lyrics = LyricsConfig {
        external_lookup: provider.is_some(),
        provider_url: provider.map_or_else(
            || "http://127.0.0.1:1".to_string(),
            wiremock::MockServer::uri,
        ),
        ..LyricsConfig::default()
    };
    config
}

async fn get(state: &AppState, path: &str, token: &str) -> (StatusCode, Value, Option<String>) {
    send(state, "GET", path, token, None).await
}

async fn send(
    state: &AppState,
    verb: &str,
    path: &str,
    token: &str,
    if_none_match: Option<&str>,
) -> (StatusCode, Value, Option<String>) {
    let mut builder = Request::builder()
        .method(verb)
        .uri(path)
        .header(AUTHORIZATION, format!("Bearer {token}"));
    if let Some(etag) = if_none_match {
        builder = builder.header(IF_NONE_MATCH, etag);
    }
    let resp = build_router(state.clone())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let etag = resp
        .headers()
        .get(axum::http::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body, etag)
}

#[tokio::test]
async fn navidrome_tags_win_and_are_cached() {
    let nav = MockServer::start().await;
    // `expect(1)` is the real assertion: the second request must be
    // served from the cache, not re-fetched.
    Mock::given(method("GET"))
        .and(path("/rest/getLyricsBySongId"))
        .respond_with(ResponseTemplate::new(200).set_body_json(subsonic_ok(&json!({
            "lyricsList": { "structuredLyrics": [{
                "lang": "eng", "synced": true, "offset": 0,
                "line": [
                    {"start": 1000, "value": "from the file's own tags"},
                    {"start": 5000, "value": "second line"}
                ]
            }]}
        }))))
        .expect(1)
        .mount(&nav)
        .await;

    let state = common::build_state(config_for(&nav, None)).await;
    let (status, body, _) = get(&state, "/v1/lyrics/tr-1", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "navidrome");
    assert_eq!(body["synced"], true);
    assert_eq!(body["lines"][0]["start_ms"], 1000);
    assert_eq!(body["lines"][1]["text"], "second line");
    // Plain text is derived too, so a static view never strips timestamps.
    assert_eq!(body["plain"], "from the file's own tags\nsecond line");

    let (status, body, _) = get(&state, "/v1/lyrics/tr-1", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "navidrome");
}

#[tokio::test]
async fn external_provider_fills_in_when_the_file_has_no_tags() {
    let nav = MockServer::start().await;
    navidrome_without_lyrics(&nav, "tr-2", 301).await;

    let provider = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .and(query_param("artist_name", "Boards of Canada"))
        .and(query_param("track_name", "Roygbiv"))
        .and(query_param("album_name", "Music Has the Right to Children"))
        .and(query_param("duration", "301"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 771,
            "trackName": "Roygbiv",
            "artistName": "Boards of Canada",
            "duration": 301.0,
            "instrumental": false,
            "plainLyrics": "provider words",
            "syncedLyrics": "[00:02.50]provider words"
        })))
        .expect(1)
        .mount(&provider)
        .await;

    let state = common::build_state(config_for(&nav, Some(&provider))).await;
    let (status, body, _) = get(&state, "/v1/lyrics/tr-2", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "lrclib");
    // Album + duration were both supplied, so this is the exact tier.
    assert_eq!(body["match_kind"], "exact");
    assert_eq!(body["lines"][0]["start_ms"], 2500);
    assert_eq!(body["provider_id"], "771");
}

#[tokio::test]
async fn album_is_dropped_before_giving_up() {
    let nav = MockServer::start().await;
    navidrome_without_lyrics(&nav, "tr-3", 301).await;

    let provider = MockServer::start().await;
    // Tagged album disagrees with the provider's ("… (Deluxe Edition)"),
    // so the exact tier misses and the album-less retry has to save it.
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .and(query_param("album_name", "Music Has the Right to Children"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&provider)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 772,
            "trackName": "Roygbiv",
            "artistName": "Boards of Canada",
            "duration": 301.0,
            "syncedLyrics": "[00:01.00]found without the album"
        })))
        .mount(&provider)
        .await;

    let state = common::build_state(config_for(&nav, Some(&provider))).await;
    let (status, body, _) = get(&state, "/v1/lyrics/tr-3", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["match_kind"], "no_album");
    assert_eq!(body["lines"][0]["text"], "found without the album");
}

#[tokio::test]
async fn a_confirmed_absence_is_cached_as_a_miss() {
    let nav = MockServer::start().await;
    navidrome_without_lyrics(&nav, "tr-4", 200).await;

    let provider = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(ResponseTemplate::new(404))
        // Two calls on the first resolution (exact, then album-less) and
        // none on the second — the negative cache doing its job.
        .expect(2)
        .mount(&provider)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&provider)
        .await;

    let state = common::build_state(config_for(&nav, Some(&provider))).await;
    let (status, body, _) = get(&state, "/v1/lyrics/tr-4", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "none");
    assert!(body["lines"].is_null());
    assert!(body["plain"].is_null());

    let (status, body, _) = get(&state, "/v1/lyrics/tr-4", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "none");
}

#[tokio::test]
async fn fuzzy_search_respects_the_duration_guard() {
    let nav = MockServer::start().await;
    navidrome_without_lyrics(&nav, "tr-5", 301).await;

    let provider = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&provider)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            // A live version, 40 s long — exactly the wrong-edition match
            // the guard exists to reject.
            {"id": 1, "trackName": "Roygbiv (Live)", "artistName": "Boards of Canada",
             "duration": 341.0, "syncedLyrics": "[00:01.00]live version"},
            {"id": 2, "trackName": "Roygbiv", "artistName": "Boards of Canada",
             "duration": 302.0, "syncedLyrics": "[00:03.00]studio version"}
        ])))
        .mount(&provider)
        .await;

    let state = common::build_state(config_for(&nav, Some(&provider))).await;
    let (status, body, _) = get(&state, "/v1/lyrics/tr-5", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["match_kind"], "search");
    assert_eq!(body["provider_id"], "2");
    assert_eq!(body["lines"][0]["text"], "studio version");
}

#[tokio::test]
async fn a_provider_outage_is_never_cached_as_a_miss() {
    let nav = MockServer::start().await;
    navidrome_without_lyrics(&nav, "tr-6", 200).await;

    let provider = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&provider)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&provider)
        .await;

    let state = common::build_state(config_for(&nav, Some(&provider))).await;
    // "Could not check" must not render as "there are none" …
    let (status, _, _) = get(&state, "/v1/lyrics/tr-6", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // … and must not be stored, or a brief outage would blank out every
    // track played during it for the whole miss TTL.
    let (status, _, _) = get(&state, "/v1/lyrics/tr-6", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn a_stale_hit_beats_an_outage() {
    let nav = MockServer::start().await;
    // First resolution succeeds from the file's tags.
    let good = Mock::given(method("GET"))
        .and(path("/rest/getLyricsBySongId"))
        .respond_with(ResponseTemplate::new(200).set_body_json(subsonic_ok(&json!({
            "lyricsList": { "structuredLyrics": [{
                "synced": true, "offset": 0,
                "line": [{"start": 1000, "value": "cached earlier"}]
            }]}
        }))));
    let guard = nav.register_as_scoped(good).await;

    let mut config = config_for(&nav, None);
    // Expire immediately so the next read must re-resolve.
    config.lyrics.hit_ttl_days = 0;
    let state = common::build_state(config).await;
    let (status, _, _) = get(&state, "/v1/lyrics/tr-7", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::OK);

    // Now Navidrome starts failing, and the external path is disabled.
    drop(guard);
    Mock::given(method("GET"))
        .and(path("/rest/getLyricsBySongId"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&nav)
        .await;

    let (status, body, _) = get(&state, "/v1/lyrics/tr-7", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lines"][0]["text"], "cached earlier");
}

#[tokio::test]
async fn repeat_reads_revalidate_with_an_etag() {
    let nav = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getLyricsBySongId"))
        .respond_with(ResponseTemplate::new(200).set_body_json(subsonic_ok(&json!({
            "lyricsList": { "structuredLyrics": [{
                "synced": true, "offset": 0,
                "line": [{"start": 1000, "value": "hello"}]
            }]}
        }))))
        .mount(&nav)
        .await;

    let state = common::build_state(config_for(&nav, None)).await;
    let (status, _, etag) = get(&state, "/v1/lyrics/tr-8", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::OK);
    let etag = etag.expect("a strong etag is issued");

    let (status, body, _) = send(
        &state,
        "GET",
        "/v1/lyrics/tr-8",
        common::TEST_BEARER,
        Some(&etag),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body.is_null());
}

#[tokio::test]
async fn disabled_lyrics_answer_404() {
    let nav = MockServer::start().await;
    let mut config = config_for(&nav, None);
    config.lyrics.enabled = false;
    let state = common::build_state(config).await;

    // A disabled feature is an absent route, not a broken one.
    let (status, _, _) = get(&state, "/v1/lyrics/tr-9", common::TEST_BEARER).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn guests_may_read_lyrics_but_not_refresh_them() {
    let nav = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/getLyricsBySongId"))
        .respond_with(ResponseTemplate::new(200).set_body_json(subsonic_ok(&json!({
            "lyricsList": { "structuredLyrics": [{
                "synced": true, "offset": 0,
                "line": [{"start": 0, "value": "shared catalog data"}]
            }]}
        }))))
        .mount(&nav)
        .await;

    let oauth = OauthStore::open_in_memory().await.unwrap();
    oauth.set_master_password_hash("$argon2id$dummy").await.unwrap();
    oauth
        .register_client(NewClient {
            client_id: "web".to_string(),
            name: "Web".to_string(),
            redirect_uris: vec!["http://localhost:5173/cb".to_string()],
        })
        .await
        .unwrap();
    let guest_id = oauth
        .insert_user(NewUser {
            username: None,
            display_name: Some("Guest".to_string()),
            role: "guest".to_string(),
            password_hash: None,
            host_user_id: Some(1),
            expires_at_unix_ms: Some(GUEST_EXPIRES_MS),
        })
        .await
        .unwrap();
    let guest_token = oauth
        .mint_access_token_for_user("web", None, Duration::from_hours(1), Some(guest_id))
        .await
        .unwrap()
        .token;

    let state =
        common::build_state_with_oauth(config_for(&nav, None), oauth, SetupToken::none()).await;

    // Reading is the browse tier — a guest sees lyrics like any album.
    let (status, body, _) = get(&state, "/v1/lyrics/tr-10", &guest_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lines"][0]["text"], "shared catalog data");

    // Refreshing re-resolves a row the whole household shares, so it sits
    // on the write tier.
    let (status, _, _) = send(
        &state,
        "POST",
        "/v1/lyrics/tr-10/refresh",
        &guest_token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn refresh_re_resolves_a_bad_match() {
    let nav = MockServer::start().await;
    navidrome_without_lyrics(&nav, "tr-11", 301).await;

    let provider = MockServer::start().await;
    let wrong = Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1,
            "trackName": "Roygbiv",
            "artistName": "Boards of Canada",
            "duration": 301.0,
            "syncedLyrics": "[00:01.00]the wrong song"
        })));
    let guard = provider.register_as_scoped(wrong).await;

    let state = common::build_state(config_for(&nav, Some(&provider))).await;
    let (_, body, _) = get(&state, "/v1/lyrics/tr-11", common::TEST_BEARER).await;
    assert_eq!(body["lines"][0]["text"], "the wrong song");

    // The provider entry gets corrected upstream; a refresh must reach
    // past the still-unexpired cached row to see it.
    drop(guard);
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 2,
            "trackName": "Roygbiv",
            "artistName": "Boards of Canada",
            "duration": 301.0,
            "syncedLyrics": "[00:01.00]the right song"
        })))
        .mount(&provider)
        .await;

    let (status, body, _) = send(
        &state,
        "POST",
        "/v1/lyrics/tr-11/refresh",
        common::TEST_BEARER,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lines"][0]["text"], "the right song");
    assert_eq!(body["provider_id"], "2");
}
