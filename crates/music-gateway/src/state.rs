//! Process-wide shared state cloned into every request handler.
//!
//! `AppState` is intentionally trivially `Clone` (Arc-backed where needed) so
//! axum's extractor system can hand it to handlers cheaply. The reqwest client,
//! L2 cache, and OAuth state DB live here so connection pooling and the SQLite
//! pools are per-process, not per-request.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use music_cache::Cache;
use music_recommend::EventStore;
use music_recommend::FeedbackStore;
use music_recommend::ann::AnnIndex;
use music_recommend::metadata::MetadataStore;
use music_recommend::store::EmbeddingStore;
use music_recommend::types::ModelVersion;

use crate::config::Config;
use crate::diagnostics::TraceStore;
use crate::embedder::EmbedderHandle;
use crate::oauth::{OauthStore, SetupToken};
use crate::proxy::build_http_client;
use crate::sync::SyncStore;

#[derive(Debug, Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    config: Config,
    http: reqwest::Client,
    cache: Cache,
    oauth: OauthStore,
    setup_token: SetupToken,
    sync: SyncStore,
    embedder: EmbedderHandle,
    embedding_store: EmbeddingStore,
    metadata_store: MetadataStore,
    event_store: EventStore,
    feedback: FeedbackStore,
    ann: Arc<AnnIndex>,
    /// The model_version the recommender stamps on enqueue + ANN
    /// queries. Sourced from the embedder's last health probe; falls
    /// back to "default" when the embedder is disabled.
    recommend_model_version: ModelVersion,
    /// Read-only view of the diagnostics trace ring buffer. Handlers
    /// under `/v1/diagnostics/*` query this; the drainer task in
    /// `main.rs` is the sole writer. Cloning is cheap (wraps a sqlx
    /// pool).
    trace_store: TraceStore,
    /// Etags of upstream cover-art bodies that have been observed for
    /// two or more distinct cover-art ids — i.e. Navidrome's default
    /// "no artwork" placeholder. Lookups on cover-art fetches skip the
    /// SQL duplicate-check once an etag is in here. Not persisted —
    /// rebuilt naturally on restart after the first two duplicate
    /// fetches.
    placeholder_etags: RwLock<HashSet<String>>,
    /// Per-cache-key timestamp of the last placeholder revalidation
    /// attempt. When a request hits the cache and finds a placeholder
    /// (our SVG, or a Navidrome default discovered post-hoc), a
    /// background task fetches upstream to see if real art is now
    /// available; this map gates that task so a burst of cache hits
    /// (e.g. 60 covers on a page reload) coalesces into a single
    /// upstream fetch per key per cooldown window.
    placeholder_revalidations: Mutex<HashMap<String, Instant>>,
}

impl AppState {
    /// One call site (boot in `main.rs`) and one in tests; bundling
    /// these into a builder struct adds ceremony without reducing
    /// real coupling, so the long arg list stays.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        cache: Cache,
        oauth: OauthStore,
        setup_token: SetupToken,
        embedder: EmbedderHandle,
        embedding_store: EmbeddingStore,
        metadata_store: MetadataStore,
        ann: Arc<AnnIndex>,
        recommend_model_version: ModelVersion,
        trace_store: TraceStore,
    ) -> Self {
        // Events + feedback live in the same SQLite file as the
        // embedding store — recommender state, shared migrations.
        let event_store = EventStore::new(embedding_store.pool().clone());
        let feedback = FeedbackStore::new(embedding_store.pool().clone());
        Self {
            inner: Arc::new(Inner {
                config,
                http: build_http_client(),
                cache,
                oauth,
                setup_token,
                sync: SyncStore::new(),
                embedder,
                embedding_store,
                metadata_store,
                event_store,
                feedback,
                ann,
                recommend_model_version,
                trace_store,
                placeholder_etags: RwLock::new(HashSet::new()),
                placeholder_revalidations: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn bearer_token(&self) -> &str {
        &self.inner.config.server.bearer_token
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub fn cache(&self) -> &Cache {
        &self.inner.cache
    }

    pub fn oauth(&self) -> &OauthStore {
        &self.inner.oauth
    }

    pub fn setup_token(&self) -> &SetupToken {
        &self.inner.setup_token
    }

    pub fn sync(&self) -> &SyncStore {
        &self.inner.sync
    }

    pub fn embedder(&self) -> &EmbedderHandle {
        &self.inner.embedder
    }

    pub fn ann(&self) -> &Arc<AnnIndex> {
        &self.inner.ann
    }

    pub fn embedding_store(&self) -> &EmbeddingStore {
        &self.inner.embedding_store
    }

    pub fn metadata_store(&self) -> &MetadataStore {
        &self.inner.metadata_store
    }

    pub fn event_store(&self) -> &EventStore {
        &self.inner.event_store
    }

    pub fn feedback(&self) -> &FeedbackStore {
        &self.inner.feedback
    }

    pub fn recommend_model_version(&self) -> &ModelVersion {
        &self.inner.recommend_model_version
    }

    pub fn trace_store(&self) -> &TraceStore {
        &self.inner.trace_store
    }

    pub fn placeholder_etags(&self) -> &RwLock<HashSet<String>> {
        &self.inner.placeholder_etags
    }

    pub fn placeholder_revalidations(&self) -> &Mutex<HashMap<String, Instant>> {
        &self.inner.placeholder_revalidations
    }
}
