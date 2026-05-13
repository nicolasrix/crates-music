//! `POST /v1/admin/cache/invalidate` — manual flush of the L2 browse
//! cache so newly-added Navidrome content shows up without waiting
//! `browse_ttl_seconds` (default 24 h).
//!
//! Cover-art rows are deliberately preserved — Navidrome's `coverArt`
//! ids are themselves content-addressed, so invalidation is implicit
//! when art changes.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header::AUTHORIZATION};
use bytes::Bytes;
use http_body_util::BodyExt;
use music_cache::Cache;
use music_gateway::build_router;
use tower::ServiceExt;

mod common;

const ROUTE: &str = "/v1/admin/cache/invalidate";

fn auth_post(uri: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {}", common::TEST_BEARER))
        .body(Body::empty())
        .unwrap()
}

async fn seed_mixed(cache: &Cache) {
    let ttl = Duration::from_mins(1);
    for key in [
        "getAlbumList2|size=20|type=newest",
        "getAlbum|id=al-1",
        "getArtists",
        "getArtist|id=ar-1",
        "search3|query=miller",
    ] {
        cache
            .put(key, Bytes::from_static(b"body"), ttl)
            .await
            .unwrap();
    }
    cache
        .put(
            "getCoverArt|id=co-1|size=300",
            Bytes::from_static(b"png"),
            ttl,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn unauthenticated_request_is_rejected() {
    let app = build_router(common::build_state(common::test_config()).await);
    let req = Request::builder()
        .method("POST")
        .uri(ROUTE)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn invalidate_removes_browse_keeps_cover_art_and_reports_count() {
    let cache = Cache::open_in_memory().await.unwrap();
    seed_mixed(&cache).await;

    let state = common::build_state_with_cache(common::test_config(), cache.clone()).await;
    let app = build_router(state);

    let resp = app.oneshot(auth_post(ROUTE)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["removed"].as_u64(), Some(5));

    // Browse rows are gone…
    for key in [
        "getAlbumList2|size=20|type=newest",
        "getAlbum|id=al-1",
        "getArtists",
        "getArtist|id=ar-1",
        "search3|query=miller",
    ] {
        assert!(
            cache.get(key).await.unwrap().is_none(),
            "browse key {key} should be cleared"
        );
    }
    // …but cover art survives.
    assert!(
        cache
            .get("getCoverArt|id=co-1|size=300")
            .await
            .unwrap()
            .is_some(),
        "cover-art entry must survive an invalidate"
    );
}

#[tokio::test]
async fn invalidate_on_empty_cache_returns_zero() {
    let app = build_router(common::build_state(common::test_config()).await);
    let resp = app.oneshot(auth_post(ROUTE)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["removed"].as_u64(), Some(0));
}
