//! `/rest/getCoverArt` proxy + cache + missing-art SVG fallback.
//!
//! Verifies:
//!   - successful covers are buffered, cached, and served without re-hitting upstream;
//!   - cached responses carry `ETag` + `Cache-Control: ... immutable` headers;
//!   - matching `If-None-Match` returns `304`;
//!   - upstream `404` on `getCoverArt` is rewritten to a `200 image/svg+xml`
//!     placeholder so the browser never sees a broken image;
//!   - the cache key includes the `size` query param (different sizes don't collide);
//!   - upstream `5xx` on `getCoverArt` is forwarded verbatim (do not poison the cache).

use std::time::Duration;

use axum::body::Body;
use axum::http::{
    Request, StatusCode,
    header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH},
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

const FAKE_PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR-fake-image-bytes";

#[tokio::test]
async fn cover_art_is_cached_then_served_without_second_upstream_call() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(FAKE_PNG),
        )
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
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=200"))
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);
    let r1_body = r1.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(r1_body.as_ref(), FAKE_PNG);

    let r2 = app
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=200"))
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::OK);
    let r2_body = r2.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(r2_body.as_ref(), FAKE_PNG);
    // wiremock .expect(1) on drop fires if the second call hit upstream.
}

#[tokio::test]
async fn cover_art_response_carries_etag_and_revalidating_cache_control() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(FAKE_PNG),
        )
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
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=200"))
        .await
        .unwrap();
    assert!(response.headers().get(ETAG).is_some(), "ETag must be set");
    let cc = response
        .headers()
        .get(CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(cc.contains("max-age="), "expected max-age, got {cc:?}");
    // We deliberately do *not* send `immutable` — Navidrome can swap a
    // default placeholder for a real cover under the same id, and the
    // gateway can retroactively rewrite cached bodies once placeholder
    // detection fires. Both transitions need to be visible to the
    // browser cache.
    assert!(
        !cc.contains("immutable"),
        "cover-art responses must allow revalidation, got {cc:?}"
    );
    assert!(
        cc.contains("must-revalidate"),
        "expected must-revalidate, got {cc:?}"
    );
}

#[tokio::test]
async fn cover_art_if_none_match_match_returns_304() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(FAKE_PNG),
        )
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
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=200"))
        .await
        .unwrap();
    let etag = r1
        .headers()
        .get(ETAG)
        .expect("etag header")
        .to_str()
        .unwrap()
        .to_string();

    let r2 = app
        .oneshot(auth_if_none_match(
            "/rest/getCoverArt?id=al-1&size=200",
            &etag,
        ))
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn cover_art_size_in_cache_key_so_different_sizes_dont_collide() {
    // Two distinct sizes for the same id must produce two distinct
    // cache entries — otherwise the first request pins all subsequent
    // sizes to whatever Navidrome returned the first time.
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .and(query_param("size", "200"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(b"image-200" as &[u8]),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .and(query_param("size", "600"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(b"image-600" as &[u8]),
        )
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
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=200"))
        .await
        .unwrap();
    let r2 = app
        .clone()
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=600"))
        .await
        .unwrap();
    let b1 = r1.into_body().collect().await.unwrap().to_bytes();
    let b2 = r2.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(b1.as_ref(), b"image-200");
    assert_eq!(b2.as_ref(), b"image-600");
    // Now serve both from cache.
    let r3 = app
        .clone()
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=200"))
        .await
        .unwrap();
    let r4 = app
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=600"))
        .await
        .unwrap();
    let b3 = r3.into_body().collect().await.unwrap().to_bytes();
    let b4 = r4.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(b3.as_ref(), b"image-200");
    assert_eq!(b4.as_ref(), b"image-600");
}

#[tokio::test]
async fn upstream_404_is_rewritten_to_svg_placeholder() {
    // Navidrome returns 404 when no cover file exists for an id.
    // The proxy turns that into a deterministic SVG placeholder so the
    // browser never sees a broken-image icon.
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Not found"))
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
        .oneshot(auth("/rest/getCoverArt?id=al-missing&size=200"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let ct = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        ct.starts_with("image/svg+xml"),
        "expected svg content-type, got {ct:?}"
    );
    // Placeholders must NOT carry the real-art `max-age=300` directive.
    // If they did, a browser that cached the placeholder once would keep
    // showing it for 5 min after the underlying art became available
    // (e.g. user filled in art in Navidrome, or the gateway background-
    // revalidates and gets real bytes). The etag still makes the
    // steady-state revalidation cheap (304).
    let cc = response
        .headers()
        .get(CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        cc.contains("no-cache"),
        "placeholder must use no-cache so flip-to-real-art is visible promptly, got {cc:?}"
    );
    assert!(
        !cc.contains("max-age=300"),
        "placeholder must not use real-art freshness, got {cc:?}"
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();
    assert!(body_str.contains("<svg"), "body should be an SVG");
}

#[tokio::test]
async fn placeholder_for_same_id_is_deterministic() {
    // The placeholder hue is derived from the cover-art id; two requests
    // for the same id yield byte-identical SVGs (so the cache key dedupes
    // and the etag stays stable across cold paths).
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(ResponseTemplate::new(404))
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
        .oneshot(auth("/rest/getCoverArt?id=al-X&size=200"))
        .await
        .unwrap();
    let r2 = app
        .oneshot(auth("/rest/getCoverArt?id=al-X&size=200"))
        .await
        .unwrap();
    let b1 = r1.into_body().collect().await.unwrap().to_bytes();
    let b2 = r2.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(b1, b2, "placeholder must be deterministic");
    let s1 = std::str::from_utf8(&b1).unwrap();
    assert!(s1.contains("<svg"), "expected SVG placeholder body");
}

#[tokio::test]
async fn duplicate_body_for_many_distinct_ids_is_treated_as_placeholder() {
    // Navidrome returns its built-in default placeholder image (same
    // bytes regardless of id) for albums/artists that have no artwork —
    // 200 OK, not 404. The gateway detects this once enough distinct
    // cover-art ids have committed the same bytes (PLACEHOLDER_DUPLICATE_THRESHOLD).
    // A two-album collision would false-positive on legitimate cover
    // sharing (multi-disc sets, deluxe editions, compilations); the
    // threshold raises the bar to "implausibly many albums share this
    // exact body" — characteristic of a hardcoded placeholder.
    let placeholder_body: &[u8] = b"navidrome-default-placeholder-bytes";
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(placeholder_body),
        )
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

    // Threshold semantics: classifier fires when at least N *other*
    // distinct ids share the etag. So priming N ids and requesting the
    // (N+1)th lets the classifier see N others on that final request.
    for i in 1..=5 {
        let r = app
            .clone()
            .oneshot(auth(&format!(
                "/rest/getCoverArt?id=al-{i}&size=200"
            )))
            .await
            .unwrap();
        let b = r.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            b.as_ref(),
            placeholder_body,
            "below threshold (id #{i} sees < threshold other ids) must serve raw upstream bytes",
        );
    }

    // Sixth distinct id: classifier sees five other ids → fires. SVG.
    let r6 = app
        .clone()
        .oneshot(auth("/rest/getCoverArt?id=al-6&size=200"))
        .await
        .unwrap();
    let b6 = r6.into_body().collect().await.unwrap().to_bytes();
    assert!(
        std::str::from_utf8(&b6).unwrap().contains("<svg"),
        "request that sees >= threshold other ids must return SVG, got {b6:?}"
    );

    // Seventh id: already-known placeholder etag (fast path), also SVG.
    let r7 = app
        .oneshot(auth("/rest/getCoverArt?id=al-7&size=200"))
        .await
        .unwrap();
    let b7 = r7.into_body().collect().await.unwrap().to_bytes();
    assert!(std::str::from_utf8(&b7).unwrap().contains("<svg"));
}

#[tokio::test]
async fn small_number_of_albums_sharing_real_art_is_not_classified() {
    // Multi-disc sets, compilations, and deluxe editions of the same
    // album genuinely share cover-art bytes across distinct cover-art
    // ids. With a threshold-of-1 classifier, the second one to load
    // would have its (real!) cover swept and replaced with SVG. The
    // threshold protects against that — fewer than threshold ids
    // sharing the same bytes is presumed to be legitimate.
    let shared_real_art: &[u8] = b"real-album-cover-shared-across-multi-disc-set";
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(shared_real_art),
        )
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

    // Four distinct ids share the same real cover. None should be
    // classified as a placeholder.
    for i in 1..=4 {
        let r = app
            .clone()
            .oneshot(auth(&format!(
                "/rest/getCoverArt?id=al-disc{i}&size=200"
            )))
            .await
            .unwrap();
        let b = r.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            b.as_ref(),
            shared_real_art,
            "disc #{i} of {{1..4}} sharing real art must not be classified",
        );
    }
}

#[tokio::test]
async fn duplicate_body_for_same_id_different_size_is_not_a_placeholder() {
    // Some Subsonic servers return the same body regardless of `size`
    // (Navidrome doesn't always re-thumbnail). Two cache entries with
    // the same etag but the same id must NOT be classified as a
    // placeholder — that'd false-positive on legitimate covers.
    let body: &[u8] = b"real-album-cover-bytes";
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(body),
        )
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
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=200"))
        .await
        .unwrap();
    let r2 = app
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=600"))
        .await
        .unwrap();
    let b2 = r2.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(b2.as_ref(), body, "same id at a different size must not be flagged");
}

#[tokio::test]
async fn detected_placeholder_evicts_existing_cache_entries() {
    // After detection on the second id, requesting the *first* id again
    // must serve the SVG — the cached "real upstream body" entry under
    // id-1 has to be swept so the next fetch re-runs through detection.
    let placeholder_body: &[u8] = b"navidrome-default-placeholder";
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(placeholder_body),
        )
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

    // Prime enough distinct ids that the (N+1)th's classifier sees
    // N other ids and fires; sweep then evicts entries 1..N. The first
    // id is what we then re-request to observe the sweep took effect.
    for i in 1..=6 {
        let _ = app
            .clone()
            .oneshot(auth(&format!(
                "/rest/getCoverArt?id=al-{i}&size=200"
            )))
            .await
            .unwrap();
    }
    // Sweep is fire-and-forget on a tokio task — give it room to commit.
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let r = app
            .clone()
            .oneshot(auth("/rest/getCoverArt?id=al-1&size=200"))
            .await
            .unwrap();
        let b = r.into_body().collect().await.unwrap().to_bytes();
        if std::str::from_utf8(&b).unwrap().contains("<svg") {
            return;
        }
    }
    panic!("id=al-1 never started serving the SVG placeholder after detection");
}

#[tokio::test]
async fn seed_param_drives_placeholder_initial() {
    // The web client passes `?seed=<display name>` so the placeholder's
    // initial reflects the album title / artist name rather than the
    // opaque cover-art id ("ar-abcdef" → "A" for every artist).
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(ResponseTemplate::new(404))
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

    let r = app
        .oneshot(auth(
            "/rest/getCoverArt?id=ar-x&size=200&seed=Pink%20Floyd",
        ))
        .await
        .unwrap();
    let body = r.into_body().collect().await.unwrap().to_bytes();
    let s = std::str::from_utf8(&body).unwrap();
    // The placeholder text element wraps the initial — ">P<" is the
    // closing/opening tag boundary, so >P< means initial is exactly P.
    assert!(s.contains(">P<"), "expected initial 'P' in SVG, got {s:?}");
}

#[tokio::test]
async fn seed_param_is_not_forwarded_to_upstream() {
    // `seed` is a gateway-only hint. Forwarding it to Navidrome could
    // confuse strict implementations; safer to strip it.
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(FAKE_PNG),
        )
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
        .oneshot(auth(
            "/rest/getCoverArt?id=al-1&size=200&seed=Brian%20Eno",
        ))
        .await
        .unwrap();

    let received = upstream.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let q = received[0].url.query().unwrap_or_default();
    assert!(
        !q.contains("seed="),
        "seed must not be forwarded to upstream, query={q:?}"
    );
}

#[tokio::test]
async fn cache_hit_with_placeholder_body_is_substituted_with_svg() {
    // Real-world scenario: on the user's first page load, 60 album
    // covers fetch in parallel against a cold cache. The duplicate
    // check races (no entries committed yet when each request runs
    // it), so all 60 cache as raw upstream bodies. The next visit's
    // requests hit cache — and must classify the etag as a placeholder
    // (now that the duplicates are visible) and substitute the SVG.
    let placeholder_body: &[u8] = b"navidrome-default-placeholder-bytes";
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(placeholder_body),
        )
        .mount(&upstream)
        .await;

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state_with_cache(cfg, cache.clone()).await;
    let app = build_router(state);

    // Pre-seed the cache to simulate the cold-parallel-fetch race
    // outcome. To make the cache-hit request for al-1 see >= threshold
    // *other* ids with the same etag, we need >= threshold + 1 entries
    // total; six gives us five-other-ids visible at classify time.
    let body = Bytes::copy_from_slice(placeholder_body);
    let ttl = Duration::from_hours(24);
    for i in 1..=6 {
        cache
            .put(
                &format!("getCoverArt|id=al-{i}|size=200"),
                body.clone(),
                ttl,
            )
            .await
            .unwrap();
    }

    // A cache-hit request must classify the entry as a placeholder
    // (>= threshold OTHER ids share its etag) and return the SVG, not
    // the cached raw upstream body.
    let r = app
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=200"))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let body = r.into_body().collect().await.unwrap().to_bytes();
    let s = std::str::from_utf8(&body).unwrap();
    assert!(s.contains("<svg"), "expected SVG substitute, got {s:?}");
}

#[tokio::test]
async fn cached_placeholder_svg_is_rerendered_with_current_seed() {
    // Reality: cache may already hold an SVG from a previous request
    // that was made *before* the web client started passing
    // `?seed=<display-name>`. Such an SVG was rendered with the
    // cover-art id as fallback seed, which always starts with "al-…"
    // or "ar-…" → first letter is always "A". A subsequent request
    // *with* a real seed must re-render rather than serve the stale
    // SVG — otherwise every album shows "A" forever.
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(ResponseTemplate::new(404))
        // expect(0): the cache hit path must serve from cache without
        // re-fetching upstream, even when re-rendering.
        .expect(0)
        .mount(&upstream)
        .await;

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state_with_cache(cfg, cache.clone()).await;
    let app = build_router(state);

    // Seed the cache with an SVG that has "A" as its initial — the
    // shape we'd produce for `seed = "al-Paramore_xyz"`.
    let stale_svg = br#"<?xml version="1.0" encoding="UTF-8"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><text>A</text></svg>"#;
    cache
        .put(
            "getCoverArt|id=al-Paramore_xyz|size=400",
            Bytes::copy_from_slice(stale_svg),
            Duration::from_hours(24),
        )
        .await
        .unwrap();

    let r = app
        .oneshot(auth(
            "/rest/getCoverArt?id=al-Paramore_xyz&size=400&seed=Paramore",
        ))
        .await
        .unwrap();
    let body = r.into_body().collect().await.unwrap().to_bytes();
    let s = std::str::from_utf8(&body).unwrap();
    assert!(s.contains(">P<"), "expected re-rendered SVG with 'P', got {s:?}");
    assert!(!s.contains(">A<"), "stale 'A' must not survive re-render");
}

#[tokio::test]
async fn cache_hit_with_real_cover_passes_through_unchanged() {
    // Negative side of the cache-hit classifier: a unique-etag cache
    // entry must not be classified as a placeholder. Real albums look
    // exactly like this — single key per id, etag found nowhere else.
    let real_cover: &[u8] = b"unique-real-album-cover-bytes";
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(real_cover),
        )
        // expect(0) — cache-hit path must not retry upstream.
        .expect(0)
        .mount(&upstream)
        .await;

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state_with_cache(cfg, cache.clone()).await;
    let app = build_router(state);

    cache
        .put(
            "getCoverArt|id=al-real|size=200",
            Bytes::copy_from_slice(real_cover),
            Duration::from_hours(24),
        )
        .await
        .unwrap();

    let r = app
        .oneshot(auth("/rest/getCoverArt?id=al-real&size=200"))
        .await
        .unwrap();
    let body = r.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.as_ref(), real_cover, "real cover must pass through");
}

#[tokio::test]
async fn cached_placeholder_revalidates_against_upstream_real_art() {
    // The user has filled in metadata in Navidrome so an album that
    // previously had no cover now has one. The gateway's cache still
    // holds our SVG placeholder (TTL is 30 days). Each cache hit on
    // the SVG must spawn a background revalidation; if upstream now
    // serves real art, the cache entry is replaced and subsequent
    // requests return the upstream bytes — no operator intervention.
    let real_cover: &[u8] = b"\x89PNG\r\n\x1a\n-real-album-cover-now-tagged";
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(real_cover),
        )
        .mount(&upstream)
        .await;

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state_with_cache(cfg, cache.clone()).await;
    let app = build_router(state);

    // Stale SVG in cache — same shape we'd produce when Navidrome had
    // no art for this id.
    let stale_svg = br#"<?xml version="1.0" encoding="UTF-8"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><text>A</text></svg>"#;
    cache
        .put(
            "getCoverArt|id=al-fixed|size=200",
            Bytes::copy_from_slice(stale_svg),
            Duration::from_hours(24),
        )
        .await
        .unwrap();

    // First request must serve the cached SVG immediately (does not
    // block on upstream).
    let r1 = app
        .clone()
        .oneshot(auth(
            "/rest/getCoverArt?id=al-fixed&size=200&seed=Paramore",
        ))
        .await
        .unwrap();
    let b1 = r1.into_body().collect().await.unwrap().to_bytes();
    assert!(
        std::str::from_utf8(&b1).unwrap().contains("<svg"),
        "first request must still serve SVG (revalidation is async)"
    );

    // Background revalidation must replace the cached SVG with upstream
    // real art. Poll until subsequent requests return real bytes.
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let r = app
            .clone()
            .oneshot(auth(
                "/rest/getCoverArt?id=al-fixed&size=200&seed=Paramore",
            ))
            .await
            .unwrap();
        let body = r.into_body().collect().await.unwrap().to_bytes();
        if body.as_ref() == real_cover {
            return;
        }
    }
    panic!("placeholder was never replaced with upstream real art");
}

#[tokio::test]
async fn revalidation_keeps_svg_when_upstream_still_serves_placeholder() {
    // Conservative path: when upstream returns bytes that match a
    // *different* cached cover-art id (i.e. the duplicate-classifier
    // would call it a placeholder), revalidation must NOT replace our
    // SVG with those bytes. Otherwise we'd regress from a clean SVG
    // back to Navidrome's "no artwork" image.
    let placeholder_bytes: &[u8] = b"navidrome-default-placeholder-bytes";
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(placeholder_bytes),
        )
        .mount(&upstream)
        .await;

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state_with_cache(cfg, cache.clone()).await;
    let app = build_router(state);

    // Pre-seed: SVG for the id we'll request, plus enough OTHER ids
    // sharing the placeholder bytes that the duplicate-classifier
    // (threshold-N) fires when revalidation fetches them from upstream.
    let stale_svg = br#"<?xml version="1.0" encoding="UTF-8"?><svg xmlns="http://www.w3.org/2000/svg"><text>A</text></svg>"#;
    cache
        .put(
            "getCoverArt|id=al-target|size=200",
            Bytes::copy_from_slice(stale_svg),
            Duration::from_hours(24),
        )
        .await
        .unwrap();
    for i in 1..=5 {
        cache
            .put(
                &format!("getCoverArt|id=al-other-{i}|size=200"),
                Bytes::copy_from_slice(placeholder_bytes),
                Duration::from_hours(24),
            )
            .await
            .unwrap();
    }

    // First request: cache hit → SVG. Background revalidation fires.
    let r1 = app
        .clone()
        .oneshot(auth(
            "/rest/getCoverArt?id=al-target&size=200&seed=Foo",
        ))
        .await
        .unwrap();
    let b1 = r1.into_body().collect().await.unwrap().to_bytes();
    assert!(std::str::from_utf8(&b1).unwrap().contains("<svg"));

    // Give the background task time to fetch + classify + decide-not-to-write.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Subsequent requests must STILL serve SVG — the revalidation must
    // have detected the upstream bytes as a placeholder and left the
    // cached SVG alone.
    let r2 = app
        .clone()
        .oneshot(auth(
            "/rest/getCoverArt?id=al-target&size=200&seed=Foo",
        ))
        .await
        .unwrap();
    let b2 = r2.into_body().collect().await.unwrap().to_bytes();
    let s2 = std::str::from_utf8(&b2).unwrap();
    assert!(
        s2.contains("<svg"),
        "revalidation must not replace SVG with bytes that classify as placeholder, got {s2:?}"
    );
}

#[tokio::test]
async fn revalidation_is_rate_limited_per_key() {
    // Revalidation must coalesce: a burst of cache-hit requests for
    // the same cover-art id within the cooldown window must trigger
    // at most one upstream fetch. Without this, every page reload of
    // a 60-album view would issue 60 background requests on top of
    // the 60 cache-hit responses.
    let real_cover: &[u8] = b"\x89PNG\r\n\x1a\n-revalidated-art";
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(real_cover),
        )
        .expect(1) // ← the assertion: exactly one upstream call across the burst
        .mount(&upstream)
        .await;

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state_with_cache(cfg, cache.clone()).await;
    let app = build_router(state);

    let stale_svg = br#"<?xml version="1.0" encoding="UTF-8"?><svg xmlns="http://www.w3.org/2000/svg"><text>A</text></svg>"#;
    cache
        .put(
            "getCoverArt|id=al-burst|size=200",
            Bytes::copy_from_slice(stale_svg),
            Duration::from_hours(24),
        )
        .await
        .unwrap();

    // 10 rapid cache-hit requests for the same id.
    for _ in 0..10 {
        let _ = app
            .clone()
            .oneshot(auth(
                "/rest/getCoverArt?id=al-burst&size=200&seed=Burst",
            ))
            .await
            .unwrap();
    }
    // Settle period for in-flight tasks to drain.
    tokio::time::sleep(Duration::from_millis(300)).await;
    // wiremock .expect(1) on drop fails if any spawn issued more than one upstream GET.
}

#[tokio::test]
async fn subsonic_json_error_is_treated_as_no_art() {
    // Subsonic surfaces "no artwork for this id" as HTTP 200 +
    // `application/json` with `subsonic-response.status = "failed"`,
    // not as 404. Common after re-tagging metadata in Navidrome:
    // cached cover-art ids become stale and Navidrome no longer
    // recognises them. The proxy must treat that as no-art and
    // substitute our SVG, not cache the JSON-error bytes as if they
    // were image data.
    let json_error = br#"{"subsonic-response":{"status":"failed","version":"1.16.1","type":"navidrome","error":{"code":70,"message":"Artwork not found"}}}"#;
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_bytes(json_error as &[u8]),
        )
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

    let r = app
        .oneshot(auth(
            "/rest/getCoverArt?id=al-stale-id&size=200&seed=Brand%20New%20Eyes",
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let ct = r
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        ct.starts_with("image/svg+xml"),
        "expected SVG content-type, got {ct:?}"
    );
    let body = r.into_body().collect().await.unwrap().to_bytes();
    let s = std::str::from_utf8(&body).unwrap();
    assert!(s.contains("<svg"), "expected SVG body, got {s:?}");
    assert!(
        s.contains(">B<"),
        "seed initial 'B' should drive the placeholder, got {s:?}"
    );
}

#[tokio::test]
async fn revalidation_keeps_svg_when_upstream_returns_json_error() {
    // After the user re-tags metadata in Navidrome, cached cover-art
    // ids become stale. Background revalidation will hit upstream and
    // get a Subsonic JSON error (HTTP 200 + application/json + error
    // code 70). It must NOT replace our cached SVG with that JSON.
    let json_error = br#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":70,"message":"Artwork not found"}}}"#;
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_bytes(json_error as &[u8]),
        )
        .mount(&upstream)
        .await;

    let cache = Cache::open_in_memory().await.unwrap();
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state_with_cache(cfg, cache.clone()).await;
    let app = build_router(state);

    let stale_svg = br#"<?xml version="1.0" encoding="UTF-8"?><svg xmlns="http://www.w3.org/2000/svg"><text>B</text></svg>"#;
    cache
        .put(
            "getCoverArt|id=al-stale-id|size=200",
            Bytes::copy_from_slice(stale_svg),
            Duration::from_hours(24),
        )
        .await
        .unwrap();

    // First request: SVG cache hit → serves SVG, spawns revalidation.
    let r1 = app
        .clone()
        .oneshot(auth(
            "/rest/getCoverArt?id=al-stale-id&size=200&seed=Brand%20New%20Eyes",
        ))
        .await
        .unwrap();
    let b1 = r1.into_body().collect().await.unwrap().to_bytes();
    assert!(std::str::from_utf8(&b1).unwrap().contains("<svg"));

    // Settle window for the background fetch to come back with JSON error.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Subsequent request must STILL be SVG — JSON-error must not have
    // poisoned the cache.
    let r2 = app
        .clone()
        .oneshot(auth(
            "/rest/getCoverArt?id=al-stale-id&size=200&seed=Brand%20New%20Eyes",
        ))
        .await
        .unwrap();
    let b2 = r2.into_body().collect().await.unwrap().to_bytes();
    let s2 = std::str::from_utf8(&b2).unwrap();
    assert!(
        s2.contains("<svg"),
        "JSON-error must not replace SVG, got {s2:?}"
    );
}

#[tokio::test]
async fn upstream_5xx_is_forwarded_and_not_cached() {
    // A transient upstream error must NOT poison the cache with a
    // sentinel placeholder — that would mask Navidrome recoveries.
    // Instead, forward the error so the client (or a later retry)
    // sees the real status and we re-hit upstream next time.
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getCoverArt"))
        .respond_with(ResponseTemplate::new(503))
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

    let r1 = app
        .clone()
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=200"))
        .await
        .unwrap();
    let r2 = app
        .oneshot(auth("/rest/getCoverArt?id=al-1&size=200"))
        .await
        .unwrap();
    // Both requests must reach upstream — the second can't be served
    // from a cached error. Status doesn't have to be 503 verbatim
    // (we may opt to coerce to 502 BAD_GATEWAY) but it must be an error.
    assert!(r1.status().is_server_error() || r1.status() == StatusCode::BAD_GATEWAY);
    assert!(r2.status().is_server_error() || r2.status() == StatusCode::BAD_GATEWAY);
}
