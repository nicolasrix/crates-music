//! Recommend benchmark — drives the in-process recommend pipeline against
//! real (gateway-owned) state and emits aggregate quality metrics.
//!
//! ## Why this exists
//!
//! `cargo bench` and the criterion suites under `crates/music-recommend`
//! measure *latency*: did this PR make ANN queries slower? They don't
//! answer the question the FilterStats telemetry was added for: did this
//! PR make the recommendations *better*? "Better" here is a multi-axis
//! quality vector — mean similarity to seed, distribution spread, "tax"
//! charged by the diversity filter, slate fill rate when the cap fires —
//! and measuring it requires running many recommendation calls against
//! the same population of tracks, then aggregating the FilterStats
//! we already capture per call.
//!
//! ## What it does
//!
//! 1. Boots from a gateway TOML so it picks up the same SQLite + ANN
//!    files the live gateway uses. Read-only-friendly: SQLite WAL +
//!    mmap'd HNSW both tolerate a parallel reader, so the gateway can
//!    keep running.
//! 2. Picks `--iterations` random embedded tracks (deterministic via
//!    `--rng-seed`). For each:
//!    - **`--queue-mode cold`**: empty queue context. Models the
//!      "first track into a fresh station" case. The cap rarely fires
//!      because no artist is yet at saturation.
//!    - **`--queue-mode saturated`**: pre-fills the queue with the
//!      seed track plus `max_per_artist - 1` more tracks by the same
//!      artist (when available), pushing that artist to the cap.
//!      Forces the cap to fire on the next refill — this is where
//!      the diversity-tax measurement actually has signal.
//! 3. Runs the same code path the production handler uses
//!    (`AnnIndex::query_excluding` → `QueueFilter` walk over the
//!    candidates), records the FilterStats arrays + per-call wall
//!    clock.
//! 4. Aggregates: mean / p50 / p95 of admit similarity, drop similarity,
//!    per-call true tax, latency. Emits both a console table and an
//!    optional JSON snapshot for diff-comparing two runs.
//!
//! ## Reproducibility
//!
//! The `--rng-seed` flag seeds a `SmallRng`; the same seed value across
//! two runs produces the same sample of seed tracks. That means
//! comparing "old recommender vs new recommender" becomes a direct A/B:
//! same 200 calls, same inputs, only the configured knobs differ.
//!
//! Because the seed *list* is deterministic but the underlying ANN /
//! metadata can drift between runs (new tracks ingested, embeddings
//! refreshed), the JSON snapshot stamps the embedded-track count and
//! model version so a reader can spot population changes.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, ValueEnum};
use music_core::TrackId;
use music_gateway::Config;
use music_recommend::ann::AnnIndex;
use music_recommend::metadata::{MetadataStore, TrackMetadata};
use music_recommend::queue_filter::{
    DiversityMode, FilterDecision, QueueFilter, QueueFilterConfig,
};
use music_recommend::store::EmbeddingStore;
use music_recommend::types::ModelVersion;
use music_recommend::{MmrCandidate, mmr_rerank};
use rand::SeedableRng;
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use serde::Serialize;
use sqlx::Row;

/// HNSW parameters must match what the gateway opens the index with —
/// usearch will error or produce wrong results if connectivity differs.
const ANN_DIM: usize = 512;
const ANN_CONNECTIVITY: usize = 16;
/// Internal ANN top-K multiplier when a queue context is supplied.
/// Mirrors `FILTER_BUFFER_FACTOR` in `recommend.rs`. We cap at this
/// value so the post-filter walk has the same headroom the production
/// handler gets.
const FILTER_BUFFER_FACTOR: usize = 4;
const MAX_INTERNAL_N: usize = 100;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum QueueMode {
    /// Empty queue context — the cap rarely fires.
    Cold,
    /// Pre-fill the queue with same-artist tracks so the cap is at
    /// saturation. Forces the diversity filter to engage.
    Saturated,
}

/// CLI mirror of [`DiversityMode`]. Lives here (not in the recommend
/// crate) so the bench owns its `clap` derive without forcing the
/// library to take a `clap` dependency.
#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
enum DiversityModeArg {
    HardCap,
    Mmr,
    Off,
}

impl From<DiversityModeArg> for DiversityMode {
    fn from(a: DiversityModeArg) -> Self {
        match a {
            DiversityModeArg::HardCap => DiversityMode::HardCap,
            DiversityModeArg::Mmr => DiversityMode::Mmr,
            DiversityModeArg::Off => DiversityMode::Off,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "recommend-bench", about = "Recommender quality benchmark")]
struct Args {
    /// Gateway config TOML. We read SQLite + ANN paths from it.
    #[arg(short, long)]
    config: PathBuf,

    /// Number of iterations (random seed picks) to run.
    #[arg(short, long, default_value_t = 200)]
    iterations: usize,

    /// RNG seed for reproducible seed-track sampling.
    #[arg(long, default_value_t = 42)]
    rng_seed: u64,

    /// Queue context mode for each iteration.
    #[arg(long, value_enum, default_value_t = QueueMode::Saturated)]
    queue_mode: QueueMode,

    /// Final slate cap (matches the gateway handler's `n` parameter).
    #[arg(long, default_value_t = 20)]
    top_n: usize,

    /// Per-artist cap. Default matches the production default.
    #[arg(long, default_value_t = 2)]
    max_per_artist: u32,

    /// Whether title dedup is enabled (matches production default).
    #[arg(long, default_value_t = true)]
    dedup_titles: bool,

    /// Slate selection algorithm. `hard_cap` reproduces the legacy
    /// behaviour and is the default. `mmr` re-ranks using
    /// [`MmrCandidate`] vectors. `off` disables diversity gating
    /// (queue exclusion still runs).
    #[arg(long, value_enum, default_value_t = DiversityModeArg::HardCap)]
    diversity_mode: DiversityModeArg,

    /// MMR λ in [0, 1]. Only consulted when `diversity_mode == mmr`.
    #[arg(long, default_value_t = 0.7)]
    lambda: f32,

    /// Soft same-artist penalty `μ` for MMR. `0.0` disables; the
    /// production default is 0.15. Stacks linearly per same-artist
    /// admit. Only consulted when `diversity_mode == mmr`.
    #[arg(long, default_value_t = 0.15)]
    artist_penalty_weight: f32,

    /// Override the model version. Defaults to the version with the
    /// most `done` rows in `track_embeddings` — i.e. the active one.
    #[arg(long)]
    model_version: Option<String>,

    /// Optional JSON output path. The console table prints regardless.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config =
        Config::load(&args.config).with_context(|| format!("loading {}", args.config.display()))?;

    // Mirror the gateway's path conventions: oauth.state_db is the
    // anchor, .recommend.sqlite + .ann hang off it.
    let recommend_db = config.oauth.state_db.with_extension("recommend.sqlite");
    let ann_path = config.oauth.state_db.with_extension("ann");

    eprintln!("opening recommend DB: {}", recommend_db.display());
    let embedding_store = EmbeddingStore::open(&recommend_db)
        .await
        .with_context(|| format!("opening {}", recommend_db.display()))?;
    let metadata_store = MetadataStore::new(embedding_store.pool().clone());

    eprintln!("opening ANN: {}", ann_path.display());
    let ann = AnnIndex::open(&ann_path, ANN_DIM, ANN_CONNECTIVITY)
        .with_context(|| format!("opening ANN {}", ann_path.display()))?;
    eprintln!("ANN size: {} vectors", ann.len()?);

    let model_version = match &args.model_version {
        Some(v) => ModelVersion::from(v.as_str()),
        None => active_model_version(&embedding_store)
            .await
            .context("auto-detecting active model_version")?,
    };
    eprintln!("model_version: {model_version}");

    let seed_pool = sample_seed_pool(
        &embedding_store,
        &model_version,
        args.iterations,
        args.rng_seed,
    )
    .await
    .context("sampling seed track ids")?;
    eprintln!("seed pool: {} tracks", seed_pool.len());
    if seed_pool.is_empty() {
        return Err(anyhow!(
            "no embedded tracks with status='done' found for model {model_version} — \
             run the gateway long enough to populate embeddings, or pass --model-version"
        ));
    }

    let cfg = QueueFilterConfig {
        diversity_mode: args.diversity_mode.into(),
        mmr_lambda: args.lambda,
        artist_penalty_weight: args.artist_penalty_weight,
        max_per_artist: args.max_per_artist,
        dedup_titles: args.dedup_titles,
    };

    let mut rows: Vec<IterRecord> = Vec::with_capacity(seed_pool.len());
    let mut skipped_no_metadata = 0usize;
    let mut skipped_no_vector = 0usize;
    let started = Instant::now();
    for (i, seed_id) in seed_pool.iter().enumerate() {
        match run_one(
            &ann,
            &metadata_store,
            seed_id,
            args.queue_mode,
            args.top_n,
            cfg,
        )
        .await?
        {
            IterOutcome::Recorded(rec) => rows.push(rec),
            IterOutcome::SkippedNoMetadata => skipped_no_metadata += 1,
            IterOutcome::SkippedNoVector => skipped_no_vector += 1,
        }
        if (i + 1) % 25 == 0 {
            eprintln!("  {} / {} iterations", i + 1, seed_pool.len());
        }
    }
    let total_wall_ms = started.elapsed().as_secs_f64() * 1000.0;

    let report = build_report(
        &args,
        &model_version,
        ann.len().unwrap_or(0),
        seed_pool.len(),
        skipped_no_metadata,
        skipped_no_vector,
        total_wall_ms,
        &rows,
    );

    print_table(&report);

    if let Some(out_path) = &args.output {
        let json = serde_json::to_string_pretty(&report).context("serializing report")?;
        std::fs::write(out_path, json)
            .with_context(|| format!("writing {}", out_path.display()))?;
        eprintln!("wrote {}", out_path.display());
    }

    Ok(())
}

// --- per-iteration plumbing ------------------------------------------

enum IterOutcome {
    Recorded(IterRecord),
    SkippedNoMetadata,
    SkippedNoVector,
}

/// Single-call snapshot. Mirrors what the gateway's `FilterStats`
/// would have captured — but we own the array entirely (rather than
/// going through the trace store JSON round-trip), and we add a
/// per-call wall-clock duration.
struct IterRecord {
    admitted_sims: Vec<f32>,
    dropped_sims: Vec<f32>,
    dropped_artist: u32,
    dropped_dedup: u32,
    elapsed_us: u64,
}

async fn run_one(
    ann: &AnnIndex,
    metadata: &MetadataStore,
    seed_id: &TrackId,
    queue_mode: QueueMode,
    top_n: usize,
    cfg: QueueFilterConfig,
) -> Result<IterOutcome> {
    // 1. Need the seed's metadata both to build the saturated queue
    //    AND because the QueueFilter wants it for the now_playing
    //    artist count. Skip if missing — exercising the filter against
    //    an unindexed artist isn't representative of production load.
    let Some(seed_meta) = metadata.get(seed_id).await? else {
        return Ok(IterOutcome::SkippedNoMetadata);
    };

    // 2. Build the queue snapshot.
    let queue_track_ids = match queue_mode {
        QueueMode::Cold => Vec::new(),
        QueueMode::Saturated => build_saturated_queue(metadata, seed_id, &seed_meta, cfg).await?,
    };

    // 3. Get seed vector. ANN-first, no SQLite fallback (the bench
    //    tests the active path, not the rebuild-from-store path).
    let Some(seed_vector) = ann.get_vector(seed_id)? else {
        return Ok(IterOutcome::SkippedNoVector);
    };

    // 4. Build excludes for the ANN query: seed + queue items minus
    //    now-playing. Mirrors `from_any` in the production handler.
    let now_playing = if queue_track_ids.is_empty() {
        None
    } else {
        Some(seed_id.clone())
    };
    let mut excludes: Vec<TrackId> = Vec::with_capacity(queue_track_ids.len() + 1);
    excludes.push(seed_id.clone());
    for id in &queue_track_ids {
        if Some(id) != now_playing.as_ref() {
            excludes.push(id.clone());
        }
    }

    // 5. Internal_n: same buffer as production when the filter is
    //    going to walk the candidates. For cold mode the filter
    //    rarely drops anything, but use the same N so the bench is
    //    apples-to-apples between modes.
    let internal_n = top_n
        .saturating_mul(FILTER_BUFFER_FACTOR)
        .min(MAX_INTERNAL_N);

    // --- timer starts: ANN query + metadata fetch + filter walk ---
    let t0 = Instant::now();

    let candidates = ann.query_excluding(&seed_vector, internal_n, &excludes)?;

    // 6. Build the per-candidate metadata lookup. Single SQLite hit
    //    that covers queue ids + candidate ids — same shape as the
    //    handler's `build_filter_with_metadata`.
    let mut needed: Vec<TrackId> = Vec::with_capacity(queue_track_ids.len() + candidates.len());
    needed.extend(queue_track_ids.iter().cloned());
    needed.extend(candidates.iter().map(|c| c.track_id.clone()));
    let metadata_map = metadata.get_many(&needed).await?;

    // 7. Apply the filter exactly like `apply_queue_filter_*` in the
    //    handler — but split here per `diversity_mode` so the bench
    //    measures whichever path it was asked about. Path layout
    //    mirrors `walk_hard_cap` / `walk_mmr` / `walk_off`; keeping
    //    them in sync is a deliberate manual concern (the harness
    //    can't reuse the gateway's private helpers without exporting
    //    them, and exposing those for a bench doesn't pay off).
    let filter = QueueFilter::build(&queue_track_ids, now_playing.as_ref(), &metadata_map, cfg);
    let (admitted_sims, dropped_sims, dropped_artist, dropped_dedup) = match cfg.diversity_mode {
        DiversityMode::HardCap => walk_hard_cap_bench(&candidates, &metadata_map, filter, top_n),
        DiversityMode::Mmr => walk_mmr_bench(&candidates, &metadata_map, filter, ann, cfg, top_n),
        DiversityMode::Off => walk_off_bench(&candidates, &filter, top_n),
    };

    let elapsed_us = u64::try_from(t0.elapsed().as_micros()).unwrap_or(u64::MAX);
    // --- timer ends ---

    Ok(IterOutcome::Recorded(IterRecord {
        admitted_sims,
        dropped_sims,
        dropped_artist,
        dropped_dedup,
        elapsed_us,
    }))
}

/// Hard-cap walk — same shape as `walk_hard_cap` in the handler.
/// Returns `(admitted_sims, dropped_sims, dropped_artist, dropped_dedup)`
/// as a tuple so the caller can plug the values straight into
/// `IterRecord`.
fn walk_hard_cap_bench(
    candidates: &[music_recommend::ann::AnnQueryResult],
    metadata_map: &std::collections::HashMap<TrackId, TrackMetadata>,
    mut filter: QueueFilter,
    top_n: usize,
) -> (Vec<f32>, Vec<f32>, u32, u32) {
    let mut admitted_sims = Vec::with_capacity(top_n);
    let mut dropped_sims = Vec::new();
    let mut dropped_artist = 0u32;
    let mut dropped_dedup = 0u32;
    for c in candidates {
        if admitted_sims.len() >= top_n {
            break;
        }
        if filter.is_excluded(&c.track_id) {
            continue;
        }
        match filter.try_accept(metadata_map.get(&c.track_id)) {
            FilterDecision::Accept => admitted_sims.push(c.similarity),
            FilterDecision::RejectArtistCap => {
                dropped_artist += 1;
                dropped_sims.push(c.similarity);
            }
            FilterDecision::RejectDedup => {
                dropped_dedup += 1;
                dropped_sims.push(c.similarity);
            }
        }
    }
    (admitted_sims, dropped_sims, dropped_artist, dropped_dedup)
}

/// MMR walk — mirrors `walk_mmr` in the handler. Vectors come from the
/// ANN, missing ones leave the diversity term at 0.
fn walk_mmr_bench(
    candidates: &[music_recommend::ann::AnnQueryResult],
    metadata_map: &std::collections::HashMap<TrackId, TrackMetadata>,
    mut filter: QueueFilter,
    ann: &AnnIndex,
    cfg: QueueFilterConfig,
    top_n: usize,
) -> (Vec<f32>, Vec<f32>, u32, u32) {
    // Drop excluded up front; no point hydrating their vectors.
    let surviving: Vec<&music_recommend::ann::AnnQueryResult> = candidates
        .iter()
        .filter(|c| !filter.is_excluded(&c.track_id))
        .collect();

    let mmr_inputs: Vec<MmrCandidate> = surviving
        .iter()
        .map(|c| MmrCandidate {
            track_id: c.track_id.clone(),
            sim_to_seed: c.similarity,
            vector: ann.get_vector(&c.track_id).ok().flatten(),
            artist_key: metadata_map
                .get(&c.track_id)
                .map(QueueFilter::artist_key_for),
            relevance_bonus: 0.0,
        })
        .collect();

    let want = top_n.saturating_mul(2);
    let order = mmr_rerank(
        &mmr_inputs,
        cfg.mmr_lambda,
        want,
        cfg.artist_penalty_weight,
        filter.artist_counts(),
    );

    let mut admitted_sims = Vec::with_capacity(top_n);
    let mut dropped_sims = Vec::new();
    let mut dropped_artist = 0u32;
    let mut dropped_dedup = 0u32;
    for idx in order {
        if admitted_sims.len() >= top_n {
            break;
        }
        let c = surviving[idx];
        match filter.try_accept(metadata_map.get(&c.track_id)) {
            FilterDecision::Accept => admitted_sims.push(c.similarity),
            FilterDecision::RejectArtistCap => {
                dropped_artist += 1;
                dropped_sims.push(c.similarity);
            }
            FilterDecision::RejectDedup => {
                dropped_dedup += 1;
                dropped_sims.push(c.similarity);
            }
        }
    }
    (admitted_sims, dropped_sims, dropped_artist, dropped_dedup)
}

/// Off walk — admit candidates in input order until `top_n`. Queue
/// exclusion still runs so the slate doesn't repeat what's already in
/// the queue.
fn walk_off_bench(
    candidates: &[music_recommend::ann::AnnQueryResult],
    filter: &QueueFilter,
    top_n: usize,
) -> (Vec<f32>, Vec<f32>, u32, u32) {
    let mut admitted_sims = Vec::with_capacity(top_n);
    for c in candidates {
        if admitted_sims.len() >= top_n {
            break;
        }
        if filter.is_excluded(&c.track_id) {
            continue;
        }
        admitted_sims.push(c.similarity);
    }
    (admitted_sims, Vec::new(), 0, 0)
}

/// Pre-fill the queue with the seed track plus up to
/// `max_per_artist - 1` other tracks by the same artist. Goal: push
/// the seed's artist to the cap so the next refill (which will surface
/// same-artist neighbors) gets blocked, exposing the cap's tax.
///
/// If the artist has fewer tracks in the catalog than the cap, returns
/// what's available. The bench iteration still records — undersaturated
/// runs are useful data on their own (they show how often the saturation
/// strategy can't be applied at all).
async fn build_saturated_queue(
    metadata: &MetadataStore,
    seed_id: &TrackId,
    seed_meta: &TrackMetadata,
    cfg: QueueFilterConfig,
) -> Result<Vec<TrackId>> {
    if cfg.max_per_artist == 0 {
        // Cap disabled — saturation makes no sense; just return the seed.
        return Ok(vec![seed_id.clone()]);
    }
    let want = cfg.max_per_artist as usize;
    // Cap the SQL `LIMIT` so we never fetch more than we'd use. We ask
    // for `want` (not `want - 1`) because the response may include the
    // seed itself, which we filter out below.
    let same_artist = same_artist_track_ids(metadata.pool(), seed_meta, want).await?;

    let mut queue: Vec<TrackId> = Vec::with_capacity(want);
    queue.push(seed_id.clone());
    for id in same_artist {
        if queue.len() >= want {
            break;
        }
        if &id == seed_id {
            continue;
        }
        queue.push(id);
    }
    Ok(queue)
}

/// Look up other tracks by the same artist as `seed_meta`. Prefers
/// `artist_id` (the stable handle) and falls back to a case-insensitive
/// match on the artist name when the metadata row didn't carry an id —
/// mirrors the dual-key strategy [`music_recommend::queue_filter::artist_key`]
/// uses internally.
async fn same_artist_track_ids(
    pool: &sqlx::SqlitePool,
    seed_meta: &TrackMetadata,
    limit: usize,
) -> Result<Vec<TrackId>> {
    // Bound the LIMIT placeholder; sqlx would coerce a usize via i64
    // anyway, but doing it ourselves keeps the cast predictable.
    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
    let rows = if let Some(aid) = seed_meta.artist_id.as_deref()
        && !aid.is_empty()
    {
        sqlx::query("SELECT track_id FROM track_metadata WHERE artist_id = ?1 LIMIT ?2")
            .bind(aid)
            .bind(limit_i64)
            .fetch_all(pool)
            .await?
    } else {
        // Name fallback. LOWER-on-both-sides; index on artist isn't
        // present so this is a table scan, but the bench runs offline
        // and the `track_metadata` table at single-user scale is small.
        let lname = seed_meta.artist.to_lowercase();
        sqlx::query(
            "SELECT track_id FROM track_metadata \
             WHERE LOWER(artist) = ?1 LIMIT ?2",
        )
        .bind(lname)
        .bind(limit_i64)
        .fetch_all(pool)
        .await?
    };
    Ok(rows
        .into_iter()
        .map(|r| TrackId::from(r.get::<String, _>(0)))
        .collect())
}

// --- seed sampling ----------------------------------------------------

/// Pick `n` random track ids from the embedded set. The query joins
/// against `track_metadata` so we don't sample tracks the bench would
/// then have to skip for missing metadata.
async fn sample_seed_pool(
    store: &EmbeddingStore,
    model_version: &ModelVersion,
    n: usize,
    rng_seed: u64,
) -> Result<Vec<TrackId>> {
    let rows = sqlx::query(
        "SELECT e.track_id \
           FROM track_embeddings e \
           JOIN track_metadata  m ON m.track_id = e.track_id \
          WHERE e.status = 'done' AND e.model_version = ?1",
    )
    .bind(model_version.as_str())
    .fetch_all(store.pool())
    .await?;

    let mut all: Vec<TrackId> = rows
        .into_iter()
        .map(|r| TrackId::from(r.get::<String, _>(0)))
        .collect();

    // Deterministic shuffle via SmallRng → take(n). Sampling-without-
    // replacement so a 200-iteration run on a 5k-track library doesn't
    // benchmark the same seed twice.
    let mut rng = SmallRng::seed_from_u64(rng_seed);
    all.shuffle(&mut rng);
    all.truncate(n);
    Ok(all)
}

/// "Active" model version = the one with the most `done` rows. When
/// the user hasn't passed `--model-version`, this is what we query
/// against, mirroring how the live gateway picks the active version
/// from the embedder's last health probe.
async fn active_model_version(store: &EmbeddingStore) -> Result<ModelVersion> {
    let row = sqlx::query(
        "SELECT model_version FROM track_embeddings \
          WHERE status = 'done' \
          GROUP BY model_version \
          ORDER BY COUNT(*) DESC LIMIT 1",
    )
    .fetch_optional(store.pool())
    .await?;
    let v: String = row
        .ok_or_else(|| anyhow!("no embedded tracks found in track_embeddings"))?
        .get(0);
    Ok(ModelVersion::from(v.as_str()))
}

// --- aggregation + reporting -----------------------------------------

#[derive(Debug, Serialize)]
struct Report {
    config: ReportConfig,
    population: ReportPopulation,
    totals: ReportTotals,
    admit: SimSummary,
    /// Renamed at the JSON layer to `drop`; the trailing-underscore
    /// Rust binding is just to avoid shadowing the Drop trait method
    /// name in nearby imports.
    #[serde(rename = "drop")]
    drop_: SimSummary,
    /// Per-call true tax = mean(drop_sims) - mean(admit_sims). Only
    /// computed for calls where both sets are non-empty.
    tax_per_call: TaxSummary,
    latency_us: LatencySummary,
}

#[derive(Debug, Serialize)]
struct ReportConfig {
    iterations_requested: usize,
    rng_seed: u64,
    queue_mode: &'static str,
    top_n: usize,
    max_per_artist: u32,
    dedup_titles: bool,
    diversity_mode: DiversityModeArg,
    /// Recorded regardless of `diversity_mode` so saved JSON reports
    /// remain self-describing — `λ=0.7` for a hard-cap run is harmless
    /// and removes the "did the bench use the right λ?" ambiguity.
    lambda: f32,
    /// Same self-describing motivation as `lambda`: report the
    /// configured artist penalty for every run, even hard-cap ones
    /// where it goes unused.
    artist_penalty_weight: f32,
}

#[derive(Debug, Serialize)]
struct ReportPopulation {
    model_version: String,
    ann_size: usize,
    seed_pool_size: usize,
    skipped_no_metadata: usize,
    skipped_no_vector: usize,
}

#[derive(Debug, Serialize)]
struct ReportTotals {
    iterations_recorded: usize,
    total_wall_ms: f64,
    calls_with_drops: usize,
    calls_starved: usize,
    admit_count_total: usize,
    drop_count_total: usize,
    drop_count_artist: u32,
    drop_count_dedup: u32,
}

#[derive(Debug, Serialize, Default)]
struct SimSummary {
    n: usize,
    mean: Option<f64>,
    /// 1.0 - mean (cosine distance equivalent) — the user's original
    /// "average jump" framing. Only present for the admit summary;
    /// drop side computes it but isn't really "from seed" so we skip.
    mean_distance: Option<f64>,
    p50: Option<f64>,
    p95: Option<f64>,
    p99: Option<f64>,
    min: Option<f64>,
    max: Option<f64>,
}

#[derive(Debug, Serialize, Default)]
struct TaxSummary {
    n_calls: usize,
    mean: Option<f64>,
    p50: Option<f64>,
    p95: Option<f64>,
    max: Option<f64>,
}

#[derive(Debug, Serialize, Default)]
struct LatencySummary {
    n: usize,
    mean: Option<f64>,
    p50: Option<f64>,
    p95: Option<f64>,
    p99: Option<f64>,
    max: Option<f64>,
}

#[allow(clippy::too_many_arguments)]
fn build_report(
    args: &Args,
    model_version: &ModelVersion,
    ann_size: usize,
    seed_pool_size: usize,
    skipped_no_metadata: usize,
    skipped_no_vector: usize,
    total_wall_ms: f64,
    rows: &[IterRecord],
) -> Report {
    let queue_mode = match args.queue_mode {
        QueueMode::Cold => "cold",
        QueueMode::Saturated => "saturated",
    };

    let mut admit_sims: Vec<f32> = Vec::new();
    let mut drop_sims: Vec<f32> = Vec::new();
    // Tax is computed in f64 (means are f64 already); keep it that way.
    let mut tax_per_call: Vec<f64> = Vec::new();
    let mut latencies: Vec<u64> = Vec::with_capacity(rows.len());
    let mut calls_with_drops = 0usize;
    let mut calls_starved = 0usize;
    let mut drop_count_artist = 0u32;
    let mut drop_count_dedup = 0u32;
    for r in rows {
        admit_sims.extend_from_slice(&r.admitted_sims);
        drop_sims.extend_from_slice(&r.dropped_sims);
        latencies.push(r.elapsed_us);
        drop_count_artist += r.dropped_artist;
        drop_count_dedup += r.dropped_dedup;
        let total_drops = r.dropped_artist + r.dropped_dedup;
        if total_drops > 0 {
            calls_with_drops += 1;
        }
        if r.admitted_sims.is_empty() {
            calls_starved += 1;
        }
        if !r.admitted_sims.is_empty() && !r.dropped_sims.is_empty() {
            let mean_admit = mean_f32(&r.admitted_sims);
            let mean_drop = mean_f32(&r.dropped_sims);
            if let (Some(ma), Some(md)) = (mean_admit, mean_drop) {
                tax_per_call.push(md - ma);
            }
        }
    }

    let admit = sim_summary(&admit_sims, /* with_distance = */ true);
    let drop_ = sim_summary(&drop_sims, /* with_distance = */ false);
    let tax_per_call_summary = tax_summary(&tax_per_call);
    let latency_us = latency_summary(&latencies);

    Report {
        config: ReportConfig {
            iterations_requested: args.iterations,
            rng_seed: args.rng_seed,
            queue_mode,
            top_n: args.top_n,
            max_per_artist: args.max_per_artist,
            dedup_titles: args.dedup_titles,
            diversity_mode: args.diversity_mode,
            lambda: args.lambda,
            artist_penalty_weight: args.artist_penalty_weight,
        },
        population: ReportPopulation {
            model_version: model_version.to_string(),
            ann_size,
            seed_pool_size,
            skipped_no_metadata,
            skipped_no_vector,
        },
        totals: ReportTotals {
            iterations_recorded: rows.len(),
            total_wall_ms,
            calls_with_drops,
            calls_starved,
            admit_count_total: admit_sims.len(),
            drop_count_total: drop_sims.len(),
            drop_count_artist,
            drop_count_dedup,
        },
        admit,
        drop_,
        tax_per_call: tax_per_call_summary,
        latency_us,
    }
}

fn mean_f32(xs: &[f32]) -> Option<f64> {
    if xs.is_empty() {
        None
    } else {
        let sum: f64 = xs.iter().map(|v| f64::from(*v)).sum();
        // Cast is fine: xs.len() < usize::MAX << 2^53 in any realistic case.
        #[allow(clippy::cast_precision_loss)]
        Some(sum / xs.len() as f64)
    }
}

fn percentile_f32(sorted: &[f32], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    // Nearest-rank: `idx = ceil(p * n) - 1`, clamped.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let idx = (((p * sorted.len() as f64).ceil() as usize).saturating_sub(1)).min(sorted.len() - 1);
    Some(f64::from(sorted[idx]))
}

fn percentile_u64(sorted: &[u64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let idx = (((p * sorted.len() as f64).ceil() as usize).saturating_sub(1)).min(sorted.len() - 1);
    #[allow(clippy::cast_precision_loss)]
    Some(sorted[idx] as f64)
}

fn sim_summary(values: &[f32], with_distance: bool) -> SimSummary {
    if values.is_empty() {
        return SimSummary::default();
    }
    let mut sorted: Vec<f32> = values.to_vec();
    // total_cmp: f32 has no Ord; total_cmp gives a stable total order
    // and never panics on NaN (which embeddings shouldn't produce, but
    // bench data shouldn't blow up if some pathological row sneaks in).
    sorted.sort_by(f32::total_cmp);
    let mean = mean_f32(values);
    SimSummary {
        n: values.len(),
        mean,
        mean_distance: if with_distance {
            mean.map(|m| 1.0 - m)
        } else {
            None
        },
        p50: percentile_f32(&sorted, 0.50),
        p95: percentile_f32(&sorted, 0.95),
        p99: percentile_f32(&sorted, 0.99),
        min: sorted.first().copied().map(f64::from),
        max: sorted.last().copied().map(f64::from),
    }
}

fn tax_summary(values: &[f64]) -> TaxSummary {
    if values.is_empty() {
        return TaxSummary::default();
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    TaxSummary {
        n_calls: values.len(),
        mean: mean_f64(values),
        p50: percentile_f64(&sorted, 0.50),
        p95: percentile_f64(&sorted, 0.95),
        max: sorted.last().copied(),
    }
}

fn mean_f64(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        None
    } else {
        #[allow(clippy::cast_precision_loss)]
        Some(xs.iter().sum::<f64>() / xs.len() as f64)
    }
}

fn percentile_f64(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let idx = (((p * sorted.len() as f64).ceil() as usize).saturating_sub(1)).min(sorted.len() - 1);
    Some(sorted[idx])
}

fn latency_summary(values: &[u64]) -> LatencySummary {
    if values.is_empty() {
        return LatencySummary::default();
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let n = values.len();
    #[allow(clippy::cast_precision_loss)]
    let mean = (values.iter().copied().sum::<u64>() as f64) / n as f64;
    LatencySummary {
        n,
        mean: Some(mean),
        p50: percentile_u64(&sorted, 0.50),
        p95: percentile_u64(&sorted, 0.95),
        p99: percentile_u64(&sorted, 0.99),
        #[allow(clippy::cast_precision_loss)]
        max: sorted.last().copied().map(|v| v as f64),
    }
}

fn print_table(r: &Report) {
    let opt = |x: Option<f64>| x.map_or_else(|| "  -   ".to_string(), |v| format!("{v:>7.4}"));
    let opt_us =
        |x: Option<f64>| x.map_or_else(|| "    -    ".to_string(), |v| format!("{v:>9.1}"));

    println!();
    println!("=== recommend-bench ===");
    let mode_label = match r.config.diversity_mode {
        DiversityModeArg::HardCap => "hard_cap",
        DiversityModeArg::Mmr => "mmr",
        DiversityModeArg::Off => "off",
    };
    println!(
        "config:    iters={} (recorded {}) seed={} mode={} top_n={} cap={} dedup={} diversity={} λ={:.2}",
        r.config.iterations_requested,
        r.totals.iterations_recorded,
        r.config.rng_seed,
        r.config.queue_mode,
        r.config.top_n,
        r.config.max_per_artist,
        r.config.dedup_titles,
        mode_label,
        r.config.lambda,
    );
    println!(
        "population: ann={} seed_pool={} skipped(no_meta={}, no_vec={})",
        r.population.ann_size,
        r.population.seed_pool_size,
        r.population.skipped_no_metadata,
        r.population.skipped_no_vector,
    );
    println!(
        "totals:    wall={:.1} ms  calls_with_drops={}  calls_starved={}  admits={}  drops={} (artist={} dedup={})",
        r.totals.total_wall_ms,
        r.totals.calls_with_drops,
        r.totals.calls_starved,
        r.totals.admit_count_total,
        r.totals.drop_count_total,
        r.totals.drop_count_artist,
        r.totals.drop_count_dedup,
    );
    println!();
    println!("              n      mean      p50      p95      p99      min      max");
    println!(
        "admit_sim   {:>4} {} {} {} {} {} {}",
        r.admit.n,
        opt(r.admit.mean),
        opt(r.admit.p50),
        opt(r.admit.p95),
        opt(r.admit.p99),
        opt(r.admit.min),
        opt(r.admit.max),
    );
    println!(
        "drop_sim    {:>4} {} {} {} {} {} {}",
        r.drop_.n,
        opt(r.drop_.mean),
        opt(r.drop_.p50),
        opt(r.drop_.p95),
        opt(r.drop_.p99),
        opt(r.drop_.min),
        opt(r.drop_.max),
    );
    println!("admit_dist (1-sim)        {}", opt(r.admit.mean_distance));
    println!();
    println!(
        "tax/call    {:>4} {} {} {}     -    {}",
        r.tax_per_call.n_calls,
        opt(r.tax_per_call.mean),
        opt(r.tax_per_call.p50),
        opt(r.tax_per_call.p95),
        opt(r.tax_per_call.max),
    );
    println!();
    println!(
        "latency_us  {:>4} {} {} {} {}     -    {}",
        r.latency_us.n,
        opt_us(r.latency_us.mean),
        opt_us(r.latency_us.p50),
        opt_us(r.latency_us.p95),
        opt_us(r.latency_us.p99),
        opt_us(r.latency_us.max),
    );
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_f32_handles_single_value() {
        let v = [0.5_f32];
        assert_eq!(percentile_f32(&v, 0.50), Some(0.5));
        assert_eq!(percentile_f32(&v, 0.99), Some(0.5));
    }

    #[test]
    fn percentile_f32_picks_nearest_rank() {
        // Sorted: [0.1, 0.2, 0.3, 0.4, 0.5]. percentile_f32 returns f64
        // via f64::from on the picked f32, so equality is exact for the
        // small fractions used here.
        let v = [0.1_f32, 0.2, 0.3, 0.4, 0.5];
        // p50 → ceil(0.5*5)-1 = 2 → 0.3.
        let got_p50 = percentile_f32(&v, 0.50).expect("set");
        assert!((got_p50 - 0.3_f64).abs() < 1e-6, "got {got_p50}");
        // p95 → ceil(0.95*5)-1 = 4 → 0.5.
        let got_p95 = percentile_f32(&v, 0.95).expect("set");
        assert!((got_p95 - 0.5_f64).abs() < 1e-6, "got {got_p95}");
        // p99 → ceil(0.99*5)-1 = 4 → 0.5 (clamped at last index).
        let got_p99 = percentile_f32(&v, 0.99).expect("set");
        assert!((got_p99 - 0.5_f64).abs() < 1e-6, "got {got_p99}");
    }

    #[test]
    fn percentile_f32_empty_returns_none() {
        assert_eq!(percentile_f32(&[], 0.50), None);
    }

    #[test]
    fn mean_f32_zero_length_is_none() {
        assert_eq!(mean_f32(&[]), None);
    }

    #[test]
    fn mean_f32_average_is_correct() {
        let v = [0.2_f32, 0.4, 0.6];
        let m = mean_f32(&v).expect("non-empty");
        assert!((m - 0.4).abs() < 1e-6, "got {m}");
    }

    #[test]
    fn sim_summary_with_distance_sets_mean_distance() {
        let s = sim_summary(&[0.6_f32, 0.8, 1.0], true);
        let md = s.mean_distance.expect("set");
        // mean = 0.8, distance = 0.2.
        assert!((md - 0.2).abs() < 1e-6, "got {md}");
    }

    #[test]
    fn sim_summary_without_distance_omits_mean_distance() {
        let s = sim_summary(&[0.6_f32, 0.8, 1.0], false);
        assert!(s.mean_distance.is_none());
    }

    #[test]
    fn latency_summary_handles_skewed_distribution() {
        // p99 should track the tail, not the bulk.
        let mut v: Vec<u64> = (0..99).collect();
        v.push(10_000);
        let s = latency_summary(&v);
        assert_eq!(s.n, 100);
        // p99 on 100 elements = ceil(99)-1 = 98 → second-to-last = 98.
        assert_eq!(s.p99, Some(98.0));
        assert_eq!(s.max, Some(10_000.0));
    }
}
