//! Recommend endpoints + ingest glue for the gateway.
//!
//! - `GET /v1/recommend/next?seed=<id>&n=<N>` — top-N similar tracks.
//!   Returns 404 if the seed isn't in the ANN. Caps `n` at MAX_N to
//!   keep responses bounded.
//! - `POST /v1/recommend/enqueue {track_ids: [...]}` — admin-style
//!   enqueue for the ingest queue. Idempotent on (track_id,
//!   model_version): duplicates collapse via the store's
//!   INSERT-OR-IGNORE.

use std::collections::HashSet;

use axum::{
    Json,
    extract::{Query, State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
};
use music_core::TrackId;
use music_recommend::aggregate::{aggregate_seed_results, sample_indices};
use music_recommend::types::ModelVersion;
use music_recommend::{EmbeddingKey, EmbeddingStore, ann::AnnIndex};
use rand::SeedableRng;
use rand::rngs::SmallRng;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

const MAX_N: usize = 100;
/// Hard cap on inputs to /v1/recommend/from-seeds. Refuse rather than
/// truncate — a client passing 10k seeds is buggy, not legitimate.
const MAX_SEEDS: usize = 200;
/// Hard cap on `exclude_track_ids`. Above this we 400 — same rationale.
const MAX_EXCLUDE: usize = 1000;
const DEFAULT_SAMPLE_SIZE: usize = 8;
const DEFAULT_PER_SEED_N: usize = 20;
const DEFAULT_TOP_N: usize = 20;

#[derive(Debug, Deserialize)]
pub struct RecommendNextQuery {
    pub seed: String,
    #[serde(default = "default_n")]
    pub n: usize,
}

fn default_n() -> usize {
    20
}

#[derive(Debug, Serialize)]
pub struct RecommendNextResponse {
    pub seed: String,
    pub model_version: Option<String>,
    pub degraded: bool,
    pub results: Vec<RecommendItem>,
}

#[derive(Debug, Serialize)]
pub struct RecommendItem {
    pub track_id: String,
    pub similarity: f32,
}

#[tracing::instrument(
    name = "recommend.next",
    skip_all,
    fields(
        seed = %q.seed,
        n = tracing::field::Empty,
        seed_source = tracing::field::Empty,
        results = tracing::field::Empty,
    ),
)]
pub async fn next(
    State(state): State<AppState>,
    Query(q): Query<RecommendNextQuery>,
) -> Result<Json<RecommendNextResponse>, (StatusCode, &'static str)> {
    if q.n == 0 {
        return Err((StatusCode::BAD_REQUEST, "n must be >= 1"));
    }
    let n = q.n.min(MAX_N);
    tracing::Span::current().record("n", n);
    let seed_id = TrackId::from(q.seed.clone());
    let model_version = state.recommend_model_version().clone();

    let (seed_vector, source) = lookup_seed_vector(
        state.ann(),
        state.embedding_store(),
        &seed_id,
        &model_version,
    )
    .await;
    tracing::Span::current().record("seed_source", source);
    let Some(vector) = seed_vector else {
        return Err((StatusCode::NOT_FOUND, "seed not embedded"));
    };

    let results = state
        .ann()
        .query_excluding(&vector, n, std::slice::from_ref(&seed_id))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "ann query failed"))?;
    tracing::Span::current().record("results", results.len());

    Ok(Json(RecommendNextResponse {
        seed: q.seed,
        model_version: Some(model_version.as_str().to_string()),
        degraded: false,
        results: results
            .into_iter()
            .map(|r| RecommendItem {
                track_id: r.track_id.into_inner(),
                similarity: r.similarity,
            })
            .collect(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct EnqueueRequest {
    pub track_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct EnqueueResponse {
    pub enqueued: u64,
}

pub async fn enqueue(
    State(state): State<AppState>,
    payload: Result<Json<EnqueueRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Ok(Json(req)) = payload else {
        return (StatusCode::BAD_REQUEST, "invalid JSON body").into_response();
    };
    // Dedupe inside a single request. The store's INSERT OR IGNORE
    // handles cross-request idempotency on (track_id, model_version).
    let unique: HashSet<&str> = req.track_ids.iter().map(String::as_str).collect();
    let model_version = state.recommend_model_version().clone();
    let mut enqueued = 0u64;
    for id in unique {
        let key = EmbeddingKey::new(TrackId::from(id), model_version.clone());
        if state.embedding_store().enqueue(&key).await.is_ok() {
            enqueued += 1;
        }
    }
    // The "enqueued" count is the number of inserts attempted; the
    // store's idempotency means re-enqueueing an existing key isn't
    // an error but also doesn't bump the queue length. The test
    // suite asserts behaviour on `counts`, not on this scalar.
    (StatusCode::ACCEPTED, Json(EnqueueResponse { enqueued })).into_response()
}

// --- /v1/recommend/from-seeds ----------------------------------------
//
// Multi-seed station: client hands us a set of track ids (typically a
// playlist's tracks), we sample, fan out per-seed ANN queries, and
// fold them with Σ-similarity scoring. Replaces the per-seed fan-out
// the web client used to do by hand.

#[derive(Debug, Deserialize)]
pub struct FromSeedsRequest {
    pub seeds: Vec<String>,
    /// Per-seed top-K from the ANN. Defaults to 20.
    pub per_seed_n: Option<usize>,
    /// Max seeds to actually query (random sample). Defaults to 8.
    pub sample_size: Option<usize>,
    /// Final cap on aggregated results. Defaults to 20, capped at MAX_N.
    pub top_n: Option<usize>,
    /// Extra ids to drop from results (already-added tracks, dismissed).
    /// The seed set is excluded automatically — don't repeat it here.
    #[serde(default)]
    pub exclude_track_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct FromSeedsResponse {
    pub model_version: Option<String>,
    pub degraded: bool,
    pub results: Vec<RecommendItem>,
    /// `true` when *every* sampled seed was unindexed. Lets the UI
    /// distinguish "playlist hasn't been embedded yet" from "we just
    /// don't have anything more to suggest." Mirrors the field of the
    /// same name in the web client's old `PlaylistSuggestionResult`.
    pub all_seeds_unindexed: bool,
}

#[tracing::instrument(
    name = "recommend.from_seeds",
    skip_all,
    fields(
        seeds_total = tracing::field::Empty,
        seeds_sampled = tracing::field::Empty,
        seeds_indexed = tracing::field::Empty,
        results = tracing::field::Empty,
    ),
)]
pub async fn from_seeds(
    State(state): State<AppState>,
    payload: Result<Json<FromSeedsRequest>, JsonRejection>,
) -> Result<Json<FromSeedsResponse>, (StatusCode, &'static str)> {
    let Json(req) = payload.map_err(|_| (StatusCode::BAD_REQUEST, "invalid JSON body"))?;

    if req.seeds.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "seeds must not be empty"));
    }
    if req.seeds.len() > MAX_SEEDS {
        return Err((StatusCode::BAD_REQUEST, "too many seeds"));
    }
    if req.exclude_track_ids.len() > MAX_EXCLUDE {
        return Err((StatusCode::BAD_REQUEST, "too many exclude_track_ids"));
    }

    let per_seed_n = req.per_seed_n.unwrap_or(DEFAULT_PER_SEED_N).clamp(1, MAX_N);
    let sample_size = req
        .sample_size
        .unwrap_or(DEFAULT_SAMPLE_SIZE)
        .clamp(1, req.seeds.len());
    let top_n = req.top_n.unwrap_or(DEFAULT_TOP_N).clamp(1, MAX_N);

    tracing::Span::current().record("seeds_total", req.seeds.len());
    tracing::Span::current().record("seeds_sampled", sample_size);

    let model_version = state.recommend_model_version().clone();

    // Pick `sample_size` random seed indices. RNG is request-scoped —
    // no shared state, no Mutex contention.
    let mut rng = SmallRng::from_entropy();
    let sampled: Vec<&str> = sample_indices(&mut rng, req.seeds.len(), sample_size)
        .into_iter()
        .map(|i| req.seeds[i].as_str())
        .collect();

    // Build the seed exclusion set up front so it covers BOTH the
    // sampled and the unsampled seeds — a track that's a non-sampled
    // playlist member shouldn't surface as a recommendation either.
    let mut exclude: HashSet<TrackId> =
        req.seeds.iter().map(|s| TrackId::from(s.as_str())).collect();
    for id in &req.exclude_track_ids {
        exclude.insert(TrackId::from(id.as_str()));
    }

    // Per-seed ANN queries. Each seed lookup is short and CPU-bound,
    // and the ANN takes its own internal RwLock; running these in
    // parallel via tokio tasks would not help. Sequential is simpler
    // and the cost is sample_size × ann.query (~100 µs at N=5000).
    let mut per_seed_results: Vec<Vec<music_recommend::ann::AnnQueryResult>> =
        Vec::with_capacity(sampled.len());
    let mut seeds_indexed = 0usize;
    for seed_str in &sampled {
        let seed_id = TrackId::from(*seed_str);
        let (vec, _src) = lookup_seed_vector(
            state.ann(),
            state.embedding_store(),
            &seed_id,
            &model_version,
        )
        .await;
        let Some(vector) = vec else {
            continue;
        };
        seeds_indexed += 1;
        match state.ann().query(&vector, per_seed_n) {
            Ok(hits) => per_seed_results.push(hits),
            Err(_) => return Err((StatusCode::INTERNAL_SERVER_ERROR, "ann query failed")),
        }
    }
    tracing::Span::current().record("seeds_indexed", seeds_indexed);

    let aggregated = aggregate_seed_results(&per_seed_results, &exclude, top_n);
    tracing::Span::current().record("results", aggregated.len());

    Ok(Json(FromSeedsResponse {
        model_version: Some(model_version.as_str().to_string()),
        degraded: false,
        results: aggregated
            .into_iter()
            .map(|a| RecommendItem {
                track_id: a.track_id.into_inner(),
                similarity: a.score,
            })
            .collect(),
        all_seeds_unindexed: seeds_indexed == 0,
    }))
}

// --- /v1/recommend/from-any ------------------------------------------
//
// First-indexed-wins single-seed fallback. The web client previously
// did this by looping over candidates and catching SeedNotEmbeddedError
// from each. Move the loop server-side so a 12-track album is one
// round-trip, not up to 12.

#[derive(Debug, Deserialize)]
pub struct FromAnyRequest {
    pub candidate_seeds: Vec<String>,
    #[serde(default = "default_n")]
    pub n: usize,
}

#[derive(Debug, Serialize)]
pub struct FromAnyResponse {
    pub seed_used: String,
    pub model_version: Option<String>,
    pub degraded: bool,
    pub results: Vec<RecommendItem>,
}

#[tracing::instrument(
    name = "recommend.from_any",
    skip_all,
    fields(
        candidates_total = tracing::field::Empty,
        candidates_tried = tracing::field::Empty,
        seed_used = tracing::field::Empty,
        results = tracing::field::Empty,
    ),
)]
pub async fn from_any(
    State(state): State<AppState>,
    payload: Result<Json<FromAnyRequest>, JsonRejection>,
) -> Result<Json<FromAnyResponse>, (StatusCode, &'static str)> {
    let Json(req) = payload.map_err(|_| (StatusCode::BAD_REQUEST, "invalid JSON body"))?;

    if req.candidate_seeds.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "candidate_seeds must not be empty"));
    }
    if req.candidate_seeds.len() > MAX_SEEDS {
        return Err((StatusCode::BAD_REQUEST, "too many candidate_seeds"));
    }
    if req.n == 0 {
        return Err((StatusCode::BAD_REQUEST, "n must be >= 1"));
    }
    let n = req.n.min(MAX_N);
    tracing::Span::current().record("candidates_total", req.candidate_seeds.len());

    let model_version = state.recommend_model_version().clone();

    // Try candidates in order, return first one with an ANN entry. Mirrors
    // the TS `startStationFromAny` semantics exactly: first-wins.
    let mut tried = 0usize;
    for cand in &req.candidate_seeds {
        tried += 1;
        let seed_id = TrackId::from(cand.as_str());
        let (vec, _src) = lookup_seed_vector(
            state.ann(),
            state.embedding_store(),
            &seed_id,
            &model_version,
        )
        .await;
        let Some(vector) = vec else {
            continue;
        };

        let results = state
            .ann()
            .query_excluding(&vector, n, std::slice::from_ref(&seed_id))
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "ann query failed"))?;

        tracing::Span::current().record("candidates_tried", tried);
        tracing::Span::current().record("seed_used", cand.as_str());
        tracing::Span::current().record("results", results.len());

        return Ok(Json(FromAnyResponse {
            seed_used: cand.clone(),
            model_version: Some(model_version.as_str().to_string()),
            degraded: false,
            results: results
                .into_iter()
                .map(|r| RecommendItem {
                    track_id: r.track_id.into_inner(),
                    similarity: r.similarity,
                })
                .collect(),
        }));
    }

    tracing::Span::current().record("candidates_tried", tried);
    Err((StatusCode::NOT_FOUND, "no candidate seed is indexed"))
}

/// Look up the seed's embedding. The ANN owns query-time state, so we
/// hit it first — that's the cheap path and avoids a SQLite read on
/// every recommend call. SQLite is the durable fallback in case the
/// ANN is mid-rebuild.
///
/// Returns `(vector, source)` where `source` is one of `"ann"`,
/// `"sqlite"`, or `"missing"` — recorded as a span field so
/// /diagnostics can show fallback rate at a glance.
async fn lookup_seed_vector(
    ann: &AnnIndex,
    store: &EmbeddingStore,
    seed: &TrackId,
    model_version: &ModelVersion,
) -> (Option<Vec<f32>>, &'static str) {
    if let Ok(Some(v)) = ann.get_vector(seed) {
        return (Some(v), "ann");
    }
    let key = EmbeddingKey::new(seed.clone(), model_version.clone());
    match store.get(&key).await.ok().flatten() {
        Some(emb) => (Some(emb.vector), "sqlite"),
        None => (None, "missing"),
    }
}
