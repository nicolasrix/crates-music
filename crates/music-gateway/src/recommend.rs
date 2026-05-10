//! Recommend endpoints + ingest glue for the gateway.
//!
//! - `GET /v1/recommend/next?seed=<id>&n=<N>` — top-N similar tracks.
//!   Returns 404 if the seed isn't in the ANN. Caps `n` at MAX_N to
//!   keep responses bounded.
//! - `POST /v1/recommend/enqueue {track_ids: [...]}` — admin-style
//!   enqueue for the ingest queue. Idempotent on (track_id,
//!   model_version): duplicates collapse via the store's
//!   INSERT-OR-IGNORE.

use std::collections::{HashMap, HashSet};

use axum::{
    Json,
    extract::{Query, State, rejection::JsonRejection},
    http::StatusCode,
    response::IntoResponse,
};
use music_core::TrackId;
use music_recommend::aggregate::{aggregate_seed_results, sample_indices};
use music_recommend::metadata::MetadataStore;
use music_recommend::queue_filter::{
    DiversityMode, FilterDecision, QueueFilter, QueueFilterConfig,
};
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
/// Hard cap on `queue_context.queue_track_ids`. A queue this long is
/// pathological — under any normal flow the upcoming queue is bounded
/// by `MIN_UPCOMING + history` (handful of items). 400 protects the
/// SQLite IN-clause and request payload.
const MAX_QUEUE: usize = 400;
const DEFAULT_SAMPLE_SIZE: usize = 8;
const DEFAULT_PER_SEED_N: usize = 20;
const DEFAULT_TOP_N: usize = 20;
/// Buffer factor: when a queue_context is supplied we ask the ANN /
/// aggregator for `top_n × FILTER_BUFFER_FACTOR` candidates so the
/// post-filter walk has headroom for cap/dedup rejects without a
/// second round-trip. Mirrors the web client's old `FETCH_BUFFER =
/// MIN_UPCOMING × 4`.
const FILTER_BUFFER_FACTOR: usize = 4;

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
    /// Optional queue snapshot to apply diversity filtering against.
    /// When `Some`, the server applies the per-artist cap and title
    /// dedup defined by the queue + the candidate's metadata; when
    /// `None`, results pass through unfiltered (legacy behaviour).
    #[serde(default)]
    pub queue_context: Option<QueueContext>,
}

/// Client-supplied queue snapshot for diversity filtering. Sent on
/// every refill — small (a few dozen ids × ~36 bytes) compared to
/// what's already on the wire for a recommend response.
///
/// The `now_playing_track_id` *counts* toward artist/title state but
/// is not itself excluded from results; everything else in
/// `queue_track_ids` is both counted and excluded.
#[derive(Debug, Default, Deserialize)]
pub struct QueueContext {
    #[serde(default)]
    pub queue_track_ids: Vec<String>,
    #[serde(default)]
    pub now_playing_track_id: Option<String>,
    /// `None` ⇒ default 2; `Some(0)` disables.
    #[serde(default)]
    pub max_per_artist: Option<u32>,
    /// `None` ⇒ default true.
    #[serde(default)]
    pub dedup_titles: Option<bool>,
    /// Selection algorithm. `None` ⇒ default (`hard_cap`). Accepted
    /// values match [`DiversityMode`] in lower-snake-case form.
    /// Marker is the `Option` itself, not the inner enum's `Default`,
    /// so a future redefinition of the default doesn't silently
    /// migrate old clients off the existing behaviour.
    #[serde(default)]
    pub diversity_mode: Option<DiversityModeWire>,
    /// MMR λ in [0, 1]. Only consulted when `diversity_mode == "mmr"`.
    /// Out-of-range values are clamped server-side rather than rejected.
    #[serde(default)]
    pub mmr_lambda: Option<f32>,
}

/// Wire-format mirror of [`DiversityMode`]. Lives in the HTTP layer so
/// the recommend crate doesn't take a serde dependency for a single
/// enum.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiversityModeWire {
    #[default]
    HardCap,
    Mmr,
    Off,
}

impl From<DiversityModeWire> for DiversityMode {
    fn from(w: DiversityModeWire) -> Self {
        match w {
            DiversityModeWire::HardCap => DiversityMode::HardCap,
            DiversityModeWire::Mmr => DiversityMode::Mmr,
            DiversityModeWire::Off => DiversityMode::Off,
        }
    }
}

impl QueueContext {
    fn config(&self) -> QueueFilterConfig {
        let defaults = QueueFilterConfig::default();
        QueueFilterConfig {
            diversity_mode: self
                .diversity_mode
                .map_or(defaults.diversity_mode, Into::into),
            mmr_lambda: self.mmr_lambda.unwrap_or(defaults.mmr_lambda),
            max_per_artist: self.max_per_artist.unwrap_or(defaults.max_per_artist),
            dedup_titles: self.dedup_titles.unwrap_or(defaults.dedup_titles),
        }
    }
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
        filter_capped = tracing::field::Empty,
        filter_dropped_artist = tracing::field::Empty,
        filter_dropped_dedup = tracing::field::Empty,
        filter_dropped_max_sim = tracing::field::Empty,
        filter_admitted_min_sim = tracing::field::Empty,
        filter_admitted_max_sim = tracing::field::Empty,
        filter_sim_gap = tracing::field::Empty,
        filter_admitted_sims_json = tracing::field::Empty,
        filter_dropped_sims_json = tracing::field::Empty,
    ),
)]
#[allow(clippy::too_many_lines)] // Validation + ANN fan-out + filter
                                 // dispatch + serialization are tightly
                                 // coupled at the HTTP boundary and
                                 // splitting them obscures the request
                                 // lifecycle.
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
    if let Some(qc) = &req.queue_context
        && qc.queue_track_ids.len() > MAX_QUEUE
    {
        return Err((StatusCode::BAD_REQUEST, "too many queue_track_ids"));
    }

    let per_seed_n = req.per_seed_n.unwrap_or(DEFAULT_PER_SEED_N).clamp(1, MAX_N);
    let sample_size = req
        .sample_size
        .unwrap_or(DEFAULT_SAMPLE_SIZE)
        .clamp(1, req.seeds.len());
    let top_n = req.top_n.unwrap_or(DEFAULT_TOP_N).clamp(1, MAX_N);
    // When a queue_context is present, ask the aggregator for more
    // than top_n so the post-filter walk has spare candidates to skip
    // past. Capped at MAX_N so we never blow the response.
    let internal_top_n = if req.queue_context.is_some() {
        top_n.saturating_mul(FILTER_BUFFER_FACTOR).min(MAX_N)
    } else {
        top_n
    };

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
    // Queue ids also flow into the same set so the aggregator never
    // even surfaces them; the QueueFilter then enforces the artist
    // cap + title dedup on what survives.
    let mut exclude: HashSet<TrackId> = req
        .seeds
        .iter()
        .map(|s| TrackId::from(s.as_str()))
        .collect();
    for id in &req.exclude_track_ids {
        exclude.insert(TrackId::from(id.as_str()));
    }
    if let Some(qc) = &req.queue_context {
        for id in &qc.queue_track_ids {
            // The now-playing track is *not* excluded — it counts toward
            // the artist cap (built later) but might surface as its own
            // close neighbour without being a problem.
            if Some(id.as_str()) != qc.now_playing_track_id.as_deref() {
                exclude.insert(TrackId::from(id.as_str()));
            }
        }
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

    let aggregated = aggregate_seed_results(&per_seed_results, &exclude, internal_top_n);

    let (filtered, filter_stats) = match &req.queue_context {
        Some(qc) => {
            apply_queue_filter_to_aggregate(
                state.metadata_store(),
                state.ann(),
                qc,
                aggregated,
                top_n,
            )
            .await
        }
        None => (aggregated, FilterStats::default()),
    };
    tracing::Span::current().record("results", filtered.len());
    record_filter_stats(&filter_stats);

    Ok(Json(FromSeedsResponse {
        model_version: Some(model_version.as_str().to_string()),
        degraded: false,
        results: filtered
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
    /// See [`QueueContext`] — same semantics as in [`FromSeedsRequest`].
    #[serde(default)]
    pub queue_context: Option<QueueContext>,
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
        filter_capped = tracing::field::Empty,
        filter_dropped_artist = tracing::field::Empty,
        filter_dropped_dedup = tracing::field::Empty,
        filter_dropped_max_sim = tracing::field::Empty,
        filter_admitted_min_sim = tracing::field::Empty,
        filter_admitted_max_sim = tracing::field::Empty,
        filter_sim_gap = tracing::field::Empty,
        filter_admitted_sims_json = tracing::field::Empty,
        filter_dropped_sims_json = tracing::field::Empty,
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
    if let Some(qc) = &req.queue_context
        && qc.queue_track_ids.len() > MAX_QUEUE
    {
        return Err((StatusCode::BAD_REQUEST, "too many queue_track_ids"));
    }
    let n = req.n.min(MAX_N);
    // Same buffer rationale as in from_seeds: ask the ANN for a wider
    // top-K so the post-filter walk has room.
    let internal_n = if req.queue_context.is_some() {
        n.saturating_mul(FILTER_BUFFER_FACTOR).min(MAX_N)
    } else {
        n
    };
    tracing::Span::current().record("candidates_total", req.candidate_seeds.len());

    let model_version = state.recommend_model_version().clone();

    // Build the ANN exclusion list once: the chosen seed *plus* any
    // queue ids the client gave us (minus now-playing). Snapshot once
    // — `query_excluding` takes a slice.
    let queue_excludes: Vec<TrackId> = match &req.queue_context {
        Some(qc) => qc
            .queue_track_ids
            .iter()
            .filter(|id| Some(id.as_str()) != qc.now_playing_track_id.as_deref())
            .map(|s| TrackId::from(s.as_str()))
            .collect(),
        None => Vec::new(),
    };

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

        // Build the exclude slice for this query: seed + queue items.
        let mut excludes_for_query: Vec<TrackId> = Vec::with_capacity(queue_excludes.len() + 1);
        excludes_for_query.push(seed_id.clone());
        excludes_for_query.extend(queue_excludes.iter().cloned());

        let results = state
            .ann()
            .query_excluding(&vector, internal_n, &excludes_for_query)
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "ann query failed"))?;

        let (filtered, filter_stats) = match &req.queue_context {
            Some(qc) => {
                apply_queue_filter_to_ann(state.metadata_store(), state.ann(), qc, results, n).await
            }
            None => (results, FilterStats::default()),
        };

        tracing::Span::current().record("candidates_tried", tried);
        tracing::Span::current().record("seed_used", cand.as_str());
        tracing::Span::current().record("results", filtered.len());
        record_filter_stats(&filter_stats);

        return Ok(Json(FromAnyResponse {
            seed_used: cand.clone(),
            model_version: Some(model_version.as_str().to_string()),
            degraded: false,
            results: filtered
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

/// Build a [`QueueFilter`] from a request's `queue_context` plus the
/// metadata store. One SQLite round-trip — the union of queue ids and
/// candidate ids — feeds both queue-state seeding and per-candidate
/// gating.
async fn build_filter_with_metadata(
    metadata_store: &MetadataStore,
    qc: &QueueContext,
    candidate_ids: &[TrackId],
) -> (
    QueueFilter,
    std::collections::HashMap<TrackId, music_recommend::TrackMetadata>,
) {
    let queue_ids: Vec<TrackId> = qc
        .queue_track_ids
        .iter()
        .map(|s| TrackId::from(s.as_str()))
        .collect();

    // Single fetch that covers queue + candidates. Misses fall through
    // to "no metadata" → the filter passes the candidate, which is the
    // safe default during the cache backfill window.
    let mut needed: Vec<TrackId> = Vec::with_capacity(queue_ids.len() + candidate_ids.len());
    needed.extend(queue_ids.iter().cloned());
    needed.extend(candidate_ids.iter().cloned());
    // De-dupe to avoid useless work in the IN-clause; HashSet is cheap
    // at this scale.
    let dedup: HashSet<TrackId> = needed.into_iter().collect();
    let lookup_ids: Vec<TrackId> = dedup.into_iter().collect();

    let metadata = metadata_store
        .get_many(&lookup_ids)
        .await
        .unwrap_or_else(|err| {
            // SQLite hiccup: log and degrade to "no metadata cached".
            // Filter still works — it just admits everything that the
            // queue exclusion didn't catch.
            tracing::warn!(error = %err, "metadata lookup failed; degrading queue_filter");
            std::collections::HashMap::new()
        });

    let now_playing = qc.now_playing_track_id.as_deref().map(TrackId::from);
    let filter = QueueFilter::build(&queue_ids, now_playing.as_ref(), &metadata, qc.config());
    (filter, metadata)
}

/// Per-request filter telemetry. Captured during the post-filter walk
/// and recorded onto the request span so /diagnostics can show "did
/// the filter cost us anything in this slate?".
///
/// Two layers of detail end up on the span:
///
/// 1. **Scalars** — `dropped_artist`, `dropped_dedup`,
///    `admitted_min_sim`, `admitted_max_sim`, `dropped_max_sim`,
///    `sim_gap`. Kept as direct span fields so the existing
///    /diagnostics histograms can `json_extract` them in O(1) without
///    iterating an array.
/// 2. **Per-track arrays** — `admitted_sims_json`, `dropped_sims_json`.
///    Recorded as compact JSON arrays of f32. Lets ad-hoc queries
///    compute any aggregate (mean, p50, p95, stdev) over the slate
///    distribution after the fact, without baking each new aggregate
///    into the recorder.
///
/// The headline scalar is [`Self::sim_gap`]: when positive, the filter
/// rejected a candidate stronger than the worst we admitted — a direct
/// proxy for the "genre jump" symptom we're trying to characterise
/// before tuning the algorithm.
#[derive(Debug, Default)]
struct FilterStats {
    dropped_artist: u32,
    dropped_dedup: u32,
    /// Per-admit similarity-to-seed. Length equals the admitted slate
    /// size (≤ `top_n`).
    admitted_sims: Vec<f32>,
    /// Per-drop similarity-to-seed. Length equals `total_dropped()`.
    /// Bounded by `internal_top_n - admitted` ≤ MAX_N - top_n.
    dropped_sims: Vec<f32>,
}

impl FilterStats {
    fn total_dropped(&self) -> u32 {
        self.dropped_artist + self.dropped_dedup
    }

    fn record_admit(&mut self, sim: f32) {
        self.admitted_sims.push(sim);
    }

    fn record_drop(&mut self, decision: FilterDecision, sim: f32) {
        match decision {
            FilterDecision::RejectArtistCap => self.dropped_artist += 1,
            FilterDecision::RejectDedup => self.dropped_dedup += 1,
            // Accept never reaches here — the apply loop only forwards
            // rejection decisions to `record_drop`. Defensive no-op.
            FilterDecision::Accept => return,
        }
        self.dropped_sims.push(sim);
    }

    /// Lowest similarity in the admitted slate. `None` when no admits.
    fn admitted_min(&self) -> Option<f32> {
        self.admitted_sims.iter().copied().reduce(f32::min)
    }

    /// Highest similarity in the admitted slate. `None` when no admits.
    fn admitted_max(&self) -> Option<f32> {
        self.admitted_sims.iter().copied().reduce(f32::max)
    }

    /// Highest similarity rejected by artist cap or dedup. `None` when
    /// nothing was dropped this request.
    fn dropped_max(&self) -> Option<f32> {
        self.dropped_sims.iter().copied().reduce(f32::max)
    }

    /// Spread between best dropped and worst admitted. Positive when
    /// the filter forced a worse pick than something it rejected.
    fn sim_gap(&self) -> Option<f32> {
        match (self.dropped_max(), self.admitted_min()) {
            (Some(d), Some(a)) => Some(d - a),
            _ => None,
        }
    }
}

/// Compact JSON array formatter for an `&[f32]`. Hand-rolled (rather
/// than `serde_json`) so the seam stays dependency-light and the
/// formatting is deterministic — `format!("{v:.6}")` always emits
/// fixed-decimal notation, no scientific-notation switching at the
/// edges of the f32 range.
///
/// 6 decimal places exceeds the effective precision of cosine sims
/// computed from 512-dim CLAP embeddings (which are themselves stored
/// as f32 to begin with), so no information is lost.
fn sims_as_json(sims: &[f32]) -> String {
    use std::fmt::Write as _;
    // ~9 chars per value plus brackets and commas. Slight over-estimate
    // is fine; saves the realloc on the common 20-element case.
    let mut out = String::with_capacity(sims.len() * 10 + 2);
    out.push('[');
    for (i, v) in sims.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "{v:.6}");
    }
    out.push(']');
    out
}

/// Push every collected stat onto the current span. Optional fields are
/// only recorded when populated, so old-shape consumers / empty-result
/// requests don't see misleading zeros for "best-dropped similarity".
fn record_filter_stats(stats: &FilterStats) {
    let span = tracing::Span::current();
    span.record("filter_capped", stats.total_dropped());
    span.record("filter_dropped_artist", stats.dropped_artist);
    span.record("filter_dropped_dedup", stats.dropped_dedup);
    // tracing's `Value` impl covers f64 but not f32 — cast at the seam.
    if let Some(v) = stats.dropped_max() {
        span.record("filter_dropped_max_sim", f64::from(v));
    }
    if let Some(v) = stats.admitted_min() {
        span.record("filter_admitted_min_sim", f64::from(v));
    }
    if let Some(v) = stats.admitted_max() {
        span.record("filter_admitted_max_sim", f64::from(v));
    }
    if let Some(v) = stats.sim_gap() {
        span.record("filter_sim_gap", f64::from(v));
    }
    // Per-track arrays only get recorded when non-empty. Empty arrays
    // would still be valid JSON (`"[]"`) but adding a second
    // distinguishable case (field present + empty vs. absent) to the
    // diagnostics queries buys nothing.
    if !stats.admitted_sims.is_empty() {
        span.record(
            "filter_admitted_sims_json",
            sims_as_json(&stats.admitted_sims).as_str(),
        );
    }
    if !stats.dropped_sims.is_empty() {
        span.record(
            "filter_dropped_sims_json",
            sims_as_json(&stats.dropped_sims).as_str(),
        );
    }
}

/// Field-name-agnostic view over the two recommendation candidate
/// types. Lets [`apply_queue_filter_generic`] handle both
/// `AggregatedResult` (multi-seed `from-seeds`) and `AnnQueryResult`
/// (single-seed `from-any`) without duplicating the diversity-mode
/// match for each.
trait CandidateLike: Clone {
    fn track_id(&self) -> &TrackId;
    fn sim(&self) -> f32;
}

impl CandidateLike for music_recommend::aggregate::AggregatedResult {
    fn track_id(&self) -> &TrackId {
        &self.track_id
    }
    fn sim(&self) -> f32 {
        self.score
    }
}

impl CandidateLike for music_recommend::ann::AnnQueryResult {
    fn track_id(&self) -> &TrackId {
        &self.track_id
    }
    fn sim(&self) -> f32 {
        self.similarity
    }
}

/// How much of the over-fetched candidate pool MMR is allowed to consider.
/// MMR runs greedy on `top_n × this_factor` post-exclusion candidates so
/// the artist-cap safety net (run *after* MMR) has room to drop without
/// starving the slate. Kept conservative — MMR is `O(top_n × pool ×
/// dim)`, so a too-large pool would dominate the per-call latency. With
/// `top_n=20`, `dim=512`, factor=2, pool=40, the inner loop is ~400 K
/// float ops, still well under the 1.5 ms budget.
const MMR_POOL_BUFFER_FACTOR: usize = 2;

/// Diversity-mode-aware slate selection. Owns the `match` over
/// [`DiversityMode`] so the two HTTP handlers don't each re-implement
/// the dispatch.
async fn apply_queue_filter_generic<C: CandidateLike>(
    metadata_store: &MetadataStore,
    ann: &AnnIndex,
    qc: &QueueContext,
    candidates: Vec<C>,
    top_n: usize,
) -> (Vec<C>, FilterStats) {
    let candidate_ids: Vec<TrackId> = candidates.iter().map(|c| c.track_id().clone()).collect();
    let (filter, metadata) =
        build_filter_with_metadata(metadata_store, qc, &candidate_ids).await;
    let cfg = qc.config();

    match cfg.diversity_mode {
        DiversityMode::HardCap => walk_hard_cap(candidates, &metadata, filter, top_n),
        DiversityMode::Mmr => walk_mmr(candidates, &metadata, filter, ann, cfg.mmr_lambda, top_n),
        DiversityMode::Off => walk_off(candidates, &filter, top_n),
    }
}

/// Hard-cap (legacy) path: walk candidates in input/relevance order,
/// admit each that survives [`QueueFilter::try_accept`]. Behaviourally
/// identical to the pre-MMR implementation — kept as the default.
fn walk_hard_cap<C: CandidateLike>(
    candidates: Vec<C>,
    metadata: &HashMap<TrackId, music_recommend::TrackMetadata>,
    mut filter: QueueFilter,
    top_n: usize,
) -> (Vec<C>, FilterStats) {
    let mut out = Vec::with_capacity(top_n);
    let mut stats = FilterStats::default();
    for c in candidates {
        if out.len() >= top_n {
            break;
        }
        if filter.is_excluded(c.track_id()) {
            continue;
        }
        match filter.try_accept(metadata.get(c.track_id())) {
            FilterDecision::Accept => {
                stats.record_admit(c.sim());
                out.push(c);
            }
            d => stats.record_drop(d, c.sim()),
        }
    }
    (out, stats)
}

/// MMR path: re-rank surviving candidates, then run them through the
/// queue filter as a safety net so a degenerate λ or embedding cluster
/// can't violate the per-artist cap.
///
/// Vector lookup is per-id via [`AnnIndex::get_vector`] — a missed
/// lookup leaves the candidate's diversity term at 0, scoring on
/// relevance alone. That's the same fail-open behaviour the cap path
/// has for missing metadata.
fn walk_mmr<C: CandidateLike>(
    candidates: Vec<C>,
    metadata: &HashMap<TrackId, music_recommend::TrackMetadata>,
    mut filter: QueueFilter,
    ann: &AnnIndex,
    lambda: f32,
    top_n: usize,
) -> (Vec<C>, FilterStats) {
    // 1. Drop excluded candidates up front. No point spending the
    //    vector lookup on tracks the user already has queued.
    let surviving: Vec<C> = candidates
        .into_iter()
        .filter(|c| !filter.is_excluded(c.track_id()))
        .collect();

    // 2. Hydrate vectors. ANN holds them in mmap'd HNSW, so each
    //    lookup is a hash + memcpy of `dim` f32s. At the typical
    //    `surviving.len() ≤ top_n × buffer × cap_buffer` (≤ 80) this
    //    is ~80 µs per call — under the latency budget.
    let mmr_inputs: Vec<music_recommend::MmrCandidate> = surviving
        .iter()
        .map(|c| music_recommend::MmrCandidate {
            track_id: c.track_id().clone(),
            sim_to_seed: c.sim(),
            vector: ann.get_vector(c.track_id()).ok().flatten(),
        })
        .collect();

    // 3. Re-rank. Ask for `top_n × MMR_POOL_BUFFER_FACTOR` so the
    //    safety-net try_accept walk below has spare candidates to skip
    //    past on a cap/dedup hit.
    let want = top_n.saturating_mul(MMR_POOL_BUFFER_FACTOR);
    let order = music_recommend::mmr_rerank(&mmr_inputs, lambda, want);

    // 4. Apply cap + dedup safety net in MMR-ordered sequence.
    let mut out = Vec::with_capacity(top_n);
    let mut stats = FilterStats::default();
    for idx in order {
        if out.len() >= top_n {
            break;
        }
        let c = &surviving[idx];
        match filter.try_accept(metadata.get(c.track_id())) {
            FilterDecision::Accept => {
                stats.record_admit(c.sim());
                out.push(c.clone());
            }
            d => stats.record_drop(d, c.sim()),
        }
    }
    (out, stats)
}

/// Off path: no diversity gating beyond the queue exclusion list.
/// Useful as an A/B baseline when measuring whether MMR or HardCap is
/// pulling its weight.
fn walk_off<C: CandidateLike>(
    candidates: Vec<C>,
    filter: &QueueFilter,
    top_n: usize,
) -> (Vec<C>, FilterStats) {
    let mut out = Vec::with_capacity(top_n);
    let mut stats = FilterStats::default();
    for c in candidates {
        if out.len() >= top_n {
            break;
        }
        if filter.is_excluded(c.track_id()) {
            continue;
        }
        stats.record_admit(c.sim());
        out.push(c);
    }
    (out, stats)
}

/// Apply the [`QueueFilter`] to aggregated `from-seeds` results. Returns
/// the surviving items truncated to `top_n`, plus per-request
/// [`FilterStats`] for the request span.
async fn apply_queue_filter_to_aggregate(
    metadata_store: &MetadataStore,
    ann: &AnnIndex,
    qc: &QueueContext,
    candidates: Vec<music_recommend::aggregate::AggregatedResult>,
    top_n: usize,
) -> (
    Vec<music_recommend::aggregate::AggregatedResult>,
    FilterStats,
) {
    apply_queue_filter_generic(metadata_store, ann, qc, candidates, top_n).await
}

/// Apply the [`QueueFilter`] to ANN `from-any` results. Same shape as
/// [`apply_queue_filter_to_aggregate`] but typed for `AnnQueryResult`.
async fn apply_queue_filter_to_ann(
    metadata_store: &MetadataStore,
    ann: &AnnIndex,
    qc: &QueueContext,
    candidates: Vec<music_recommend::ann::AnnQueryResult>,
    top_n: usize,
) -> (Vec<music_recommend::ann::AnnQueryResult>, FilterStats) {
    apply_queue_filter_generic(metadata_store, ann, qc, candidates, top_n).await
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

#[cfg(test)]
mod tests {
    //! Pure-Rust tests for the diagnostics helpers. The handler-level
    //! integration coverage lives in `tests/recommend.rs`; here we only
    //! exercise the stat-aggregation seam that decides what ends up in
    //! the trace store's `fields_json`.
    use super::*;

    #[test]
    fn sims_as_json_empty_returns_bracket_pair() {
        assert_eq!(sims_as_json(&[]), "[]");
    }

    #[test]
    fn sims_as_json_single_value_uses_six_decimals() {
        assert_eq!(sims_as_json(&[0.5_f32]), "[0.500000]");
    }

    #[test]
    fn sims_as_json_multiple_values_are_comma_separated_no_spaces() {
        // Compactness matters: this string lands in fields_json; spaces
        // are wasted bytes at 100k-row scale. Trailing zeros come from
        // the `{:.6}` formatter, not from the literals.
        assert_eq!(
            sims_as_json(&[0.84_f32, 0.8125_f32, 0.5_f32]),
            "[0.840000,0.812500,0.500000]"
        );
    }

    #[test]
    fn sims_as_json_handles_negative_values() {
        // Cosine sims are bounded in [-1, 1]; negatives are legal.
        assert_eq!(sims_as_json(&[-0.25_f32, 0.75_f32]), "[-0.250000,0.750000]");
    }

    #[test]
    fn filter_stats_default_has_no_admits_or_drops() {
        let s = FilterStats::default();
        assert!(s.admitted_sims.is_empty());
        assert!(s.dropped_sims.is_empty());
        assert_eq!(s.total_dropped(), 0);
        assert!(s.admitted_min().is_none());
        assert!(s.admitted_max().is_none());
        assert!(s.dropped_max().is_none());
        assert!(s.sim_gap().is_none());
    }

    #[test]
    fn record_admit_appends_to_admitted_sims() {
        let mut s = FilterStats::default();
        s.record_admit(0.9);
        s.record_admit(0.7);
        s.record_admit(0.8);
        assert_eq!(s.admitted_sims, vec![0.9, 0.7, 0.8]);
    }

    #[test]
    fn admitted_min_max_track_extremes_across_admits() {
        let mut s = FilterStats::default();
        for v in [0.9_f32, 0.7, 0.85, 0.6, 0.95] {
            s.record_admit(v);
        }
        // Comparisons are on f32 so we use exact-equal: inputs are
        // small fractions with exact f32 representations of the
        // float-literal kind, which is true of the chosen values.
        assert_eq!(s.admitted_min(), Some(0.6));
        assert_eq!(s.admitted_max(), Some(0.95));
    }

    #[test]
    fn record_drop_increments_per_decision_counter() {
        let mut s = FilterStats::default();
        s.record_drop(FilterDecision::RejectArtistCap, 0.85);
        s.record_drop(FilterDecision::RejectArtistCap, 0.80);
        s.record_drop(FilterDecision::RejectDedup, 0.78);
        assert_eq!(s.dropped_artist, 2);
        assert_eq!(s.dropped_dedup, 1);
        assert_eq!(s.total_dropped(), 3);
        assert_eq!(s.dropped_sims, vec![0.85, 0.80, 0.78]);
        assert_eq!(s.dropped_max(), Some(0.85));
    }

    #[test]
    fn record_drop_accept_variant_is_a_noop() {
        // Defensive: if a caller ever forwards an Accept here, it
        // shouldn't mutate counters. The apply loop already handles
        // Accept separately, but the contract should hold.
        let mut s = FilterStats::default();
        s.record_drop(FilterDecision::Accept, 0.5);
        assert_eq!(s.total_dropped(), 0);
        assert!(s.dropped_sims.is_empty());
    }

    #[test]
    fn sim_gap_is_dropped_max_minus_admitted_min() {
        let mut s = FilterStats::default();
        s.record_admit(0.7);
        s.record_admit(0.8);
        s.record_drop(FilterDecision::RejectArtistCap, 0.85);
        s.record_drop(FilterDecision::RejectArtistCap, 0.82);
        // best dropped (0.85) - worst admitted (0.7) = 0.15
        let gap = s.sim_gap().expect("gap defined");
        assert!(
            (gap - 0.15_f32).abs() < 1e-6,
            "expected ~0.15, got {gap}"
        );
    }

    #[test]
    fn sim_gap_is_none_when_either_side_is_empty() {
        // Admits but no drops → no gap.
        let mut a = FilterStats::default();
        a.record_admit(0.7);
        assert!(a.sim_gap().is_none());

        // Drops but no admits → no gap (the "filter starved the slate"
        // case the user is investigating).
        let mut b = FilterStats::default();
        b.record_drop(FilterDecision::RejectArtistCap, 0.85);
        assert!(b.sim_gap().is_none());
    }
}
