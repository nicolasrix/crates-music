//! Cache layer for Subsonic browse endpoints.
//!
//! Verifies:
//!   - browse responses are cached and served without re-hitting upstream;
//!   - cached responses carry an `ETag` response header;
//!   - `If-None-Match` matches → `304`;
//!   - `If-None-Match` mismatch → cached body returned (200);
//!   - cache keys normalise query-param order;
//!   - non-browse endpoints (e.g. `/rest/ping`) bypass the cache entirely;
//!   - explicit cache expiry forces a refetch;
//!   - list endpoints get the short TTL, entity endpoints keep the long one;
//!   - a stale entry is served when upstream is unavailable (and `502` only
//!     when there's nothing cached at all).

use std::time::{Duration, SystemTime};

use axum::body::Body;
use axum::http::{
    Request, StatusCode,
    header::{AUTHORIZATION, ETAG, IF_NONE_MATCH, WARNING},
};
use bytes::Bytes;
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

fn ok_album() -> serde_json::Value {
    serde_json::json!({
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "album": {
                "id": "al-1", "name": "Music for Airports",
                "artist": "Brian Eno", "artistId": "ar-1",
                "songCount": 1, "duration": 1020, "year": 1978,
                "song": [
                    {"id": "tr-1", "title": "1/1", "album": "Music for Airports",
                     "artist": "Brian Eno", "duration": 1020}
                ]
            }
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

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state_with_cache(cfg, cache.clone()).await);

    let r1 = app
        .clone()
        .oneshot(auth("/rest/getAlbumList2?type=newest&size=20"))
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);

    common::wait_for_cache_entry(&cache, "getAlbumList2|size=20|type=newest").await;

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

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state_with_cache(cfg, cache.clone()).await);

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

    common::wait_for_cache_entry(&cache, "getAlbumList2|type=newest").await;

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

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state_with_cache(cfg, cache.clone()).await);

    // Populate.
    let _ = app
        .clone()
        .oneshot(auth("/rest/getAlbumList2?type=newest"))
        .await
        .unwrap();

    common::wait_for_cache_entry(&cache, "getAlbumList2|type=newest").await;

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

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state_with_cache(cfg, cache.clone()).await);

    let _ = app
        .clone()
        .oneshot(auth("/rest/getAlbumList2?type=newest&size=20"))
        .await
        .unwrap();

    common::wait_for_cache_entry(&cache, "getAlbumList2|size=20|type=newest").await;

    let _ = app
        .oneshot(auth("/rest/getAlbumList2?size=20&type=newest"))
        .await
        .unwrap();
    // expect(1) verifies same cache key was hit twice — only one upstream call.
}

#[tokio::test]
async fn random_album_list_bypasses_cache() {
    // `getAlbumList2?type=random` is the one BROWSE_METHODS entry whose
    // response isn't a pure function of the catalog — Navidrome shuffles
    // server-side per call. Caching it would pin the first shuffle for
    // browse_ttl_seconds and make the /albums/random page show the same
    // list every visit.
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .and(query_param("type", "random"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_album_list2()))
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

    let _ = app
        .clone()
        .oneshot(auth("/rest/getAlbumList2?type=random&size=20"))
        .await
        .unwrap();
    let _ = app
        .oneshot(auth("/rest/getAlbumList2?type=random&size=20"))
        .await
        .unwrap();
    // expect(2) verifies both calls reached upstream — i.e. the cache
    // didn't intercept the second one.
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

    // The cache write is now async (fire-and-forget tokio task), so
    // the entry may not be in SQLite yet by the time the response
    // returns. Poll until it lands — keeps the test deterministic
    // without baking the spawn-and-forget pattern into a test-only API.
    let key = "getAlbumList2|type=newest";
    for _ in 0..50 {
        if cache.get(key).await.unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        cache.get(key).await.unwrap().is_some(),
        "background cache write should have committed within 1s"
    );

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

/// The regression this whole split exists for: `getAlbumList2` is a view over
/// the catalog and must expire in about a page-load, while `getAlbum` is a
/// specific record that can safely sit for a day. Asserting on the *stored*
/// TTL rather than on elapsed time keeps this deterministic — the entry
/// carries its own `ttl_seconds`, so a future edit that collapses the two
/// back into one knob fails here immediately.
#[tokio::test]
async fn list_endpoints_expire_fast_while_entity_endpoints_stay_cached() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_album_list2()))
        .mount(&upstream)
        .await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbum"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_album()))
        .mount(&upstream)
        .await;

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let list_ttl = Duration::from_secs(cfg.cache.list_ttl_seconds);
    let entity_ttl = Duration::from_secs(cfg.cache.browse_ttl_seconds);
    let app = build_router(common::build_state_with_cache(cfg, cache.clone()).await);

    let _ = app
        .clone()
        .oneshot(auth("/rest/getAlbumList2?type=newest&size=10"))
        .await
        .unwrap();
    let _ = app.oneshot(auth("/rest/getAlbum?id=al-1")).await.unwrap();

    common::wait_for_cache_entry(&cache, "getAlbumList2|size=10|type=newest").await;
    common::wait_for_cache_entry(&cache, "getAlbum|id=al-1").await;
    let list = cache
        .get("getAlbumList2|size=10|type=newest")
        .await
        .unwrap()
        .expect("album list cached");
    let entity = cache
        .get("getAlbum|id=al-1")
        .await
        .unwrap()
        .expect("album cached");

    assert_eq!(
        list.ttl, list_ttl,
        "album lists must expire on the short list TTL"
    );
    assert_eq!(
        entity.ttl, entity_ttl,
        "entity lookups keep the long browse TTL"
    );
    assert!(list.ttl < entity.ttl);
}

/// Shortening the list TTL costs the accidental outage buffer the old 24 h
/// value provided, so the fallback has to make up for it: with upstream
/// unhealthy, a stale entry beats a dead library page.
#[tokio::test]
async fn stale_list_is_served_when_upstream_is_unavailable() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&upstream)
        .await;

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    // Sixty TTLs old — long expired, so the request goes upstream, finds a
    // 503, and must fall back to this body.
    let ttl = Duration::from_secs(cfg.cache.list_ttl_seconds);
    cache
        .insert_for_test(
            "getAlbumList2|size=10|type=newest",
            Bytes::from(serde_json::to_vec(&ok_album_list2()).unwrap()),
            SystemTime::now() - ttl * 60,
            ttl,
        )
        .await
        .unwrap();

    let app = build_router(common::build_state_with_cache(cfg, cache).await);

    let res = app
        .oneshot(auth("/rest/getAlbumList2?type=newest&size=10"))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get(WARNING).unwrap(),
        "110 - \"Response is Stale\"",
        "a stale fallback must be distinguishable from a live answer"
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["subsonic-response"]["albumList2"]["album"][0]["id"],
        "al-1"
    );
}

/// The same fallback, reached the other way. A refused connection fails
/// before any HTTP response exists, so it lands in a different arm than the
/// 503 above — both have to end up serving the stale body.
#[tokio::test]
async fn stale_list_is_served_when_upstream_is_unreachable() {
    let cache = Cache::open_in_memory().await.unwrap();
    // Port 1 is reserved and never listening: connection refused.
    let cfg = common::test_config_with_upstream("http://127.0.0.1:1", "alice", "sesame");
    let ttl = Duration::from_secs(cfg.cache.list_ttl_seconds);
    cache
        .insert_for_test(
            "getAlbumList2|size=10|type=newest",
            Bytes::from(serde_json::to_vec(&ok_album_list2()).unwrap()),
            SystemTime::now() - ttl * 60,
            ttl,
        )
        .await
        .unwrap();

    let app = build_router(common::build_state_with_cache(cfg, cache).await);

    let res = app
        .oneshot(auth("/rest/getAlbumList2?type=newest&size=10"))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get(WARNING).unwrap(),
        "110 - \"Response is Stale\""
    );
}

/// The fallback must not swallow upstream's own answer. With nothing cached
/// there's nothing honest to substitute, so a 503 reaches the client as a
/// 503 — more informative than flattening every failure to 502.
#[tokio::test]
async fn upstream_5xx_without_a_cached_entry_is_forwarded_verbatim() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&upstream)
        .await;

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let app = build_router(common::build_state_with_cache(cfg, cache).await);

    let res = app
        .oneshot(auth("/rest/getAlbumList2?type=newest&size=10"))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// A refused connection with nothing cached is the one case with no better
/// answer than 502 — there's no upstream status to forward.
#[tokio::test]
async fn unreachable_upstream_without_a_cached_entry_returns_502() {
    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream("http://127.0.0.1:1", "alice", "sesame");
    let app = build_router(common::build_state_with_cache(cfg, cache).await);

    let res = app
        .oneshot(auth("/rest/getAlbumList2?type=newest&size=10"))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
}
