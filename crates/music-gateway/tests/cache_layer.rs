//! Cache layer for Subsonic browse endpoints.
//!
//! Verifies:
//!   - browse responses are cached and served without re-hitting upstream;
//!   - cached responses carry an `ETag` response header;
//!   - `If-None-Match` matches → `304`;
//!   - `If-None-Match` mismatch → cached body returned (200);
//!   - cache keys normalise query-param order;
//!   - non-browse endpoints (e.g. `/rest/ping`) bypass the cache entirely;
//!   - explicit cache expiry forces a refetch.

use std::time::{Duration, SystemTime};

use axum::body::Body;
use axum::http::{
    Request, StatusCode,
    header::{AUTHORIZATION, ETAG, IF_NONE_MATCH},
};
use http_body_util::BodyExt;
use music_cache::Cache;
use music_gateway::build_router;
use tower::ServiceExt;
use wiremock::matchers::{method as m_method, path as m_path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

fn auth(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {}", common::TEST_BEARER))
        .body(Body::empty())
        .unwrap()
}

fn auth_if_none_match(uri: &str, etag: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {}", common::TEST_BEARER))
        .header(IF_NONE_MATCH, etag)
        .body(Body::empty())
        .unwrap()
}

fn ok_album_list2() -> serde_json::Value {
    serde_json::json!({
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "albumList2": { "album": [
                {"id": "al-1", "name": "Music for Airports",
                 "artist": "Brian Eno", "artistId": "ar-1",
                 "songCount": 4, "duration": 2880, "year": 1978}
            ]}
        }
    })
}

#[tokio::test]
async fn browse_response_is_cached_then_served_without_second_upstream_call() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_album_list2()))
        .expect(1)
        .mount(&upstream)
        .await;

    let app = build_router(
        common::build_state(common::test_config_with_upstream(
            &upstream.uri(),
            "alice",
            "sesame",
        ))
        .await,
    );

    let r1 = app
        .clone()
        .oneshot(auth("/rest/getAlbumList2?type=newest&size=20"))
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);

    let r2 = app
        .oneshot(auth("/rest/getAlbumList2?type=newest&size=20"))
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::OK);
    // wiremock will assert .expect(1) on drop: any second upstream hit fails.
}

#[tokio::test]
async fn cached_response_carries_etag_header() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_album_list2()))
        .mount(&upstream)
        .await;

    let app = build_router(
        common::build_state(common::test_config_with_upstream(
            &upstream.uri(),
            "alice",
            "sesame",
        ))
        .await,
    );

    let response = app
        .oneshot(auth("/rest/getAlbumList2?type=newest"))
        .await
        .unwrap();
    assert!(response.headers().get(ETAG).is_some(), "ETag must be set");
}

#[tokio::test]
async fn if_none_match_match_returns_304() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_album_list2()))
        .mount(&upstream)
        .await;

    let app = build_router(
        common::build_state(common::test_config_with_upstream(
            &upstream.uri(),
            "alice",
            "sesame",
        ))
        .await,
    );

    // Populate, capture the etag.
    let r1 = app
        .clone()
        .oneshot(auth("/rest/getAlbumList2?type=newest"))
        .await
        .unwrap();
    let etag = r1
        .headers()
        .get(ETAG)
        .expect("etag header")
        .to_str()
        .unwrap()
        .to_string();

    // Re-request with matching If-None-Match.
    let r2 = app
        .oneshot(auth_if_none_match("/rest/getAlbumList2?type=newest", &etag))
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn if_none_match_mismatch_returns_cached_body() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_album_list2()))
        .mount(&upstream)
        .await;

    let app = build_router(
        common::build_state(common::test_config_with_upstream(
            &upstream.uri(),
            "alice",
            "sesame",
        ))
        .await,
    );

    // Populate.
    let _ = app
        .clone()
        .oneshot(auth("/rest/getAlbumList2?type=newest"))
        .await
        .unwrap();

    let response = app
        .oneshot(auth_if_none_match(
            "/rest/getAlbumList2?type=newest",
            "some-other-etag",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!bytes.is_empty(), "expected cached body, not 304");
}

#[tokio::test]
async fn cache_key_normalises_query_param_order() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .and(query_param("type", "newest"))
        .and(query_param("size", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_album_list2()))
        .expect(1)
        .mount(&upstream)
        .await;

    let app = build_router(
        common::build_state(common::test_config_with_upstream(
            &upstream.uri(),
            "alice",
            "sesame",
        ))
        .await,
    );

    let _ = app
        .clone()
        .oneshot(auth("/rest/getAlbumList2?type=newest&size=20"))
        .await
        .unwrap();
    let _ = app
        .oneshot(auth("/rest/getAlbumList2?size=20&type=newest"))
        .await
        .unwrap();
    // expect(1) verifies same cache key was hit twice — only one upstream call.
}

#[tokio::test]
async fn non_browse_endpoints_are_not_cached() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/ping"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "subsonic-response": { "status": "ok", "version": "1.16.1" }
        })))
        .expect(2)
        .mount(&upstream)
        .await;

    let app = build_router(
        common::build_state(common::test_config_with_upstream(
            &upstream.uri(),
            "alice",
            "sesame",
        ))
        .await,
    );

    let _ = app.clone().oneshot(auth("/rest/ping")).await.unwrap();
    let _ = app.oneshot(auth("/rest/ping")).await.unwrap();
    // expect(2) confirms /rest/ping isn't cached.
}

#[tokio::test]
async fn explicit_cache_expiry_triggers_refetch() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_album_list2()))
        .expect(2)
        .mount(&upstream)
        .await;

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state_with_cache(cfg, cache.clone()).await;
    let app = build_router(state);

    // Populate.
    let _ = app
        .clone()
        .oneshot(auth("/rest/getAlbumList2?type=newest"))
        .await
        .unwrap();

    // Force eviction of all entries (cutoff in the far future).
    let _removed = cache
        .expire_before(SystemTime::now() + Duration::from_secs(99_999))
        .await
        .unwrap();

    // Should refetch from upstream.
    let _ = app
        .oneshot(auth("/rest/getAlbumList2?type=newest"))
        .await
        .unwrap();
    // expect(2) confirms two upstream calls.
}
