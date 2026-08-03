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
use music_gateway::auto_projection::spawn_auto_projection_task;
use music_gateway::diagnostics::{TraceLayer, TraceStore, spawn_drainer};
use music_gateway::embedder::{EmbedderHandle, boot_probe};
use music_gateway::ingest::{
    SubsonicAudioFetcher, SubsonicMetadataFetcher, spawn_ingest_worker, spawn_metadata_backfill,
};
use music_gateway::oauth::{NewClient, OauthStore, SetupToken};
use music_gateway::whitening_text;
use music_gateway::{AppState, Config, build_router};
use music_recommend::ann::AnnIndex;
use music_recommend::ingest::{
    AudioFetcher, MetadataFetcher, MetadataIngest, rebuild_ann_from_store,
};
use music_recommend::metadata::MetadataStore;
use music_recommend::projection::ProjectionStore;
use music_recommend::store::EmbeddingStore;
use music_recommend::types::ModelVersion;
use music_recommend::whitening::{Whitening, default_k};
use music_recommend::whitening_store::WhiteningStore;
use std::time::Duration;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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
    /// Optional maintenance subcommand. With none, the gateway serves.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Reset the owner (id=1) master password, then exit. Host-only
    /// account recovery (plan D8): there is no email/recovery-code path,
    /// so the gateway host — where this runs — is the root of trust.
    /// Rewrites the Argon2 hash in place; the owner row and all its data
    /// are preserved.
    ResetMasterPassword {
        /// New password. Omit to read it from stdin (pipe it so it stays
        /// out of shell history): `echo -n 'new-pw' | music-gateway
        /// --config … reset-master-password`.
        #[arg(long)]
        password: Option<String>,
    },
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

    // Maintenance subcommands run against the config + state DB, then exit
    // — they never bind the listener or boot the recommender.
    if let Some(command) = args.command {
        return run_subcommand(command, &config).await;
    }

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
    // Background failover: re-probe the endpoint list and switch the active
    // sidecar without a restart. No-op when the interval is 0 or no
    // `[embedder]` block is configured.
    if let Some(cfg) = config.embedder.as_ref() {
        music_gateway::embedder::spawn_probe_loop(
            embedder.clone(),
            Duration::from_secs(cfg.probe_interval_seconds),
        );
    }
    let recommend = boot_recommender(
        &config.oauth.state_db,
        config.recommend.embedding_dim,
        config.recommend.whitening_enabled,
        &embedder,
    )
    .await?;

    let fetcher: Arc<dyn AudioFetcher> = Arc::new(
        SubsonicAudioFetcher::new(&config.upstream).context("building Subsonic ingest fetcher")?,
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

    // Auto-recompute the latent-space projection as new tracks land.
    // Counter + quiet-period trigger; per-run versioning with retention
    // pruning. No-op when the embedder is unreachable at boot — the
    // task body would have nothing to call.
    let _auto_projection_handle = spawn_auto_projection_task(
        embedder.client().cloned(),
        recommend.embedding_store.clone(),
        ProjectionStore::new(recommend.embedding_store.pool().clone()),
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
    // Background reaper for expired guest accounts (PR D). Detached for the
    // process lifetime; a 0 interval disables it.
    let _guest_sweep = music_gateway::guest_codes::spawn_guest_sweep(
        state.oauth().clone(),
        Duration::from_secs(state.config().oauth.guest_sweep_interval_seconds),
    );

    // Typo-tolerant search: build the fuzzy catalog index in the background
    // and refresh it on an interval. Until it lands, `/v1/search` falls
    // back to proxying Navidrome `search3`.
    if state.config().search.enabled {
        music_gateway::search::spawn_index_builder(
            state.search_handle(),
            state.config().upstream.clone(),
            Duration::from_secs(state.config().search.refresh_interval_seconds),
        );
    }

    // Catalog discovery: keep the embedding queue in step with Navidrome
    // so newly-added music becomes recommendable on its own, rather than
    // waiting for a manual `scripts/enqueue_all_tracks.py` run. Runs even
    // when the embedder is down — queued rows are durable and drain when
    // the sidecar returns.
    let _discovery_handle = {
        let discovery_cfg = state.config().discovery.clone();
        if discovery_cfg.enabled {
            let watcher = music_gateway::discovery::CatalogWatcher::new(
                &state.config().upstream,
                state.embedding_store().clone(),
                state.recommend_model_version().clone(),
                discovery_cfg.recent_albums,
            )
            .context("building catalog discovery watcher")?;
            tracing::info!(
                interval_seconds = discovery_cfg.interval_seconds,
                full_interval_seconds = discovery_cfg.full_interval_seconds,
                recent_albums = discovery_cfg.recent_albums,
                "discovery: catalog watch started"
            );
            music_gateway::discovery::spawn_catalog_watch(watcher, &discovery_cfg)
        } else {
            tracing::info!(
                "discovery: disabled; tracks enter the queue only via \
                 POST /v1/recommend/enqueue or /v1/admin/discovery/scan"
            );
            None
        }
    };

    let router = build_router(state);

    tracing::info!(%listen, "music-gateway listening");
    // `with_connect_info` so handlers can extract the peer `SocketAddr`
    // (the login brute-force limiter keys on it).
    axum_server::bind_rustls(listen, tls)
        .serve(router.into_make_service_with_connect_info::<std::net::SocketAddr>())
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
async fn boot_recommender(
    state_db: &Path,
    embedding_dim: usize,
    whitening_enabled: bool,
    embedder: &EmbedderHandle,
) -> Result<RecommenderState> {
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

    if embedding_dim == 0 {
        anyhow::bail!("recommend.embedding_dim must be > 0");
    }
    let ann_path = state_db.with_extension("ann");
    let ann =
        AnnIndex::open(&ann_path, embedding_dim, ANN_CONNECTIVITY).context("opening ANN index")?;
    let model_version = embedder
        .last_health()
        .map_or_else(|| ModelVersion::from("default"), |h| h.model_version);

    // All-but-the-Top whitening: load the cached transform for this model,
    // or fit one from the existing embeddings (post-hoc — no re-embedding).
    // Installing it on the ANN means stored + queried vectors are de-coned,
    // fixing CLaMP 3's anisotropy. Installing a transform forces a full ANN
    // rebuild below, since any on-disk vectors predate it.
    let mut whitening_installed = false;
    if whitening_enabled {
        if let Some(w) =
            load_or_fit_whitening(&embedding_store, embedding_dim, &model_version, embedder).await?
        {
            ann.set_whitening(Some(Arc::new(w)))
                .context("installing whitening transform")?;
            whitening_installed = true;
        } else {
            tracing::info!(
                model = %model_version,
                "recommend: whitening enabled but no embeddings yet; deferring fit to first refit"
            );
        }
    }

    // Safety net for the historical persist-on-write gap: if SQLite
    // has more `done` rows for this model than the ANN does, our
    // on-disk index is stale (likely because the gateway crashed
    // between an upsert and the next periodic persist). Rebuild from
    // SQLite — it's the source of truth — and persist immediately.
    // The "ann empty" case is the cold-start subset of this; treat
    // both with one branch. A freshly-installed whitening transform also
    // forces a rebuild: the on-disk vectors are raw (or whitened by an
    // older transform) and must be re-whitened to match.
    let ann_len = ann.len()?;
    let sqlite_done = embedding_store.counts(&model_version).await?.done;
    let ann_len_u64 = u64::try_from(ann_len).unwrap_or(u64::MAX);
    if whitening_installed || ann_len_u64 < sqlite_done {
        tracing::info!(
            model = %model_version,
            ann_len,
            sqlite_done,
            whitening = whitening_installed,
            "recommend: rebuilding ANN from SQLite"
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

/// Load the cached ABTT whitening transform for `model_version`, or fit a
/// fresh one from the existing `done` embeddings and persist it. Returns
/// `None` when there are no embeddings yet (cold start) — the caller runs
/// the ANN un-whitened until a refit (or the next boot) can fit one.
async fn load_or_fit_whitening(
    embedding_store: &EmbeddingStore,
    embedding_dim: usize,
    model_version: &ModelVersion,
    embedder: &EmbedderHandle,
) -> Result<Option<Whitening>> {
    let store = WhiteningStore::new(embedding_store.pool().clone());

    // 1. Audio transform: load the cached fit, or fit one from the corpus.
    //    A cached transform fitted at a different dim (e.g. after a
    //    CLAP→CLaMP 3 swap) is non-migratable — discard it and refit rather
    //    than crash later in `set_whitening`'s dim guard. The same applies to
    //    the ANN sidecar; see the model-bump cutover notes in CLAUDE.md.
    let cached = match store
        .get(model_version)
        .await
        .context("loading whitening")?
    {
        Some(w) if w.dim() == embedding_dim => Some(w),
        Some(stale) => {
            tracing::warn!(
                model = %model_version,
                stored = stale.dim(),
                expected = embedding_dim,
                "recommend: cached whitening has stale dim — refitting from corpus"
            );
            None
        }
        None => None,
    };

    let mut whitening = if let Some(w) = cached {
        tracing::info!(model = %model_version, k = w.k(), "recommend: loaded cached whitening");
        w
    } else {
        let corpus = embedding_store.list_done_embeddings(model_version).await?;
        if corpus.is_empty() {
            return Ok(None);
        }
        let vectors: Vec<Vec<f32>> = corpus.into_iter().map(|e| e.vector).collect();
        let k = default_k(embedding_dim);
        let w =
            Whitening::fit(&vectors, k).map_err(|e| anyhow::anyhow!("fitting whitening: {e}"))?;
        store
            .upsert(model_version, &w, vectors.len(), now_unix_ms())
            .await
            .context("persisting fitted whitening")?;
        tracing::info!(
            model = %model_version,
            n = vectors.len(),
            k = w.k(),
            "recommend: fitted + persisted whitening transform"
        );
        w
    };

    // 2. Cross-modal text mean: fit lazily if missing and the embedder is
    //    reachable (it needs to embed a prompt corpus). Without it, station
    //    text queries fall back to the audio mean and collapse — so this is
    //    best-effort, retryable via the refit endpoint, never fatal.
    if !whitening.has_text_mean() {
        if let Some(client) = embedder.client() {
            match whitening_text::fit_text_mean(client, embedding_dim).await {
                Ok(text_mean) => match whitening.clone().with_text_mean(text_mean) {
                    Ok(updated) => {
                        let n = embedding_store
                            .counts(model_version)
                            .await
                            .map_or(0, |c| usize::try_from(c.done).unwrap_or(0));
                        store
                            .upsert(model_version, &updated, n, now_unix_ms())
                            .await
                            .context("persisting text mean")?;
                        tracing::info!(model = %model_version, "recommend: fitted + persisted cross-modal text mean");
                        whitening = updated;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "recommend: text mean dim mismatch; stations stay audio-centered");
                    }
                },
                Err(e) => tracing::warn!(
                    error = %e,
                    "recommend: text-mean fit failed (embedder); stations stay audio-centered until refit"
                ),
            }
        } else {
            tracing::info!(
                model = %model_version,
                "recommend: embedder unavailable; deferring text-mean fit to a refit"
            );
        }
    }

    Ok(Some(whitening))
}

fn now_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Dispatch a maintenance subcommand. These open only the OAuth state DB
/// and exit — no listener, no recommender, no TLS.
async fn run_subcommand(command: Command, config: &Config) -> Result<()> {
    match command {
        Command::ResetMasterPassword { password } => {
            reset_master_password(config, password).await
        }
    }
}

/// Host-only owner password recovery (plan D8). Opens the state DB,
/// validates a new password (read from `--password` or stdin), and
/// rewrites the Argon2 hash on `users.id=1` **in place** — never
/// delete/recreate, which would cascade-drop the owner's data.
async fn reset_master_password(config: &Config, password: Option<String>) -> Result<()> {
    use music_gateway::oauth::handlers::MIN_PASSWORD_LEN;
    use music_gateway::oauth::password;
    use music_gateway::principal::OWNER_USER_ID;

    let oauth = OauthStore::open(&config.oauth.state_db)
        .await
        .with_context(|| {
            format!(
                "opening oauth state DB at {}",
                config.oauth.state_db.display()
            )
        })?;

    // Nothing to reset on an unconfigured gateway — bootstrap via
    // /oauth/setup first. (Reset rewrites an existing hash; it is not a
    // backdoor around the one-shot setup.)
    if oauth.master_password_hash().await?.is_none() {
        anyhow::bail!(
            "gateway is not bootstrapped — complete /oauth/setup first; there is no master password to reset"
        );
    }

    let password = match password {
        Some(p) => p,
        None => read_password_from_stdin()?,
    };
    if password.len() < MIN_PASSWORD_LEN {
        anyhow::bail!("password must be at least {MIN_PASSWORD_LEN} characters");
    }

    let phc = password::hash(&password).map_err(|e| anyhow::anyhow!("hashing password: {e}"))?;
    let updated = oauth.set_user_password(OWNER_USER_ID, &phc).await?;
    anyhow::ensure!(
        updated,
        "owner account (id=1) not found — the gateway state DB may be corrupt"
    );
    println!("master password reset for the owner account (id=1).");
    Ok(())
}

/// Read a password from stdin (for piping). Strips the trailing newline a
/// shell/`echo` appends; otherwise takes the input verbatim.
fn read_password_from_stdin() -> Result<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("reading password from stdin")?;
    let pw = buf.trim_end_matches(['\n', '\r']).to_string();
    if pw.is_empty() {
        anyhow::bail!("no password provided on stdin");
    }
    Ok(pw)
}
