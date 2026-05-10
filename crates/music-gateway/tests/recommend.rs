//! Integration tests for /v1/recommend/* endpoints.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_core::TrackId;
use music_gateway::build_router;
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
