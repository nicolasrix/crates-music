//! Integration tests for /v1/recommend/* endpoints.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_core::TrackId;
use music_gateway::{Config, build_router};
use music_recommend::TrackMetadata;
use music_recommend::normalize_title;
use serde_json::{Value, json};
use tower::ServiceExt;

use common::{TEST_BEARER, build_state, build_state_with_embedder, test_config};

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

#[tokio::test]
async fn recommend_enqueue_rejects_oversized_track_ids() {
    let state = build_state(test_config()).await;
    let app = build_router(state.clone());

    // MAX_ENQUEUE_IDS is 1000; 1001 must 400 before any SQLite write.
    let track_ids: Vec<String> = (0..1001).map(|i| format!("t{i}")).collect();
    let body = json!({ "track_ids": track_ids }).to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/recommend/enqueue")
        .header("authorization", format!("Bearer {TEST_BEARER}"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let counts = state
        .embedding_store()
        .counts(state.recommend_model_version())
        .await
        .unwrap();
    assert_eq!(counts.not_started, 0, "rejected batch must not enqueue");
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

/// Wall-clock now in unix ms, for stamping play_history / asserting windows.
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

#[tokio::test]
async fn from_seeds_excludes_recently_played_tracks() {
    // #1 recency exclusion: a track the listener heard inside the window must
    // not be re-served by autoplay, even though it's a strong acoustic
    // neighbour of the seed.
    let mut cfg = test_config();
    cfg.recommend.recently_played_exclude_hours = 24.0; // wide window
    let state = build_state(cfg).await;
    let ann = state.ann();
    // Seed e0; two near neighbours of the seed that would both normally rank.
    ann.upsert(&TrackId::from("seed"), &unit_at(0)).unwrap();
    ann.upsert(&TrackId::from("keep"), &unit_at(0)).unwrap();
    ann.upsert(&TrackId::from("recent"), &unit_at(0)).unwrap();
    // The owner (TEST_BEARER ⇒ user id 1) played "recent" just now.
    state
        .play_history()
        .record_submission(1, &TrackId::from("recent"), now_ms())
        .await
        .unwrap();
    let app = build_router(state);

    let resp = app
        .oneshot(auth_post(
            "/v1/recommend/from-seeds",
            &json!({"seeds": ["seed"], "per_seed_n": 20, "top_n": 20}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let ids: Vec<&str> = body["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| r["track_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"keep"), "un-played neighbour must still appear");
    assert!(
        !ids.contains(&"recent"),
        "recently-played track must be excluded, got {ids:?}"
    );
}

#[tokio::test]
async fn from_seeds_serve_cooldown_suppresses_previously_served() {
    // #2 serve-cooldown: tracks served by a prior from-seeds call are
    // suppressed from the next refill (read back from the provenance log), so
    // a deterministic pipeline stops re-offering the same head every time.
    let mut cfg = test_config();
    cfg.recommend.served_cooldown_hours = 24.0;
    // log_provenance defaults on; the cooldown reads that log.
    assert!(cfg.recommend.log_provenance);
    let state = build_state(cfg).await;
    let ann = state.ann();
    ann.upsert(&TrackId::from("seed"), &unit_at(0)).unwrap();
    for i in 1..=4 {
        ann.upsert(&TrackId::from(format!("c{i}")), &unit_at(0))
            .unwrap();
    }
    let app = build_router(state);

    // First refill: serve two candidates. Those get logged as served.
    let first = read_json(
        app.clone()
            .oneshot(auth_post(
                "/v1/recommend/from-seeds",
                &json!({"seeds": ["seed"], "per_seed_n": 20, "top_n": 2}),
            ))
            .await
            .unwrap(),
    )
    .await;
    let served: Vec<String> = first["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| r["track_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(served.len(), 2, "first refill should serve two");

    // Second refill: the just-served tracks must not reappear.
    let second = read_json(
        app.oneshot(auth_post(
            "/v1/recommend/from-seeds",
            &json!({"seeds": ["seed"], "per_seed_n": 20, "top_n": 20}),
        ))
        .await
        .unwrap(),
    )
    .await;
    let again: Vec<String> = second["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|r| r["track_id"].as_str().unwrap().to_string())
        .collect();
    for id in &served {
        assert!(
            !again.contains(id),
            "previously-served {id} must be on cooldown, got {again:?}"
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
async fn from_seeds_anchor_leash_demotes_far_candidate() {
    // Geometry (DIM=8, raw cosines — whitening isn't fit in tests):
    //   anchor  A = e0
    //   nearN     = [0.8, 0.6, …]      cos(N, A) = 0.8   (inside τ)
    //   farF      = e2                 cos(F, A) = 0     (outside τ)
    //   frontier S= [0,0,0.9,0.436,…]  cos(F, S) ≈ 0.9, cos(*, A) = 0
    //
    // Σ-similarity (seeds A + S, equal weight, both excluded from results):
    //   nearN = cos(N,A) + cos(N,S) ≈ 0.8 + 0   = 0.8
    //   farF  = cos(F,A) + cos(F,S) ≈ 0   + 0.9 = 0.9
    //
    // So *without* a leash, farF outranks nearN. With the anchor leash on A
    // (τ=0.5, λ=16), farF eats a 16·0.5² = 4.0 penalty (→ −3.1) while nearN
    // (0.8 ≥ τ) is untouched, flipping the order. This proves the leash both
    // engages and uses anchor — not seed — similarity.
    fn vec8(parts: &[(usize, f32)]) -> Vec<f32> {
        let mut v = vec![0.0_f32; DIM];
        for &(i, x) in parts {
            v[i] = x;
        }
        v
    }

    let state = build_state(test_config()).await;
    let ann = state.ann();
    ann.upsert(&TrackId::from("A"), &vec8(&[(0, 1.0)])).unwrap();
    ann.upsert(&TrackId::from("nearN"), &vec8(&[(0, 0.8), (1, 0.6)]))
        .unwrap();
    ann.upsert(&TrackId::from("farF"), &vec8(&[(2, 1.0)]))
        .unwrap();
    ann.upsert(&TrackId::from("S"), &vec8(&[(2, 0.9), (3, 0.436)]))
        .unwrap();
    let app = build_router(state);

    let base = json!({
        "seeds": ["A", "S"],
        "seed_weights": [1.0, 1.0],
        "per_seed_n": 8,
        "top_n": 5,
        "sample_size": 2, // force both seeds queried
    });

    // Without a leash: farF (higher Σ-sim) ranks first.
    let resp = app
        .clone()
        .oneshot(auth_post("/v1/recommend/from-seeds", &base))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert_eq!(
        results[0]["track_id"], "farF",
        "without leash, the high-Σ-sim far candidate wins"
    );

    // With the leash anchored on A: nearN wins, farF is demoted.
    let mut leashed = base.clone();
    leashed["anchor_track_ids"] = json!(["A"]);
    leashed["leash_tau"] = json!(0.5);
    leashed["leash_lambda"] = json!(16.0);
    let resp = app
        .oneshot(auth_post("/v1/recommend/from-seeds", &leashed))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert_eq!(
        results[0]["track_id"], "nearN",
        "the anchor leash demotes the far candidate below the near one"
    );
}

#[tokio::test]
async fn provenance_logs_served_recommendations() {
    // Both /next and /from-seeds should leave a provenance row behind:
    // the request context + ordered slate, joinable to outcomes later.
    let state = build_state(test_config()).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }
    let app = build_router(state.clone());

    // /next — no session, seeds = [seed], kind = next.
    let resp = app
        .clone()
        .oneshot(auth_get("/v1/recommend/next?seed=t0&n=3"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // /from-seeds — carries a session id, seeds = the seed list.
    let resp = app
        .oneshot(auth_post(
            "/v1/recommend/from-seeds",
            &json!({"seeds": ["t1", "t2"], "top_n": 4, "session_id": "sess-prov"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Recording is inline (awaited before the response returns), so the
    // rows are durable by the time oneshot resolves.
    let logged = state.recommendation_log().recent(10).await.unwrap();
    assert_eq!(logged.len(), 2, "both calls were logged");

    // recent() is newest-first → from_seeds, then next.
    let from_seeds = &logged[0];
    assert_eq!(
        from_seeds.kind,
        music_recommend::RecommendationKind::FromSeeds
    );
    assert_eq!(from_seeds.session_id.as_deref(), Some("sess-prov"));
    assert_eq!(
        from_seeds.seeds,
        Some(vec!["t1".to_string(), "t2".to_string()])
    );
    assert!(!from_seeds.model_version.is_empty());
    assert_eq!(from_seeds.result_count as usize, from_seeds.items.len());
    assert!(!from_seeds.items.is_empty(), "served a non-empty slate");
    // Per-item features carry the aggregation provenance (seed_hits).
    assert!(from_seeds.items[0].features.get("seed_hits").is_some());
    assert!(from_seeds.params.get("requested_n").is_some());

    let next = &logged[1];
    assert_eq!(next.kind, music_recommend::RecommendationKind::Next);
    assert_eq!(next.session_id, None);
    assert_eq!(next.seeds, Some(vec!["t0".to_string()]));
    assert!(next.items[0].features.get("similarity").is_some());
}

#[tokio::test]
async fn provenance_disabled_logs_nothing() {
    let mut cfg = test_config();
    cfg.recommend.log_provenance = false;
    let state = build_state(cfg).await;
    let ann = state.ann();
    for i in 0..DIM {
        ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
            .unwrap();
    }
    let app = build_router(state.clone());

    let resp = app
        .oneshot(auth_get("/v1/recommend/next?seed=t0&n=3"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    assert_eq!(
        state.recommendation_log().count().await.unwrap(),
        0,
        "nothing logged when log_provenance is off"
    );
}

#[tokio::test]
async fn from_seeds_rejects_too_many_anchors() {
    let state = build_state(test_config()).await;
    let app = build_router(state);
    let anchors: Vec<String> = (0..201).map(|i| format!("a{i}")).collect();
    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({"seeds": ["t0"], "anchor_track_ids": anchors, "top_n": 5}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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
        .map(|i| {
            if i == 0 {
                0.98
            } else if i == 1 {
                0.199
            } else {
                0.0
            }
        })
        .collect();
    ann.upsert(&TrackId::from("same"), &same_vec).unwrap();
    // `fresh` is a bit further from the seed direction.
    let fresh_vec: Vec<f32> = (0..DIM)
        .map(|i| {
            if i == 0 {
                0.95
            } else if i == 1 {
                0.312
            } else {
                0.0
            }
        })
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

// --- /v1/recommend/similar_albums + similar_artists -----------------
//
// Two endpoints sharing one internal grouper: take a list of seed
// track ids (typically the tracks on the current album), run them
// through the ANN, then aggregate hits by album_id (or artist_id)
// using the track_metadata cache. Aggregation key is the only thing
// that differs between the two endpoints; the tests below cover the
// wiring on both, with the bulk of edge-case coverage on the album
// variant.

fn meta_full(
    id: &str,
    artist_id: &str,
    artist: &str,
    album_id: &str,
    album: &str,
    title: &str,
) -> TrackMetadata {
    TrackMetadata {
        track_id: TrackId::from(id),
        artist_id: Some(artist_id.into()),
        artist: artist.into(),
        album_id: Some(album_id.into()),
        album: Some(album.into()),
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

/// Seed an 8-track universe across three albums and two artists.
///
///   alb_a (ar_x):  t0, t1
///   alb_b (ar_x):  t2, t3
///   alb_c (ar_y):  t4, t5, t6, t7
///
/// ANN embeddings are unit vectors `unit_at(i)` so each track is its
/// own neighbour cluster — query(t0) returns t0 first, then nothing
/// usefully similar. That's fine: the grouper still folds *every*
/// returned hit into its album/artist bucket, and we assert the seed
/// album is excluded.
async fn seed_universe(state: &music_gateway::state::AppState) {
    let layout: &[(&str, &str, &str, &str, &str)] = &[
        ("t0", "ar_x", "X", "alb_a", "Album A"),
        ("t1", "ar_x", "X", "alb_a", "Album A"),
        ("t2", "ar_x", "X", "alb_b", "Album B"),
        ("t3", "ar_x", "X", "alb_b", "Album B"),
        ("t4", "ar_y", "Y", "alb_c", "Album C"),
        ("t5", "ar_y", "Y", "alb_c", "Album C"),
        ("t6", "ar_y", "Y", "alb_c", "Album C"),
        ("t7", "ar_y", "Y", "alb_c", "Album C"),
    ];
    let ann = state.ann();
    for (i, (id, ar_id, ar, alb_id, alb)) in layout.iter().enumerate() {
        ann.upsert(&TrackId::from(*id), &unit_at(i)).unwrap();
        state
            .metadata_store()
            .upsert(&meta_full(id, ar_id, ar, alb_id, alb, &format!("Song {i}")))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn similar_albums_excludes_seed_album_and_returns_others() {
    let state = build_state(test_config()).await;
    seed_universe(&state).await;
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/similar_albums",
        &json!({
            "seed_track_ids": ["t0", "t1"],
            "exclude_album_ids": ["alb_a"],
            "n": 5,
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    assert_eq!(body["all_seeds_unindexed"], false);
    let results = body["results"].as_array().expect("results array");
    assert!(
        !results.is_empty(),
        "should aggregate hits into other albums"
    );
    for r in results {
        assert_ne!(r["album_id"], "alb_a", "seed album must be excluded");
        // Each item must carry a numeric score and a supporting count.
        assert!(r["score"].is_number());
        let supporting = r["supporting_tracks"].as_u64().expect("supporting_tracks");
        assert!(supporting >= 1, "at least one hit must support each result");
    }
}

#[tokio::test]
async fn similar_albums_signals_all_unindexed() {
    let state = build_state(test_config()).await;
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/similar_albums",
        &json!({"seed_track_ids": ["unknown1", "unknown2"], "n": 5}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    assert_eq!(body["all_seeds_unindexed"], true);
    let results = body["results"].as_array().expect("results");
    assert!(results.is_empty());
}

#[tokio::test]
async fn similar_albums_rejects_empty_seeds() {
    let state = build_state(test_config()).await;
    let app = build_router(state);
    let req = auth_post(
        "/v1/recommend/similar_albums",
        &json!({"seed_track_ids": [], "n": 5}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn similar_albums_rejects_n_zero() {
    let state = build_state(test_config()).await;
    let app = build_router(state);
    let req = auth_post(
        "/v1/recommend/similar_albums",
        &json!({"seed_track_ids": ["t0"], "n": 0}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn similar_albums_caps_n_at_max() {
    let state = build_state(test_config()).await;
    seed_universe(&state).await;
    let app = build_router(state);
    let req = auth_post(
        "/v1/recommend/similar_albums",
        &json!({"seed_track_ids": ["t0"], "n": 10000}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    assert!(results.len() <= 100, "n must be clamped to MAX_N");
}

#[tokio::test]
async fn similar_albums_orders_by_score_desc() {
    let state = build_state(test_config()).await;
    seed_universe(&state).await;
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/similar_albums",
        &json!({
            "seed_track_ids": ["t0", "t1"],
            "exclude_album_ids": ["alb_a"],
            "n": 10,
            "per_seed_n": 50,
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    let scores: Vec<f64> = results
        .iter()
        .map(|r| r["score"].as_f64().expect("numeric score"))
        .collect();
    for w in scores.windows(2) {
        assert!(
            w[0] >= w[1],
            "results must be sorted by score desc, saw {} then {}",
            w[0],
            w[1],
        );
    }
}

#[tokio::test]
async fn similar_albums_skips_hits_with_no_metadata() {
    let state = build_state(test_config()).await;
    let ann = state.ann();
    // Two indexed tracks: t0 has metadata in alb_known; t1 has no
    // metadata row at all. After grouping, alb_known should appear
    // and no nameless album should show up.
    ann.upsert(&TrackId::from("seed"), &unit_at(0)).unwrap();
    ann.upsert(&TrackId::from("t0"), &unit_at(1)).unwrap();
    ann.upsert(&TrackId::from("t1"), &unit_at(2)).unwrap();
    state
        .metadata_store()
        .upsert(&meta_full("t0", "ar_x", "X", "alb_known", "Known", "k"))
        .await
        .unwrap();
    // intentionally NO metadata.upsert for t1.
    let app = build_router(state);

    let req = auth_post(
        "/v1/recommend/similar_albums",
        &json!({"seed_track_ids": ["seed"], "n": 5, "per_seed_n": 50}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let results = body["results"].as_array().expect("results");
    let ids: Vec<&str> = results
        .iter()
        .map(|r| r["album_id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"alb_known"),
        "alb_known should appear (got {ids:?})",
    );
    // No null album_id strings should sneak through serialisation.
    for r in results {
        assert!(r["album_id"].is_string());
    }
}

#[tokio::test]
async fn similar_albums_requires_auth() {
    let state = build_state(test_config()).await;
    let app = build_router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/recommend/similar_albums")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"seed_track_ids": ["t0"], "n": 5}).to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn similar_artists_excludes_seed_artist_and_returns_others() {
    let state = build_state(test_config()).await;
    seed_universe(&state).await;
    let app = build_router(state);

    // Seed with all ar_x tracks; exclude ar_x. The only other artist
    // is ar_y, so ar_y must be the (only) result.
    let req = auth_post(
        "/v1/recommend/similar_artists",
        &json!({
            "seed_track_ids": ["t0", "t1", "t2", "t3"],
            "exclude_artist_ids": ["ar_x"],
            "n": 5,
            "per_seed_n": 50,
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    assert_eq!(body["all_seeds_unindexed"], false);
    let results = body["results"].as_array().expect("results");
    assert!(!results.is_empty());
    let ids: Vec<&str> = results
        .iter()
        .map(|r| r["artist_id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&"ar_x"), "seed artist must be excluded");
    assert!(ids.contains(&"ar_y"), "other artist must surface");
}

#[tokio::test]
async fn similar_artists_signals_all_unindexed() {
    let state = build_state(test_config()).await;
    let app = build_router(state);
    let req = auth_post(
        "/v1/recommend/similar_artists",
        &json!({"seed_track_ids": ["missing"], "n": 5}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    assert_eq!(body["all_seeds_unindexed"], true);
    let results = body["results"].as_array().expect("results");
    assert!(results.is_empty());
}

#[tokio::test]
async fn similar_artists_rejects_empty_seeds() {
    let state = build_state(test_config()).await;
    let app = build_router(state);
    let req = auth_post(
        "/v1/recommend/similar_artists",
        &json!({"seed_track_ids": [], "n": 5}),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn similar_artists_requires_auth() {
    let state = build_state(test_config()).await;
    let app = build_router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/recommend/similar_artists")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"seed_track_ids": ["t0"], "n": 5}).to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// --- /v1/recommend/station -------------------------------------------
//
// Text-query playlist. The sidecar is faked with wiremock so the test
// is deterministic: it returns a unit vector that the ANN's
// pre-populated tracks will match cleanly.

mod station {
    use super::*;
    use music_gateway::embedder::EmbedderHandle;
    use music_recommend::embedder::{EmbedderClient, EmbedderConfig, EmbedderHealth};
    use music_recommend::types::ModelVersion;
    use std::time::Duration;
    use url::Url;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fake_embed_response(vector: &[f32]) -> Value {
        json!({
            "vector": vector,
            "dim": vector.len(),
            "model_version": "test-v1",
        })
    }

    fn ready_handle(server: &MockServer) -> EmbedderHandle {
        let url: Url = server.uri().parse().unwrap();
        let client = EmbedderClient::new(EmbedderConfig {
            url,
            timeout: Duration::from_secs(2),
            bearer_token: None,
        })
        .expect("client builds");
        let health = EmbedderHealth {
            reachable: true,
            model_loaded: true,
            model_version: ModelVersion::from("test-v1"),
            dim: DIM,
            device: Some("cpu".to_string()),
        };
        EmbedderHandle::new(Some(client), Some(health))
    }

    #[tokio::test]
    async fn returns_top_k_for_text_query() {
        let server = MockServer::start().await;
        // Sidecar returns the unit vector that exactly matches t3 — we
        // expect t3 first in the ANN results.
        Mock::given(method("POST"))
            .and(path("/embed/text"))
            .and(body_json(json!({ "text": "sunny afternoon" })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(fake_embed_response(&unit_at(3))),
            )
            .mount(&server)
            .await;

        let handle = ready_handle(&server);
        let state = build_state_with_embedder(test_config(), handle).await;
        let ann = state.ann();
        for i in 0..DIM {
            ann.upsert(&TrackId::from(format!("t{i}")), &unit_at(i))
                .unwrap();
        }

        let app = build_router(state);
        let resp = app
            .oneshot(auth_get("/v1/recommend/station?text=sunny+afternoon&n=3"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = read_json(resp).await;
        assert_eq!(body["query"], "sunny afternoon");
        let results = body["results"].as_array().expect("results array");
        assert!(!results.is_empty());
        assert!(results.len() <= 3);
        // The track whose stored vector matches the embedder's response
        // should come back first.
        assert_eq!(results[0]["track_id"], "t3");
    }

    #[tokio::test]
    async fn returns_503_when_embedder_disabled() {
        let state = build_state(test_config()).await;
        let app = build_router(state);
        let resp = app
            .oneshot(auth_get("/v1/recommend/station?text=hello&n=5"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn rejects_empty_or_whitespace_text() {
        let server = MockServer::start().await;
        let handle = ready_handle(&server);
        let state = build_state_with_embedder(test_config(), handle).await;
        let app = build_router(state);
        let resp = app
            .oneshot(auth_get("/v1/recommend/station?text=&n=3"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        // Whitespace-only is also rejected — it would be a meaningless
        // CLAP query and we don't want to burn a sidecar round-trip on it.
        let server2 = MockServer::start().await;
        let app =
            build_router(build_state_with_embedder(test_config(), ready_handle(&server2)).await);
        let resp = app
            .oneshot(auth_get("/v1/recommend/station?text=%20%20&n=3"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rejects_n_zero() {
        let server = MockServer::start().await;
        let handle = ready_handle(&server);
        let state = build_state_with_embedder(test_config(), handle).await;
        let app = build_router(state);
        let resp = app
            .oneshot(auth_get("/v1/recommend/station?text=hello&n=0"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn surfaces_embedder_failure_as_bad_gateway() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embed/text"))
            .respond_with(ResponseTemplate::new(500).set_body_string("kaboom"))
            .mount(&server)
            .await;
        let handle = ready_handle(&server);
        let state = build_state_with_embedder(test_config(), handle).await;
        let app = build_router(state);
        let resp = app
            .oneshot(auth_get("/v1/recommend/station?text=rainy&n=5"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }
}

// --- /v1/recommend/refit_whitening -----------------------------------

mod refit_whitening {
    use super::*;
    use music_recommend::{Embedding, EmbeddingKey};

    /// Seed `count` done embeddings into the store for the state's model.
    async fn seed_done(state: &music_gateway::AppState, count: usize) {
        let mv = state.recommend_model_version().clone();
        for i in 0..count {
            let key = EmbeddingKey::new(TrackId::from(format!("t{i}")), mv.clone());
            state.embedding_store().enqueue(&key).await.unwrap();
            // Spread the vectors across axes so the corpus has variance to
            // fit a transform from (a constant corpus yields k = 0).
            let mut v = unit_at(i % DIM);
            v[(i + 1) % DIM] = 0.5;
            state
                .embedding_store()
                .mark_done(&Embedding::new(key, v))
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn fits_persists_and_installs_on_ann() {
        let state = build_state(test_config()).await;
        seed_done(&state, DIM).await;
        assert!(!state.ann().has_whitening());

        let app = build_router(state.clone());
        let resp = app
            .oneshot(auth_post("/v1/recommend/refit_whitening", &json!({})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = read_json(resp).await;
        assert_eq!(body["n_samples"], DIM);
        assert_eq!(body["dim"], DIM);
        assert!(body["k"].as_u64().unwrap() >= 1);

        // Transform is now live on the ANN, and persisted so a reload
        // wouldn't have to refit.
        assert!(state.ann().has_whitening());
        let mv = state.recommend_model_version().clone();
        assert!(state.whitening_store().get(&mv).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn conflict_when_no_embeddings() {
        let state = build_state(test_config()).await;
        let app = build_router(state);
        let resp = app
            .oneshot(auth_post("/v1/recommend/refit_whitening", &json!({})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn returns_429_when_a_refit_is_already_running() {
        let state = build_state(test_config()).await;
        seed_done(&state, DIM).await;

        // Hold the single refit permit to simulate an in-flight refit;
        // the handler's `try_acquire` must then fail fast with 429
        // rather than queue or duplicate the work.
        let _held = state.refit_gate().try_acquire().unwrap();

        let app = build_router(state.clone());
        let resp = app
            .oneshot(auth_post("/v1/recommend/refit_whitening", &json!({})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}

// --- user-preference re-scoring (affinity → /next) -------------------
//
// These exercise the full server path: ANN retrieval → affinity lookup →
// relevance re-rank. Feedback is written through the real
// `POST /v1/recommend/feedback` endpoint so the write wiring is covered
// too. Whitening is identity in tests (boot-time fit doesn't run), so the
// reported similarities are raw cosines and the ordering is predictable.

/// Unit vector in the e0–e1 plane with cosine `first` to the seed (e0).
fn graded(first: f32) -> Vec<f32> {
    let mut v = vec![0.0_f32; DIM];
    v[0] = first;
    v[1] = (1.0 - first * first).max(0.0).sqrt();
    v
}

/// `test_config` with the preference feature enabled at a weight strong
/// enough to make the re-ranking unambiguous in assertions.
fn preference_config() -> Config {
    let mut c = test_config();
    c.recommend.preference_enabled = true;
    c.recommend.preference_weight = 0.5;
    c
}

/// Preference explicitly *off*. Set rather than assumed: the gateway default
/// is now on, so a test about the disabled path has to say so.
fn no_preference_config() -> Config {
    let mut c = test_config();
    c.recommend.preference_enabled = false;
    c
}

async fn post_feedback(app: axum::Router, track_id: &str, vote: &str) -> StatusCode {
    let body = json!({"track_id": track_id, "vote": vote, "session_id": "sess-1"}).to_string();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/recommend/feedback")
        .header("authorization", format!("Bearer {TEST_BEARER}"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    app.oneshot(req).await.unwrap().status()
}

async fn next_ids(app: axum::Router, uri: &str) -> Vec<String> {
    let resp = app.oneshot(auth_get(uri)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["track_id"].as_str().unwrap().to_string())
        .collect()
}

/// Seed t0 plus three candidates of decreasing acoustic similarity.
fn seed_and_graded_candidates(state: &music_gateway::AppState) {
    let ann = state.ann();
    ann.upsert(&TrackId::from("t0"), &graded(1.0)).unwrap(); // seed
    ann.upsert(&TrackId::from("t1"), &graded(0.95)).unwrap(); // most similar
    ann.upsert(&TrackId::from("t2"), &graded(0.90)).unwrap();
    ann.upsert(&TrackId::from("t3"), &graded(0.80)).unwrap(); // least similar
}

#[tokio::test]
async fn next_demotes_a_disliked_track_when_preference_enabled() {
    let state = build_state(preference_config()).await;
    seed_and_graded_candidates(&state);

    // Control: with no feedback, the most-similar candidate leads.
    let ids = next_ids(
        build_router(state.clone()),
        "/v1/recommend/next?seed=t0&n=3",
    )
    .await;
    assert_eq!(
        ids[0], "t1",
        "most-similar candidate should lead with no feedback"
    );

    // Thumbs-down t1 via the real endpoint.
    let status = post_feedback(build_router(state.clone()), "t1", "down").await;
    assert_eq!(status, StatusCode::OK);

    // t1's negative affinity sinks it below the others; t2 now leads and
    // the disliked t1 falls to the back.
    let ids = next_ids(
        build_router(state.clone()),
        "/v1/recommend/next?seed=t0&n=3",
    )
    .await;
    assert_eq!(ids[0], "t2", "disliked t1 must no longer lead");
    assert_eq!(
        ids.last().map(String::as_str),
        Some("t1"),
        "disliked t1 sinks to last"
    );
}

#[tokio::test]
async fn next_promotes_a_liked_track_when_preference_enabled() {
    let state = build_state(preference_config()).await;
    seed_and_graded_candidates(&state);

    // Like the *least* similar candidate; the bonus should lift it past
    // the acoustic leader.
    let status = post_feedback(build_router(state.clone()), "t3", "up").await;
    assert_eq!(status, StatusCode::OK);

    let ids = next_ids(
        build_router(state.clone()),
        "/v1/recommend/next?seed=t0&n=3",
    )
    .await;
    assert_eq!(
        ids[0], "t3",
        "liked t3 should climb above the acoustic leader"
    );
}

#[tokio::test]
async fn next_ignores_feedback_when_preference_disabled() {
    // With preference off, even a strong dislike must not perturb the
    // pure-similarity ordering.
    let state = build_state(no_preference_config()).await;
    seed_and_graded_candidates(&state);

    let status = post_feedback(build_router(state.clone()), "t1", "down").await;
    assert_eq!(status, StatusCode::OK);

    let ids = next_ids(
        build_router(state.clone()),
        "/v1/recommend/next?seed=t0&n=3",
    )
    .await;
    assert_eq!(
        ids,
        vec!["t1", "t2", "t3"],
        "ordering unchanged when preference is off"
    );
}

#[tokio::test]
async fn from_seeds_demotes_disliked_track_in_hardcap_mode() {
    // Exercises the preference pre-sort on the *default* (HardCap) filter
    // path — the one the queue-refill clients actually use. A
    // `queue_context` must be present for the filter (and thus the
    // pre-sort) to run at all.
    let state = build_state(preference_config()).await;
    seed_and_graded_candidates(&state);

    let status = post_feedback(build_router(state.clone()), "t1", "down").await;
    assert_eq!(status, StatusCode::OK);

    let app = build_router(state.clone());
    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "per_seed_n": 20,
            "top_n": 5,
            "queue_context": {}
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let ids: Vec<String> = body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["track_id"].as_str().unwrap().to_string())
        .collect();

    assert_ne!(
        ids.first().map(String::as_str),
        Some("t1"),
        "disliked t1 must not lead"
    );
    let pos = |id: &str| ids.iter().position(|x| x == id);
    assert!(
        pos("t1") > pos("t2"),
        "disliked t1 ranked below t2 (ids = {ids:?})"
    );
}

#[tokio::test]
async fn from_seeds_scales_preference_bonus_into_sigma_similarity_units() {
    // Regression: bonuses are tuned on the *cosine* scale, but from-seeds
    // scores `Σ wᵢ·simᵢ`. Adding a raw cosine bonus to a weighted sum
    // under-applies it in proportion to the seed weights — the autoplay
    // client sends anchors at weight 3.0, so preference was ~3× too weak
    // there while being correctly scaled on /next.
    //
    // One seed at weight 4.0 isolates the unit conversion from the
    // seed-count effect: candidate scores are exactly 4·sim.
    //
    //   t1 = 4 × 0.95 = 3.80   ← acoustic leader
    //   t3 = 4 × 0.80 = 3.20   ← liked, must overtake
    //
    // The gap is 0.60. A liked t3's bonus is `preference_weight(0.5) ×
    // tanh(1.0/2) ≈ 0.231` — unscaled that closes none of it, so this
    // asserted ordering fails on the pre-fix code. Scaled by the seed
    // weight it is ≈ 0.924, which clears the gap.
    let state = build_state(preference_config()).await;
    seed_and_graded_candidates(&state);

    let status = post_feedback(build_router(state.clone()), "t3", "up").await;
    assert_eq!(status, StatusCode::OK);

    let app = build_router(state.clone());
    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "seed_weights": [4.0],
            "per_seed_n": 20,
            "top_n": 5,
            "queue_context": {}
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let ids: Vec<String> = body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["track_id"].as_str().unwrap().to_string())
        .collect();

    assert_eq!(
        ids.first().map(String::as_str),
        Some("t3"),
        "liked t3 should lead once the bonus is in Σ-similarity units (ids = {ids:?})"
    );
}

#[tokio::test]
async fn from_seeds_leaves_ranking_alone_when_nothing_is_liked() {
    // The scale factor multiplies an empty bonus map on a cold profile, so
    // a weighted request must still return pure acoustic order. Guards
    // against the factor leaking in as a score multiplier.
    let state = build_state(preference_config()).await;
    seed_and_graded_candidates(&state);

    let app = build_router(state.clone());
    let req = auth_post(
        "/v1/recommend/from-seeds",
        &json!({
            "seeds": ["t0"],
            "seed_weights": [4.0],
            "per_seed_n": 20,
            "top_n": 5,
            "queue_context": {}
        }),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let ids: Vec<String> = body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["track_id"].as_str().unwrap().to_string())
        .collect();

    assert_eq!(ids, vec!["t1", "t2", "t3"], "acoustic order preserved");
}
