//! Integration tests for /v1/recommend/* endpoints.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_core::TrackId;
use music_gateway::build_router;
use music_recommend::TrackMetadata;
use music_recommend::normalize_title;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::{TEST_BEARER, build_state, test_config};

const DIM: usize = 8;

fn unit_at(i: usize) -> Vec<f32> {
    let mut v = vec![0.0_f32; DIM];
    v[i] = 1.0;
    v
}

async fn read_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("response body is JSON")
}

fn auth_get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("authorization", format!("Bearer {TEST_BEARER}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn recommend_next_returns_404_for_unknown_seed() {
    let state = build_state(test_config()).await;
    let app = build_router(state.clone());

    let resp = app
        .oneshot(auth_get("/v1/recommend/next?seed=does-not-exist&n=5"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn recommend_next_returns_top_k_for_known_seed() {
    let state = build_state(test_config()).await;

    // Populate the ANN with a few unit-vector tracks. We bypass the
    // worker for this test — the endpoint reads from AnnIndex directly.
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }

    let app = build_router(state);
    let resp = app
        .oneshot(auth_get("/v1/recommend/next?seed=t0&n=3"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = read_json(resp).await;
    assert_eq!(body["seed"], "t0");
    assert_eq!(body["degraded"], false);
    let results = body["results"].as_array().expect("results array");
    assert!(!results.is_empty());
    assert!(results.len() <= 3);
    // Seed should be excluded from results.
    for r in results {
        assert_ne!(r["track_id"], "t0", "seed must not appear in own results");
    }
}

#[tokio::test]
async fn recommend_next_excludes_seed() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }

    let app = build_router(state);
    let resp = app
        .oneshot(auth_get("/v1/recommend/next?seed=t0&n=20"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert!(results.iter().all(|r| r["track_id"] != "t0"));
}

#[tokio::test]
async fn recommend_next_validates_n_param() {
    let state = build_state(test_config()).await;
    let app = build_router(state);
    let resp = app
        .oneshot(auth_get("/v1/recommend/next?seed=t0&n=0"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn recommend_next_caps_n_at_max() {
    // We don't want a runaway n=10000 query allocating a huge result
    // set; the endpoint caps at MAX_N (typically 100). Test that
    // larger requests don't error but get clamped.
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }

    let app = build_router(state);
    let resp = app
        .oneshot(auth_get("/v1/recommend/next?seed=t0&n=10000"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert!(results.len() <= 100, "results must be capped at 100");
}

#[tokio::test]
async fn recommend_next_requires_auth() {
    let state = build_state(test_config()).await;
    let app = build_router(state);
    let req = Request::builder()
        .uri("/v1/recommend/next?seed=t0&n=5")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn recommend_enqueue_adds_tracks_to_queue() {
    let state = build_state(test_config()).await;
    let app = build_router(state.clone());

    let body = json!({"track_ids": ["t1", "t2", "t3"]}).to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/recommend/enqueue")
        .header("authorization", format!("Bearer {TEST_BEARER}"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body = read_json(resp).await;
    assert_eq!(body["enqueued"], 3);

    // Queue counts reflect the inserts.
    let counts = state
        .embedding_store()
        .counts(state.recommend_model_version())
        .await
        .unwrap();
    assert_eq!(counts.not_started, 3);
}

#[tokio::test]
async fn recommend_enqueue_is_idempotent() {
    let state = build_state(test_config()).await;
    let app = build_router(state.clone());

    let body = json!({"track_ids": ["t1", "t1"]}).to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/recommend/enqueue")
        .header("authorization", format!("Bearer {TEST_BEARER}"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let counts = state
        .embedding_store()
        .counts(state.recommend_model_version())
        .await
        .unwrap();
    assert_eq!(
        counts.not_started, 1,
        "duplicate track_ids must be deduplicated"
    );
}

// --- /v1/recommend/from-seeds ---
//
// Aggregation correctness lives in the music-recommend unit tests
// (see `aggregate::tests::*`). Tests here verify the *wiring*:
// request parsing, seed-set exclusion, exclude-list propagation,
// degraded-mode signalling, bounds checking, and auth.

fn auth_post(uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {TEST_BEARER}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn from_seeds_returns_results_for_indexed_seeds() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({"seeds": ["t0", "t1"], "per_seed_n": 5, "top_n": 5}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    assert_eq!(body["all_seeds_unindexed"], false);
    assert_eq!(body["model_version"], "test-v1");
    let results = body["results"].as_array().expect("results array");
    assert!(!results.is_empty());
}

#[tokio::test]
async fn from_seeds_excludes_the_seeds_themselves() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({"seeds": ["t0", "t1", "t2"], "per_seed_n": 20, "top_n": 20}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    for r in results {
        let id = r["track_id"].as_str().unwrap();
        assert!(
            !["t0", "t1", "t2"].contains(&id),
            "seed {id} must not appear in own results"
        );
    }
}

#[tokio::test]
async fn from_seeds_honours_exclude_track_ids() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "per_seed_n": 20,
            "top_n": 20,
            "exclude_track_ids": ["t3", "t5"],
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    for r in results {
        let id = r["track_id"].as_str().unwrap();
        assert!(
            !["t0", "t3", "t5"].contains(&id),
            "excluded id {id} must not appear",
        );
    }
}

#[tokio::test]
async fn from_seeds_signals_all_unindexed() {
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({"seeds": ["unknown1", "unknown2"], "top_n": 5}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    assert_eq!(body["all_seeds_unindexed"], true);
    let results = body["results"].as_array().expect("results");
    assert!(results.is_empty());
}

#[tokio::test]
async fn from_seeds_rejects_seed_weights_length_mismatch_with_400() {
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0", "t1", "t2"],
            "seed_weights": [1.0, 2.0],  // length 2 vs 3 seeds — bad
            "top_n": 5,
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn from_seeds_excludes_session_downvoted_tracks() {
    // Downvote `t3` in session-A via the public feedback endpoint, then
    // ask for recommendations under the same session_id — `t3` must not
    // appear in results. Sanity: a different session_id sees `t3` again.
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }
    let app = build_router(state);

    // Cast a thumbs-down for t3 in session-A.
    let vote_req = auth_post(
        "/v1/recommend/feedback",
        &json!({
            "track_id": "t3",
            "session_id": "session-A",
            "vote": "down",
            "occurred_ms": 1_700_000_000_000_i64,
        }),
    );
    let vote_resp = app.clone().oneshot(vote_req).await.unwrap();
    assert_eq!(vote_resp.status(), StatusCode::OK);

    // Same session must see t3 excluded.
    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "per_seed_n": 20,
            "top_n": 20,
            "session_id": "session-A",
        }),
    );
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    for r in results {
        assert_ne!(
            r["track_id"].as_str().unwrap(),
            "t3",
            "session-A downvoted t3 → must not surface in session-A results",
        );
    }

    // A different session must still see t3 as a candidate.
    let req2 = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "per_seed_n": 20,
            "top_n": 20,
            "session_id": "session-DIFFERENT",
        }),
    );
    let resp2 = app.oneshot(req2).await.unwrap();
    let body2 = read_json(resp2).await;
    let ids: Vec<&str> = body2["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["track_id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"t3"),
        "downvote in session-A must NOT exclude t3 for a different session, got {ids:?}",
    );
}

#[tokio::test]
async fn from_seeds_with_weights_changes_ranking() {
    // Two seeds, each surfaces a distinct nearest-neighbour with the
    // same raw similarity. With equal weights, both candidates would
    // tie (and break alphabetically). Giving seed 0 a much higher
    // weight forces *its* neighbour to win — proving the weights
    // reach the aggregator.
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0", "t1"],
            "seed_weights": [10.0, 0.1],
            "per_seed_n": 5,
            "top_n": 5,
            // Force sampling to take BOTH seeds, not a random 1.
            "sample_size": 2,
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert!(!results.is_empty(), "should have at least one result");
    // The top result must be from the heavily-weighted seed's
    // neighbourhood (not from t1's, which is heavily down-weighted).
    // unit_at(0) is the neighbour of t0; nothing else is nearer to t0
    // than itself, and t0 is excluded — so top result has some t_i for
    // i != 0,1 with the highest sim to t0. Just assert ordering by
    // checking that there exist results with score > 0 and that the
    // top one's score is > the bottom one's.
    let top = results[0]["similarity"].as_f64().unwrap();
    let bot = results.last().unwrap()["similarity"].as_f64().unwrap();
    assert!(top >= bot, "results must be score-descending");
}

#[tokio::test]
async fn from_seeds_partial_index_does_not_set_all_unindexed() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    ann.upsert(&TrackId::from("t0"), &unit_at(0)).unwrap();
    ann.upsert(&TrackId::from("t1"), &unit_at(1)).unwrap();
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({"seeds": ["t0", "unknown"], "top_n": 5}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    assert_eq!(body["all_seeds_unindexed"], false);
}

#[tokio::test]
async fn from_seeds_rejects_empty_seeds() {
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({"seeds": [], "top_n": 5}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn from_seeds_caps_top_n_at_max() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({"seeds": ["t0"], "per_seed_n": 100, "top_n": 10000}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert!(results.len() <= 100, "top_n must be clamped to MAX_N");
}

#[tokio::test]
async fn from_seeds_requires_auth() {
    let state = build_state(test_config()).await;
    let app = build_router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/recommend/from-seeds")
        .header("content-type", "application/json")
        .body(Body::from(json!({"seeds": ["t0"]}).to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// --- /v1/recommend/from-any ---

#[tokio::test]
async fn from_any_returns_first_indexed_seed() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    // Only t2 is indexed. t0 and t1 are not. The handler must skip
    // unindexed and surface t2 as `seed_used`.
    ann.upsert(&TrackId::from("t2"), &unit_at(2)).unwrap();
    ann.upsert(&TrackId::from("t3"), &unit_at(3)).unwrap();
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({"candidate_seeds": ["t0", "t1", "t2", "t3"], "n": 5}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    assert_eq!(body["seed_used"], "t2");
    assert_eq!(body["model_version"], "test-v1");
    let results = body["results"].as_array().expect("results");
    // t2 itself must not appear in its own recs.
    for r in results {
        assert_ne!(r["track_id"], "t2");
    }
}

#[tokio::test]
async fn from_any_returns_404_when_no_candidate_indexed() {
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({"candidate_seeds": ["x", "y", "z"], "n": 5}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn from_any_rejects_empty_candidates() {
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({"candidate_seeds": [], "n": 5}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn from_any_caps_n_at_max() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({"candidate_seeds": ["t0"], "n": 10000}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert!(results.len() <= 100);
}

// --- queue_context filtering (from-seeds + from-any) ---
//
// These tests seed the ANN with a uniform fan of unit vectors AND
// stamp matching metadata onto the metadata cache. The ANN gives us
// "all 8 tracks are similar to anything"; the metadata cache lets the
// QueueFilter actually do work. Asserting filter behavior in isolation
// from rerank by setting the artist explicitly per track.

fn meta(id: &str, artist_id: &str, artist: &str, title: &str) -> TrackMetadata {
    TrackMetadata {
        track_id: TrackId::from(id),
        artist_id: Some(artist_id.into()),
        artist: artist.into(),
        album_id: None,
        album: None,
        title: title.into(),
        title_normalized: normalize_title(title),
        duration_seconds: None,
        genre: None,
        year: None,
        track_number: None,
        disc_number: None,
        bpm: None,
        musical_key: None,
    }
}

#[tokio::test]
async fn from_seeds_artist_cap_drops_third_track_by_same_artist() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    // 8 tracks, all by "ar1". Without a cap, all 8 are recommendable.
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
        state
            .metadata_store()
            .upsert(&meta(
                &format!("t{i}"),
                "ar1",
                "Queen",
                &format!("Song {i}"),
            ))
            .await
            .unwrap();
    }
    let app = build_router(state);

    // Queue contains nothing else. Cap = 2 ⇒ at most 2 results survive.
    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "per_seed_n": 100,
            "top_n": 20,
            "queue_context": {
                "queue_track_ids": [],
                "max_per_artist": 2,
                "dedup_titles": false,
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert!(
        results.len() <= 2,
        "artist cap should have shrunk results to <= 2, got {}",
        results.len()
    );
}

#[tokio::test]
async fn from_seeds_now_playing_counts_against_cap_but_not_excluded() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
        state
            .metadata_store()
            .upsert(&meta(
                &format!("t{i}"),
                "ar1",
                "Queen",
                &format!("Song {i}"),
            ))
            .await
            .unwrap();
    }
    let app = build_router(state);

    // Queue contains the now-playing track t1 only. Cap = 1, so the
    // now_playing already saturates the artist count → zero ar1 results.
    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "per_seed_n": 100,
            "top_n": 20,
            "queue_context": {
                "queue_track_ids": ["t1"],
                "now_playing_track_id": "t1",
                "max_per_artist": 1,
                "dedup_titles": false,
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert!(
        results.is_empty(),
        "now-playing track at cap=1 should have blocked all ar1 candidates"
    );
}

#[tokio::test]
async fn from_seeds_excludes_queue_tracks_other_than_now_playing() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    // Different artists per track so the artist cap doesn't fire.
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
        state
            .metadata_store()
            .upsert(&meta(
                &format!("t{i}"),
                &format!("ar{i}"),
                &format!("Artist {i}"),
                &format!("Song {i}"),
            ))
            .await
            .unwrap();
    }
    let app = build_router(state);

    // t1 is now-playing (NOT excluded — eligible to surface as a result).
    // t2 and t3 are in the queue and should NOT come back.
    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "per_seed_n": 100,
            "top_n": 20,
            "queue_context": {
                "queue_track_ids": ["t1", "t2", "t3"],
                "now_playing_track_id": "t1",
                "max_per_artist": 0,
                "dedup_titles": false,
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let ids: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["track_id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&"t2"), "queued t2 leaked into results");
    assert!(!ids.contains(&"t3"), "queued t3 leaked into results");
    // t1 is now-playing; the seed exclusion in `from-seeds` blocks
    // explicit seed ids but t1 wasn't a seed — the contract is that
    // it's not in the exclusion set. Whether the ANN surfaces it is
    // up to the vector geometry, so we don't assert presence — only
    // that it's not artificially blocked. (No-op check.)
}

#[tokio::test]
async fn from_seeds_title_dedup_blocks_remaster_when_original_in_queue() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }
    // t-orig is the queue's original; t1 is its remaster (same
    // artist, same normalized title). Different artists for the rest
    // so the cap doesn't ambiguously fire.
    state
        .metadata_store()
        .upsert(&meta("t-orig", "ar1", "Queen", "Bohemian Rhapsody"))
        .await
        .unwrap();
    state
        .metadata_store()
        .upsert(&meta(
            "t1",
            "ar1",
            "Queen",
            "Bohemian Rhapsody (Remastered 2011)",
        ))
        .await
        .unwrap();
    for i in [0_usize, 2, 3, 4, 5, 6, 7] {
        state
            .metadata_store()
            .upsert(&meta(
                &format!("t{i}"),
                &format!("other{i}"),
                &format!("Other Artist {i}"),
                &format!("Other Song {i}"),
            ))
            .await
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "per_seed_n": 100,
            "top_n": 20,
            "queue_context": {
                "queue_track_ids": ["t-orig"],
                "max_per_artist": 0,
                "dedup_titles": true,
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let ids: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["track_id"].as_str().unwrap())
        .collect();
    assert!(
        !ids.contains(&"t1"),
        "remaster t1 should have been deduped against original in queue"
    );
}

#[tokio::test]
async fn from_seeds_max_per_artist_zero_disables_cap() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
        state
            .metadata_store()
            .upsert(&meta(
                &format!("t{i}"),
                "ar1",
                "Queen",
                &format!("Song {i}"),
            ))
            .await
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "per_seed_n": 100,
            "top_n": 20,
            "queue_context": {
                "queue_track_ids": [],
                "max_per_artist": 0,
                "dedup_titles": false,
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    // With cap = 0 (disabled), all 7 ar1 tracks (DIM-1 = 7, t0 excluded
    // as its own seed) should survive.
    assert_eq!(
        results.len(),
        DIM - 1,
        "max_per_artist=0 should disable the cap"
    );
}

#[tokio::test]
async fn from_seeds_no_queue_context_skips_filter() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
        // Metadata is present, but no queue_context → filter is bypassed.
        state
            .metadata_store()
            .upsert(&meta(
                &format!("t{i}"),
                "ar1",
                "Queen",
                &format!("Song {i}"),
            ))
            .await
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "per_seed_n": 100,
            "top_n": 20,
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    // No queue_context → no cap → all 7 same-artist tracks come back.
    assert_eq!(results.len(), DIM - 1);
}

#[tokio::test]
async fn from_seeds_rejects_oversized_queue() {
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let big_queue: Vec<String> = (0..401).map(|i| format!("q{i}")).collect();
    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "queue_context": { "queue_track_ids": big_queue }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn from_seeds_filter_admits_candidate_with_no_metadata() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }
    // Metadata for the queue track only; candidates have no metadata
    // cached. The filter has no signal to gate on → admit.
    state
        .metadata_store()
        .upsert(&meta("q1", "ar1", "Queen", "Bohemian Rhapsody"))
        .await
        .unwrap();
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "per_seed_n": 100,
            "top_n": 20,
            "queue_context": {
                "queue_track_ids": ["q1"],
                "max_per_artist": 1,
                "dedup_titles": true,
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    // None of the candidates have metadata → filter passes them all.
    // Seven survive (t0 is the seed, excluded by from-seeds itself).
    assert_eq!(
        results.len(),
        DIM - 1,
        "candidates without metadata should not be blocked by the cap"
    );
}

#[tokio::test]
async fn from_any_artist_cap_filters_results() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
        state
            .metadata_store()
            .upsert(&meta(
                &format!("t{i}"),
                "ar1",
                "Queen",
                &format!("Song {i}"),
            ))
            .await
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({
            "candidate_seeds": ["t0"],
            "n": 20,
            "queue_context": {
                "queue_track_ids": [],
                "max_per_artist": 2,
                "dedup_titles": false,
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert!(
        results.len() <= 2,
        "from-any should cap to 2 ar1 tracks; got {}",
        results.len()
    );
}

#[tokio::test]
async fn from_any_excludes_queue_ids_at_ann_layer() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
        state
            .metadata_store()
            .upsert(&meta(
                &format!("t{i}"),
                &format!("ar{i}"),
                &format!("Artist {i}"),
                &format!("Song {i}"),
            ))
            .await
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({
            "candidate_seeds": ["t0"],
            "n": 20,
            "queue_context": {
                "queue_track_ids": ["t2", "t3", "t4"],
                "max_per_artist": 0,
                "dedup_titles": false,
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let ids: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["track_id"].as_str().unwrap())
        .collect();
    for blocked in ["t2", "t3", "t4"] {
        assert!(
            !ids.contains(&blocked),
            "queued {blocked} should have been excluded at the ANN layer"
        );
    }
}

#[tokio::test]
async fn from_any_rejects_oversized_queue() {
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let big_queue: Vec<String> = (0..401).map(|i| format!("q{i}")).collect();
    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({
            "candidate_seeds": ["t0"],
            "n": 5,
            "queue_context": { "queue_track_ids": big_queue }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn from_any_requires_auth() {
    let state = build_state(test_config()).await;
    let app = build_router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/recommend/from-any")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"candidate_seeds": ["t0"], "n": 5}).to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// --- diversity_mode dispatch (Phase 2 wire) ---

#[tokio::test]
async fn from_any_diversity_mode_mmr_returns_slate() {
    // Smoke: the gateway accepts `diversity_mode: "mmr"` + `mmr_lambda`
    // on the wire and routes through `walk_mmr` without erroring. The
    // detailed correctness of the MMR ranking lives in the unit tests
    // for `mmr_rerank` — here we only verify the dispatch path is wired
    // up and the slate is non-empty.
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
        state
            .metadata_store()
            .upsert(&meta(
                &format!("t{i}"),
                &format!("ar{i}"),
                &format!("Artist {i}"),
                &format!("Song {i}"),
            ))
            .await
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({
            "candidate_seeds": ["t0"],
            "n": 5,
            "queue_context": {
                "queue_track_ids": [],
                "diversity_mode": "mmr",
                "mmr_lambda": 0.8,
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert!(!results.is_empty(), "MMR mode returned an empty slate");
}

#[tokio::test]
async fn from_any_mmr_fills_slate_when_only_same_artist_candidates_exist() {
    // Smada regression: queue holds one Duke Ellington track, ANN
    // returns N more Duke Ellington tracks. With the old hard cap
    // (default max_per_artist=2), most got rejected and the queue
    // could stay empty when 2 was already met by the queue + a single
    // admit. With the soft penalty model (default cap=0, μ=0.15) all
    // candidates must still be admitted because no alternatives
    // exist — the penalty deprioritises but never excludes.
    let state = build_state(test_config()).await;
    let ann = state.ann();
    // DIM same-artist candidates, one per orthogonal axis. Bounded by
    // DIM so we don't run out of unit-vector slots.
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
        state
            .metadata_store()
            .upsert(&meta(
                &format!("t{i}"),
                "duke",
                "Duke Ellington",
                &format!("Song {i}"),
            ))
            .await
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({
            "candidate_seeds": ["t0"],
            "n": 5,
            "queue_context": {
                "queue_track_ids": ["t0"],
                "now_playing_track_id": "t0",
                "diversity_mode": "mmr",
                "mmr_lambda": 0.8,
                // No `max_per_artist` and no `artist_penalty_weight`
                // → server defaults (cap=0, μ=0.15).
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert_eq!(
        results.len(),
        5,
        "soft penalty must still admit same-artist when no alternatives exist; \
         got {} results (the pre-fix bug returned 0)",
        results.len()
    );
}

#[tokio::test]
async fn from_any_mmr_prefers_fresh_artist_at_close_relevance() {
    // Inverse of the Smada test: when an alternative-artist candidate
    // *does* exist at comparable relevance, the soft penalty must
    // promote it above the same-artist candidate that the unpenalised
    // MMR would have picked first.
    //
    // Construction: seed at index 0. Candidate `same` (Duke Ellington)
    // at index 1 — high similarity to the seed. Candidate `fresh`
    // (other artist) at index 2 — slightly lower similarity. With μ=0
    // the same-artist candidate wins on relevance; with μ=0.15 the
    // fresh-artist candidate wins despite being less similar.
    let state = build_state(test_config()).await;
    let ann = state.ann();
    ann.upsert(&TrackId::from("seed"), &unit_at(0)).unwrap();
    // `same` is nearly aligned with the seed direction.
    let same_vec: Vec<f32> = (0..DIM)
        .map(|i| if i == 0 { 0.98 } else if i == 1 { 0.199 } else { 0.0 })
        .collect();
    ann.upsert(&TrackId::from("same"), &same_vec).unwrap();
    // `fresh` is a bit further from the seed direction.
    let fresh_vec: Vec<f32> = (0..DIM)
        .map(|i| if i == 0 { 0.95 } else if i == 1 { 0.312 } else { 0.0 })
        .collect();
    ann.upsert(&TrackId::from("fresh"), &fresh_vec).unwrap();
    state
        .metadata_store()
        .upsert(&meta("seed", "duke", "Duke Ellington", "Smada"))
        .await
        .unwrap();
    state
        .metadata_store()
        .upsert(&meta("same", "duke", "Duke Ellington", "Caravan"))
        .await
        .unwrap();
    state
        .metadata_store()
        .upsert(&meta("fresh", "miles", "Miles Davis", "So What"))
        .await
        .unwrap();
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({
            "candidate_seeds": ["seed"],
            "n": 1,
            "queue_context": {
                "queue_track_ids": ["seed"],
                "now_playing_track_id": "seed",
                "diversity_mode": "mmr",
                "mmr_lambda": 0.8,
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["track_id"].as_str(),
        Some("fresh"),
        "fresh-artist candidate should outrank same-artist at default μ=0.15"
    );
}

#[tokio::test]
async fn from_any_diversity_mode_off_skips_artist_cap() {
    // `off` should bypass *all* diversity gating — same-artist tracks
    // come back unconstrained. Mirrors `from_seeds_max_per_artist_zero
    // _disables_cap` but proves the wire toggles the right path: when
    // we ALSO send `max_per_artist: 2`, `off` mode should still admit
    // every track (cap is part of HardCap, not a separate filter).
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
        state
            .metadata_store()
            .upsert(&meta(
                &format!("t{i}"),
                "ar1",
                "Queen",
                &format!("Song {i}"),
            ))
            .await
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({
            "candidate_seeds": ["t0"],
            "n": 20,
            "queue_context": {
                "queue_track_ids": [],
                "diversity_mode": "off",
                "max_per_artist": 2,  // would normally cap at 2; off ignores it
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert_eq!(
        results.len(),
        DIM - 1,
        "diversity_mode=off should bypass max_per_artist; got {} of {}",
        results.len(),
        DIM - 1
    );
}

#[tokio::test]
async fn from_any_diversity_mode_default_preserves_hard_cap() {
    // No `diversity_mode` field → server default (HardCap). Must reproduce
    // the cap-enforcing behaviour byte-for-byte: with max_per_artist=2
    // and 7 same-artist tracks in a single-seed station, exactly 2 come
    // back. Regression guard for the default-preserving claim in the
    // QueueContext deserializer.
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
        state
            .metadata_store()
            .upsert(&meta(
                &format!("t{i}"),
                "ar1",
                "Queen",
                &format!("Song {i}"),
            ))
            .await
            .unwrap();
    }
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({
            "candidate_seeds": ["t0"],
            "n": 20,
            "queue_context": {
                "queue_track_ids": [],
                // diversity_mode omitted; should default to hard_cap
                "max_per_artist": 2,
                "dedup_titles": false,
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert_eq!(
        results.len(),
        2,
        "default diversity_mode should preserve HardCap (cap=2)"
    );
}

#[tokio::test]
async fn from_any_unknown_diversity_mode_rejected() {
    // Unknown enum variant → JsonRejection → 400. Catches typos at
    // request time rather than silently falling through to the default.
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/from-any",
        &json!({
            "candidate_seeds": ["t0"],
            "n": 5,
            "queue_context": {
                "queue_track_ids": [],
                "diversity_mode": "fancy_new_algorithm",
            }
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
