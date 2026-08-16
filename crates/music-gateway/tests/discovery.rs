//! Catalog discovery — automatic enqueue of newly-added Navidrome tracks.
//!
//! Covers both scan tiers against a mock Navidrome — the recent scan
//! (`getAlbumList2?type=newest` → `getAlbum` expansion) and the full
//! sweep (paged empty-query `search3`) — plus the idempotence property
//! the whole design rests on (re-offering the catalog every tick must
//! not re-queue anything), and the admin-triggered sweep.

use axum::body::Body;
use axum::http::{Request, StatusCode, header::AUTHORIZATION};
use http_body_util::BodyExt;
use music_core::TrackId;
use music_gateway::build_router;
use music_gateway::discovery::CatalogWatcher;
use music_recommend::store::EmbeddingStore;
use music_recommend::types::{Embedding, EmbeddingKey, ModelVersion};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{method as m_method, path as m_path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;

const MODEL: &str = "clamp3-v1";

fn model() -> ModelVersion {
    ModelVersion::from(MODEL)
}

fn ok_envelope(payload: &Value) -> Value {
    let mut body = json!({"status": "ok", "version": "1.16.1"});
    let obj = body.as_object_mut().unwrap();
    for (k, v) in payload.as_object().unwrap() {
        obj.insert(k.clone(), v.clone());
    }
    json!({ "subsonic-response": body })
}

fn song(id: &str) -> Value {
    json!({
        "id": id,
        "title": format!("Song {id}"),
        "album": "An Album",
        "artist": "An Artist",
        "duration": 200
    })
}

/// Mount a `search3` mock returning `ids` as the first page. The sweep
/// stops on the first short page, so one page under the 500 page size
/// terminates the walk — no need to mock pagination.
async fn mount_search3(upstream: &MockServer, ids: &[&str]) {
    let songs: Vec<Value> = ids.iter().map(|id| song(id)).collect();
    Mock::given(m_method("GET"))
        .and(m_path("/rest/search3"))
        .and(query_param("songOffset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_envelope(&json!({
            "searchResult3": {"artist": [], "album": [], "song": songs}
        }))))
        .mount(upstream)
        .await;
}

fn watcher_for(upstream: &MockServer, store: EmbeddingStore) -> CatalogWatcher {
    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    CatalogWatcher::new(&cfg.upstream, store, model(), cfg.discovery.recent_albums)
        .expect("build watcher")
}

#[tokio::test]
async fn recent_scan_enqueues_tracks_from_newly_added_albums() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_envelope(&json!({
            "albumList2": {"album": [{"id": "al-new", "name": "Fresh", "artist": "An Artist"}]}
        }))))
        .mount(&upstream)
        .await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbum"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_envelope(&json!({
            "album": {
                "id": "al-new",
                "name": "Fresh",
                "artist": "An Artist",
                "song": [song("t1"), song("t2")]
            }
        }))))
        .mount(&upstream)
        .await;

    let store = EmbeddingStore::open_in_memory().await.unwrap();
    let watcher = watcher_for(&upstream, store.clone());

    let stats = watcher.scan_recent().await.expect("scan");
    assert_eq!(stats.seen, 2);
    assert_eq!(stats.enqueued, 2, "both tracks of the new album are queued");
    assert_eq!(store.counts(&model()).await.unwrap().not_started, 2);
}

#[tokio::test]
async fn full_sweep_enqueues_the_whole_catalog_then_goes_quiet() {
    let upstream = MockServer::start().await;
    mount_search3(&upstream, &["t1", "t2", "t3"]).await;

    let store = EmbeddingStore::open_in_memory().await.unwrap();
    let watcher = watcher_for(&upstream, store.clone());

    let first = watcher.scan_full().await.expect("first sweep");
    assert_eq!(first.enqueued, 3, "fresh install self-seeds");

    // The steady state: the sweep re-offers the same catalog every day
    // and must queue nothing. If this ever regresses, every track gets
    // re-embedded on a loop.
    let second = watcher.scan_full().await.expect("second sweep");
    assert_eq!(second.seen, 3);
    assert_eq!(second.enqueued, 0, "known tracks must not be re-queued");
    assert_eq!(store.counts(&model()).await.unwrap().not_started, 3);
}

#[tokio::test]
async fn sweep_leaves_already_embedded_tracks_alone() {
    let upstream = MockServer::start().await;
    mount_search3(&upstream, &["t1", "t2"]).await;

    let store = EmbeddingStore::open_in_memory().await.unwrap();
    // `t1` is already embedded from an earlier run.
    let k = EmbeddingKey::new(TrackId::from("t1"), model());
    store.enqueue(&k).await.unwrap();
    store.claim_next(&model()).await.unwrap();
    store
        .mark_done(&Embedding::new(k, vec![0.1, 0.2]))
        .await
        .unwrap();

    let watcher = watcher_for(&upstream, store.clone());
    let stats = watcher.scan_full().await.expect("sweep");

    assert_eq!(stats.enqueued, 1, "only the unseen track is queued");
    let counts = store.counts(&model()).await.unwrap();
    assert_eq!(counts.done, 1);
    assert_eq!(counts.not_started, 1);
}

#[tokio::test]
async fn a_failed_album_expansion_does_not_sink_the_recent_scan() {
    let upstream = MockServer::start().await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbumList2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_envelope(&json!({
            "albumList2": {"album": [{"id": "al-bad", "name": "Broken", "artist": "X"}]}
        }))))
        .mount(&upstream)
        .await;
    Mock::given(m_method("GET"))
        .and(m_path("/rest/getAlbum"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&upstream)
        .await;

    let store = EmbeddingStore::open_in_memory().await.unwrap();
    let watcher = watcher_for(&upstream, store.clone());

    // Skipped, not fatal — the next tick and the full sweep both retry.
    let stats = watcher.scan_recent().await.expect("scan survives one bad album");
    assert_eq!(stats.seen, 0);
    assert_eq!(stats.enqueued, 0);
}

#[tokio::test]
async fn admin_scan_endpoint_reports_what_it_queued() {
    let upstream = MockServer::start().await;
    mount_search3(&upstream, &["t1", "t2"]).await;

    let cfg = common::test_config_with_upstream(&upstream.uri(), "alice", "sesame");
    let state = common::build_state(cfg).await;
    let store = state.embedding_store().clone();
    let model_version = state.recommend_model_version().clone();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/discovery/scan")
                .header(AUTHORIZATION, format!("Bearer {}", common::TEST_BEARER))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["seen"], 2);
    assert_eq!(json["enqueued"], 2);
    assert_eq!(store.counts(&model_version).await.unwrap().not_started, 2);
}

/// Regression: a gateway that booted while the embedder was down has no
/// model identity, and must refuse to enqueue rather than stamp rows with
/// a placeholder.
///
/// This is the 2026-08-15 incident in miniature. A restart during an
/// embedder outage latched the literal `"default"` as the model_version;
/// the boot sweep then saw an empty queue under that brand-new key,
/// enqueued all ~7.8k catalog tracks, and the ingest workers re-embedded
/// the entire library overnight into a bucket nothing else read.
#[tokio::test]
async fn admin_scan_refuses_when_model_version_unknown() {
    let state = common::build_state_unknown_model(common::test_config()).await;
    let store = state.embedding_store().clone();
    let sentinel = state.recommend_model_version().clone();
    assert!(
        !state.recommend_writes_enabled(),
        "an unreachable embedder must leave writes disabled"
    );
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/admin/discovery/scan")
                .header(AUTHORIZATION, format!("Bearer {}", common::TEST_BEARER))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    // Assert on the guard's own wording: this test runs against an
    // unreachable upstream, so a bare 503 could come from the sweep
    // failing rather than from the guard refusing to start it.
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        String::from_utf8_lossy(&body).contains("model_version unknown"),
        "503 must come from the unknown-model guard, not an upstream failure"
    );
    let counts = store.counts(&sentinel).await.unwrap();
    assert_eq!(
        counts.not_started, 0,
        "no rows may be queued under the unknown-model sentinel"
    );
}

/// The manual enqueue endpoint is the other `model_version`-keyed write
/// path, and needs the same guard.
#[tokio::test]
async fn enqueue_endpoint_refuses_when_model_version_unknown() {
    let state = common::build_state_unknown_model(common::test_config()).await;
    let store = state.embedding_store().clone();
    let sentinel = state.recommend_model_version().clone();
    let app = build_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/recommend/enqueue")
                .header(AUTHORIZATION, format!("Bearer {}", common::TEST_BEARER))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"track_ids":["t1","t2"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        String::from_utf8_lossy(&body).contains("model_version unknown"),
        "503 must come from the unknown-model guard"
    );
    assert_eq!(store.counts(&sentinel).await.unwrap().not_started, 0);
}
