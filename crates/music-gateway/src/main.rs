//! Gateway binary. Loads config, wires the router, terminates TLS via
//! axum-server + rustls.
//!
//! TLS is wired here, not in `app.rs`, so unit/integration tests can exercise
//! the router over plain HTTP via `tower::ServiceExt::oneshot`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use music_cache::Cache;
use music_gateway::diagnostics::{TraceLayer, TraceStore, spawn_drainer};
use music_gateway::embedder::{EmbedderHandle, boot_probe};
use music_gateway::ingest::{
    SubsonicAudioFetcher, SubsonicMetadataFetcher, spawn_ingest_worker, spawn_metadata_backfill,
};
use music_gateway::oauth::{NewClient, OauthStore, SetupToken};
use music_gateway::{AppState, Config, build_router};
use music_recommend::ann::AnnIndex;
use music_recommend::ingest::{AudioFetcher, MetadataFetcher, MetadataIngest, rebuild_ann_from_store};
use music_recommend::metadata::MetadataStore;
use music_recommend::store::EmbeddingStore;
use music_recommend::types::ModelVersion;
use std::time::Duration;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Default embedding dimension. CLAP's audio + text encoders share a
/// 512-dim space; we hardcode this here because the ANN index has to
/// commit to a dim at construction time. If a future model uses a
/// different dim, this becomes a config option.
const DEFAULT_EMBEDDING_DIM: usize = 512;
/// HNSW connectivity. usearch's recommended default for cosine-style
/// similarity at our scale (~10⁴ vectors).
const ANN_CONNECTIVITY: usize = 16;

/// Diagnostics-trace channel buffer. Spans land here from the
/// `TraceLayer` and are drained on a tick. Sized for ~1 s of bursty
/// emission across 8 ingest workers + a few HTTP handlers.
const TRACES_CHANNEL_BUFFER: usize = 1024;
/// How often the diagnostics drainer flushes buffered spans into
/// SQLite. Short enough that the diagnostics page feels live; long
/// enough to amortize the SQLite write cost across a batch.
const TRACES_FLUSH_INTERVAL: Duration = Duration::from_millis(500);
/// Ring-buffer cap on the spans table. ~24-48 hours of ingest +
/// request data at the rates we see in P6.
const TRACES_MAX_ROWS: usize = 100_000;

#[derive(Debug, Parser)]
#[command(name = "music-gateway", about = "Music gateway for Navidrome")]
struct Args {
    /// Path to the gateway config TOML.
    #[arg(short, long, env = "MUSIC_GATEWAY_CONFIG")]
    config: PathBuf,
}

#[tokio::main]
#[allow(clippy::too_many_lines)] // boot path; decomposing further would obscure the order of operations
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install rustls ring crypto provider");

    let args = Args::parse();
    let config = Config::load(&args.config)
        .with_context(|| format!("loading config from {}", args.config.display()))?;

    // Diagnostics traces DB lives next to the OAuth state DB but in
    // its own SQLite file — different lifecycle (ring-buffered,
    // throwaway) from OAuth state (irreplaceable). See
    // crates/music-gateway/src/diagnostics/mod.rs for the design
    // rationale.
    let traces_db_path = config.oauth.state_db.with_extension("traces.sqlite");
    let trace_store = TraceStore::open(&traces_db_path)
        .await
        .with_context(|| format!("opening traces DB at {}", traces_db_path.display()))?;
    let (trace_layer, trace_rx) = TraceLayer::new(TRACES_CHANNEL_BUFFER);
    let _trace_drainer = spawn_drainer(
        trace_store.clone(),
        trace_rx,
        TRACES_FLUSH_INTERVAL,
        TRACES_MAX_ROWS,
    );

    // Layered subscriber: env-filtered fmt to stdout (existing behavior)
    // + diagnostics layer that sinks spans into the traces DB.
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .with(trace_layer)
        .init();

    let listen = config.server.listen;
    let tls = RustlsConfig::from_pem_file(&config.server.tls_cert, &config.server.tls_key)
        .await
        .context("loading TLS cert + key (mkcert PEM expected)")?;

    let cache = Cache::open(&config.cache.path)
        .await
        .with_context(|| format!("opening cache at {}", config.cache.path.display()))?;

    let oauth = OauthStore::open(&config.oauth.state_db)
        .await
        .with_context(|| {
            format!(
                "opening oauth state DB at {}",
                config.oauth.state_db.display()
            )
        })?;

    // Register pre-declared OAuth clients (idempotent: skip when the
    // client_id is already in the DB).
    for client in &config.oauth.clients {
        if oauth.find_client(&client.client_id).await?.is_none() {
            oauth
                .register_client(NewClient {
                    client_id: client.client_id.clone(),
                    name: client.name.clone(),
                    redirect_uris: client.redirect_uris.clone(),
                })
                .await
                .with_context(|| {
                    format!("registering pre-declared oauth client {}", client.client_id)
                })?;
            tracing::info!(client = %client.client_id, "registered oauth client");
        }
    }

    let setup_token = if oauth.master_password_hash().await?.is_none() {
        let token = SetupToken::generate();
        if let Some(value) = token.value() {
            tracing::warn!(
                "gateway is unconfigured — visit https://{}/oauth/setup with token: {}",
                listen,
                value
            );
        }
        token
    } else {
        SetupToken::none()
    };

    let embedder = boot_probe(config.embedder.as_ref()).await;
    let recommend = boot_recommender(&config.oauth.state_db, &embedder).await?;

    let fetcher: Arc<dyn AudioFetcher> = Arc::new(
        SubsonicAudioFetcher::new(&config.upstream)
            .context("building Subsonic ingest fetcher")?,
    );
    let metadata_fetcher: Arc<dyn MetadataFetcher> = Arc::new(
        SubsonicMetadataFetcher::new(&config.upstream)
            .context("building Subsonic metadata fetcher")?,
    );
    let metadata_ingest = MetadataIngest {
        store: recommend.metadata_store.clone(),
        fetcher: Arc::clone(&metadata_fetcher),
    };
    let _ingest_handles = spawn_ingest_worker(
        recommend.embedding_store.clone(),
        recommend.ann.clone(),
        embedder.client().cloned(),
        fetcher,
        Some(metadata_ingest),
        &recommend.model_version,
    );

    let _backfill_handle = spawn_metadata_backfill(
        recommend.metadata_store.clone(),
        metadata_fetcher,
        recommend.model_version.clone(),
    );

    let state = AppState::new(
        config,
        cache,
        oauth,
        setup_token,
        embedder,
        recommend.embedding_store,
        recommend.metadata_store,
        recommend.ann,
        recommend.model_version,
        trace_store.clone(),
    );
    let router = build_router(state);

    tracing::info!(%listen, "music-gateway listening");
    axum_server::bind_rustls(listen, tls)
        .serve(router.into_make_service())
        .await
        .context("axum-server")?;

    Ok(())
}

struct RecommenderState {
    embedding_store: EmbeddingStore,
    metadata_store: MetadataStore,
    ann: Arc<AnnIndex>,
    model_version: ModelVersion,
}

/// Boot the recommender: open the embedding DB, recover crashed
/// in-progress rows, open the ANN, and rebuild the ANN from SQLite
/// when it's empty (cold start or wiped sidecar). Extracted from
/// `main` so the entrypoint stays readable.
async fn boot_recommender(state_db: &Path, embedder: &EmbedderHandle) -> Result<RecommenderState> {
    let recommend_db_path = state_db.with_extension("recommend.sqlite");
    let embedding_store = EmbeddingStore::open(&recommend_db_path)
        .await
        .with_context(|| format!("opening recommend DB at {}", recommend_db_path.display()))?;

    // Any rows still in `in_progress` belong to the previous gateway
    // run; reset them so the worker re-attempts.
    let reset = embedding_store.reset_in_progress().await?;
    if reset > 0 {
        tracing::info!(rows = reset, "recommend: reset stuck in_progress rows");
    }

    let ann_path = state_db.with_extension("ann");
    let ann = AnnIndex::open(&ann_path, DEFAULT_EMBEDDING_DIM, ANN_CONNECTIVITY)
        .context("opening ANN index")?;
    let model_version = embedder
        .last_health()
        .map_or_else(|| ModelVersion::from("default"), |h| h.model_version);

    // Safety net for the historical persist-on-write gap: if SQLite
    // has more `done` rows for this model than the ANN does, our
    // on-disk index is stale (likely because the gateway crashed
    // between an upsert and the next periodic persist). Rebuild from
    // SQLite — it's the source of truth — and persist immediately.
    // The "ann empty" case is the cold-start subset of this; treat
    // both with one branch.
    let ann_len = ann.len()?;
    let sqlite_done = embedding_store.counts(&model_version).await?.done;
    let ann_len_u64 = u64::try_from(ann_len).unwrap_or(u64::MAX);
    if ann_len_u64 < sqlite_done {
        tracing::info!(
            model = %model_version,
            ann_len,
            sqlite_done,
            "recommend: ANN behind SQLite, rebuilding"
        );
        rebuild_ann_from_store(&embedding_store, &ann, &model_version)
            .await
            .context("rebuilding ANN from store")?;
        if ann.len()? > 0 {
            ann.persist().context("persisting rebuilt ANN")?;
        }
    }

    // Metadata store shares the same SQLite pool — they're sibling
    // tables under the same migration set (0001 embeddings, 0002 events,
    // 0003 track_metadata). One file, one pool, two stores.
    let metadata_store = MetadataStore::new(embedding_store.pool().clone());
    let metadata_count = metadata_store.count().await?;
    tracing::info!(rows = metadata_count, "recommend: metadata cache loaded");

    Ok(RecommenderState {
        embedding_store,
        metadata_store,
        ann: Arc::new(ann),
        model_version,
    })
}
