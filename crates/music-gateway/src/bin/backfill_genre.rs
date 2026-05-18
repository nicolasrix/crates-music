//! One-shot backfill for the new `track_metadata.genre` column.
//!
//! After the ingest pipeline started capturing the Subsonic `genre` tag,
//! existing rows that were written before the fix retain `genre IS NULL`.
//! This binary walks them and re-fetches each via the same
//! `SubsonicMetadataFetcher` the live gateway uses — re-using the upsert
//! path means we also pick up any other field the upstream may now
//! populate (e.g. an album that was retitled in Navidrome).
//!
//! Run with the same `--config` path as the gateway binary. The gateway
//! does not need to be stopped, but writes happen against the live
//! `gateway-state.recommend.sqlite` so it's quieter to do this during
//! idle time.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use music_gateway::Config;
use music_gateway::ingest::SubsonicMetadataFetcher;
use music_recommend::ingest::MetadataFetcher;
use music_recommend::metadata::MetadataStore;
use music_recommend::store::EmbeddingStore;
use tracing_subscriber::EnvFilter;

/// Hard cap on the candidate snapshot. We pull the full list of
/// null-genre track ids in one round-trip — at ~50 bytes/id this is
/// trivial memory up to ~20k rows. The cap defends against a
/// pathologically huge cache.
const CANDIDATE_CAP: i64 = 50_000;
/// Polite pause between consecutive Subsonic getSong calls. Navidrome is
/// not heavily rate-limited, but at 5k+ tracks a per-call pause is
/// cheap insurance against accidentally hammering it.
const FETCH_PAUSE: Duration = Duration::from_millis(20);

#[derive(Debug, Parser)]
#[command(
    name = "backfill-genre",
    about = "Backfill null genre rows in track_metadata"
)]
struct Args {
    /// Path to the gateway config TOML — same file the gateway binary uses.
    #[arg(short, long, env = "MUSIC_GATEWAY_CONFIG")]
    config: PathBuf,
    /// Cap on rows to refresh. Useful for a quick sanity run before
    /// committing to the full walk.
    #[arg(long)]
    max: Option<u64>,
    /// Print progress every N rows. Default 50.
    #[arg(long, default_value_t = 50)]
    progress_every: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let config = Config::load(&args.config)
        .with_context(|| format!("loading config from {}", args.config.display()))?;

    let recommend_db_path = config.oauth.state_db.with_extension("recommend.sqlite");
    let embedding_store = EmbeddingStore::open(&recommend_db_path)
        .await
        .with_context(|| format!("opening {}", recommend_db_path.display()))?;
    let store = MetadataStore::new(embedding_store.pool().clone());

    let fetcher = SubsonicMetadataFetcher::new(&config.upstream)
        .context("building SubsonicMetadataFetcher")?;

    // Snapshot the candidate set in one round-trip. Iterating a
    // re-queried set would loop forever on tracks whose upstream genre
    // really is null (the upsert writes null back, so the row stays in
    // the WHERE genre IS NULL bucket on the next pass). The snapshot
    // pattern dodges that entirely — each id is visited at most once.
    let candidates = store
        .null_genre_ids(CANDIDATE_CAP)
        .await
        .context("querying null_genre_ids")?;
    tracing::info!(candidates = candidates.len(), "backfill: snapshot taken");

    let limit = args
        .max
        .and_then(|m| usize::try_from(m).ok())
        .unwrap_or(usize::MAX);
    let work: Vec<_> = candidates.into_iter().take(limit).collect();

    let started = Instant::now();
    let mut total_attempted: u64 = 0;
    let mut total_upserted: u64 = 0;
    let mut total_with_genre: u64 = 0;
    let mut total_failed: u64 = 0;

    for track_id in &work {
        total_attempted += 1;
        match fetcher.fetch_metadata(track_id).await {
            Ok(m) => {
                if m.genre.is_some() {
                    total_with_genre += 1;
                }
                store.upsert(&m).await.context("upserting metadata row")?;
                total_upserted += 1;
            }
            Err(e) => {
                tracing::warn!(track = %track_id, error = %e, "backfill: fetch failed");
                total_failed += 1;
            }
        }
        if total_attempted.is_multiple_of(args.progress_every) {
            tracing::info!(
                attempted = total_attempted,
                upserted = total_upserted,
                with_genre = total_with_genre,
                failed = total_failed,
                "backfill: progress",
            );
        }
        tokio::time::sleep(FETCH_PAUSE).await;
    }

    tracing::info!(
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        attempted = total_attempted,
        upserted = total_upserted,
        with_genre = total_with_genre,
        failed = total_failed,
        "backfill: complete",
    );
    Ok(())
}
