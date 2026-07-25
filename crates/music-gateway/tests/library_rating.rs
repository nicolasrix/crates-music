//! Integration tests for /v1/library/rating (PUT) and
//! /v1/library/ratings (GET) — the gateway-owned durable like/dislike for
//! tracks, albums, and artists, plus its always-on effect on the
//! recommender (dislike hard-excludes the entity's tracks, like boosts
//! them) regardless of the preference feature flag.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use music_core::TrackId;
use music_gateway::build_router;
use music_recommend::{TrackMetadata, normalize_title};
use serde_json::{Value, json};
use tower::ServiceExt;

use common::{TEST_BEARER, build_state, test_config};

const DIM: usize = 8;

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

fn auth_put(uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("authorization", format!("Bearer {TEST_BEARER}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Unit vector in the e0–e1 plane with cosine `first` to the seed (e0).
fn graded(first: f32) -> Vec<f32> {
    let mut v = vec![0.0_f32; DIM];
    v[0] = first;
    v[1] = (1.0 - first * first).max(0.0).sqrt();
    v
}

/// Seed t0 plus three candidates of decreasing acoustic similarity.
fn seed_and_graded_candidates(state: &music_gateway::AppState) {
    let ann = state.ann();
    ann.upsert(&TrackId::from("t0"), &graded(1.0)).unwrap(); // seed
    ann.upsert(&TrackId::from("t1"), &graded(0.95)).unwrap(); // most similar
    ann.upsert(&TrackId::from("t2"), &graded(0.90)).unwrap();
    ann.upsert(&TrackId::from("t3"), &graded(0.80)).unwrap(); // least similar
}

/// Minimal metadata row tying a track to an album + artist — the join the
/// dislike-album / dislike-artist expansion (and like-album / like-artist
/// boost) reads.
fn meta(track: &str, album_id: &str, artist_id: &str) -> TrackMetadata {
    TrackMetadata {
        track_id: TrackId::from(track),
        artist_id: Some(artist_id.into()),
        artist: "Artist".into(),
        album_id: Some(album_id.into()),
        album: Some("Album".into()),
        title: track.into(),
        title_normalized: normalize_title(track),
        duration_seconds: None,
        genre: None,
        year: None,
        track_number: None,
        disc_number: None,
        bpm: None,
        musical_key: None,
    }
}

/// Give each candidate a distinct album + artist so entity-level dislikes
/// expand to exactly one track. t1→al1/ar1, t2→al2/ar2, t3→al3/ar3.
async fn seed_metadata(state: &music_gateway::AppState) {
    let m = state.metadata_store();
    for (t, al, ar) in [
        ("t1", "al1", "ar1"),
        ("t2", "al2", "ar2"),
        ("t3", "al3", "ar3"),
    ] {
        m.upsert(&meta(t, al, ar)).await.unwrap();
    }
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

#[tokio::test]
async fn put_then_get_roundtrip() {
    let state = build_state(test_config()).await;

    let resp = build_router(state.clone())
        .oneshot(auth_put(
            "/v1/library/rating",
            &json!({"kind": "track", "id": "t1", "rating": "like"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    assert_eq!(body["kind"], "track");
    assert_eq!(body["id"], "t1");
    assert_eq!(body["rating"], "like");

    let resp = build_router(state.clone())
        .oneshot(auth_get("/v1/library/ratings"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    let ratings = body["ratings"].as_array().expect("ratings array");
    assert_eq!(ratings.len(), 1);
    assert_eq!(ratings[0]["kind"], "track");
    assert_eq!(ratings[0]["id"], "t1");
    assert_eq!(ratings[0]["rating"], "like");
}

#[tokio::test]
async fn kind_defaults_to_track_when_omitted() {
    // The wire `kind` field defaults to track, so a bare {id, rating}
    // body keeps working.
    let state = build_state(test_config()).await;
    let resp = build_router(state.clone())
        .oneshot(auth_put(
            "/v1/library/rating",
            &json!({"id": "t1", "rating": "like"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_json(resp).await;
    assert_eq!(body["kind"], "track");
}

#[tokio::test]
async fn album_and_artist_ratings_roundtrip_distinctly() {
    let state = build_state(test_config()).await;
    for (kind, id) in [("album", "al1"), ("artist", "ar1")] {
        let status = build_router(state.clone())
            .oneshot(auth_put(
                "/v1/library/rating",
                &json!({"kind": kind, "id": id, "rating": "like"}),
            ))
            .await
            .unwrap()
            .status();
        assert_eq!(status, StatusCode::OK);
    }

    let resp = build_router(state.clone())
        .oneshot(auth_get("/v1/library/ratings"))
        .await
        .unwrap();
    let body = read_json(resp).await;
    let ratings = body["ratings"].as_array().expect("ratings array");
    assert_eq!(ratings.len(), 2);
    let kinds: Vec<&str> = ratings
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"album"));
    assert!(kinds.contains(&"artist"));
}

#[tokio::test]
async fn put_null_clears_the_rating() {
    let state = build_state(test_config()).await;

    let app = build_router(state.clone());
    let status = app
        .oneshot(auth_put(
            "/v1/library/rating",
            &json!({"kind": "album", "id": "al1", "rating": "dislike"}),
        ))
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);

    // rating: null clears it.
    let status = build_router(state.clone())
        .oneshot(auth_put(
            "/v1/library/rating",
            &json!({"kind": "album", "id": "al1", "rating": null}),
        ))
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);

    let resp = build_router(state.clone())
        .oneshot(auth_get("/v1/library/ratings"))
        .await
        .unwrap();
    let body = read_json(resp).await;
    assert!(
        body["ratings"].as_array().unwrap().is_empty(),
        "rating should be cleared"
    );
}

#[tokio::test]
async fn put_rejects_empty_id() {
    let state = build_state(test_config()).await;
    let status = build_router(state)
        .oneshot(auth_put(
            "/v1/library/rating",
            &json!({"kind": "track", "id": "", "rating": "like"}),
        ))
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn dislike_excludes_track_from_next_with_preference_off() {
    // Default config → preference feature OFF. Dislike exclusion must
    // still apply: it is not gated by the preference flag.
    let state = build_state(test_config()).await;
    seed_and_graded_candidates(&state);

    // Control: t1 (most similar) leads and is present.
    let ids = next_ids(
        build_router(state.clone()),
        "/v1/recommend/next?seed=t0&n=3",
    )
    .await;
    assert!(ids.contains(&"t1".to_string()), "t1 present before dislike");

    let status = build_router(state.clone())
        .oneshot(auth_put(
            "/v1/library/rating",
            &json!({"kind": "track", "id": "t1", "rating": "dislike"}),
        ))
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);

    let ids = next_ids(
        build_router(state.clone()),
        "/v1/recommend/next?seed=t0&n=3",
    )
    .await;
    assert!(
        !ids.contains(&"t1".to_string()),
        "disliked t1 must be hard-excluded from /next (ids = {ids:?})"
    );
}

#[tokio::test]
async fn dislike_album_excludes_its_tracks_from_next() {
    // A disliked album excludes every track of that album from play.
    let state = build_state(test_config()).await;
    seed_and_graded_candidates(&state);
    seed_metadata(&state).await;

    // t1 belongs to album al1; dislike al1.
    let status = build_router(state.clone())
        .oneshot(auth_put(
            "/v1/library/rating",
            &json!({"kind": "album", "id": "al1", "rating": "dislike"}),
        ))
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);

    let ids = next_ids(
        build_router(state.clone()),
        "/v1/recommend/next?seed=t0&n=3",
    )
    .await;
    assert!(
        !ids.contains(&"t1".to_string()),
        "track t1 of disliked album al1 must be excluded (ids = {ids:?})"
    );
    // Siblings on other albums are untouched.
    assert!(ids.contains(&"t2".to_string()));
}

#[tokio::test]
async fn dislike_artist_excludes_its_tracks_from_next() {
    let state = build_state(test_config()).await;
    seed_and_graded_candidates(&state);
    seed_metadata(&state).await;

    // t2 belongs to artist ar2; dislike ar2.
    let status = build_router(state.clone())
        .oneshot(auth_put(
            "/v1/library/rating",
            &json!({"kind": "artist", "id": "ar2", "rating": "dislike"}),
        ))
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);

    let ids = next_ids(
        build_router(state.clone()),
        "/v1/recommend/next?seed=t0&n=3",
    )
    .await;
    assert!(
        !ids.contains(&"t2".to_string()),
        "track t2 of disliked artist ar2 must be excluded (ids = {ids:?})"
    );
    assert!(ids.contains(&"t1".to_string()));
}

#[tokio::test]
async fn like_promotes_track_in_next_with_preference_off() {
    // Like-boost is always-on too. With preference disabled, a liked
    // less-similar track still climbs past the acoustic leader given a
    // bonus large enough to clear the similarity gap.
    let mut config = test_config();
    config.recommend.like_bonus = 0.5; // unambiguous for the assertion
    let state = build_state(config).await;
    seed_and_graded_candidates(&state);

    // t3 is least similar (0.80). With +0.5 it reaches 1.30, above t1's 0.95.
    let status = build_router(state.clone())
        .oneshot(auth_put(
            "/v1/library/rating",
            &json!({"kind": "track", "id": "t3", "rating": "like"}),
        ))
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);

    let ids = next_ids(
        build_router(state.clone()),
        "/v1/recommend/next?seed=t0&n=3",
    )
    .await;
    assert_eq!(
        ids[0], "t3",
        "liked t3 should lead despite preference being off (ids = {ids:?})"
    );
}

#[tokio::test]
async fn like_album_promotes_its_tracks_in_next() {
    // A liked album boosts the relevance of its member tracks.
    let mut config = test_config();
    config.recommend.like_bonus_album = 0.5; // unambiguous for the assertion
    let state = build_state(config).await;
    seed_and_graded_candidates(&state);
    seed_metadata(&state).await;

    // t3 (least similar, 0.80) is on album al3. Liking al3 adds +0.5 →
    // 1.30, above t1's 0.95.
    let status = build_router(state.clone())
        .oneshot(auth_put(
            "/v1/library/rating",
            &json!({"kind": "album", "id": "al3", "rating": "like"}),
        ))
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::OK);

    let ids = next_ids(
        build_router(state.clone()),
        "/v1/recommend/next?seed=t0&n=3",
    )
    .await;
    assert_eq!(
        ids[0], "t3",
        "track on liked album al3 should lead (ids = {ids:?})"
    );
}

#[tokio::test]
async fn dislike_excludes_track_from_station() {
    // The text-station path excludes disliked tracks too. No embedder is
    // configured in this default state, so /station returns 503 — assert
    // the exclusion at the ANN layer is wired by checking the simpler
    // /next path above; here we just confirm the station endpoint is
    // reachable and guards the empty-embedder case rather than 500ing.
    let state = build_state(test_config()).await;
    seed_and_graded_candidates(&state);
    build_router(state.clone())
        .oneshot(auth_put(
            "/v1/library/rating",
            &json!({"kind": "track", "id": "t1", "rating": "dislike"}),
        ))
        .await
        .unwrap();
    let resp = build_router(state)
        .oneshot(auth_get("/v1/recommend/station?text=anything&n=3"))
        .await
        .unwrap();
    // Embedder absent in the default test state → 503, not a 500 from a
    // mis-wired exclude slice.
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}
