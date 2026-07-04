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
use music_recommend::PlayHistoryStore;
use music_recommend::ProjectionStore;
use music_recommend::RatingStore;
use music_recommend::RecommendationLogStore;
use music_recommend::SessionStore;
use music_recommend::TrackAffinityStore;
use music_recommend::WhiteningStore;
use music_recommend::ann::AnnIndex;
use music_recommend::metadata::MetadataStore;
use music_recommend::store::EmbeddingStore;
use music_recommend::types::ModelVersion;

use tokio::sync::Semaphore;

use crate::config::Config;
use crate::diagnostics::TraceStore;
use crate::embedder::EmbedderHandle;
use crate::oauth::{OauthStore, SetupToken};
use crate::playlists::PlaylistStore;
use crate::proxy::build_http_client;
use crate::ratelimit::{LoginLimiter, RateLimiter};
use crate::sync::SyncStore;

#[derive(Debug, Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

/// Resolved user-preference re-scoring parameters. Built by
/// [`AppState::preference_params`] only when the feature is enabled; the
/// recommend path treats its absence as "preference off → no-op".
#[derive(Clone, Copy, Debug)]
pub struct PreferenceParams {
    /// MMR affinity weight `β`.
    pub weight: f32,
    /// Affinity decay half-life in milliseconds.
    pub half_life_ms: i64,
}

#[derive(Debug)]
struct Inner {
    config: Config,
    http: reqwest::Client,
    cache: Cache,
    oauth: OauthStore,
    /// Gateway-owned playlists (PR F). Shares the OAuth pool — same
    /// `gateway-state.sqlite` file, migration `0007_playlists.sql`.
    playlists: PlaylistStore,
    setup_token: SetupToken,
    sync: SyncStore,
    embedder: EmbedderHandle,
    embedding_store: EmbeddingStore,
    metadata_store: MetadataStore,
    event_store: EventStore,
    play_history: PlayHistoryStore,
    feedback: FeedbackStore,
    /// Append-only log of what the recommender served (request context +
    /// ordered slate + per-item scores). Pure data capture for future
    /// model training; gated by `config.recommend.log_provenance`.
    recommendation_log: RecommendationLogStore,
    track_affinity: TrackAffinityStore,
    /// Durable per-track like/dislike — the user's explicit taste. A
    /// separate channel from `track_affinity` (that one decays; ratings
    /// don't). Drives always-on dislike-exclusion and like-boost in the
    /// recommend path, regardless of `preference_enabled`.
    ratings: RatingStore,
    projection: ProjectionStore,
    sessions: SessionStore,
    ann: Arc<AnnIndex>,
    /// Persistence for the ANN's whitening transform. The *live* transform
    /// lives in `ann` (shared with the ingest worker); this store just
    /// caches the fitted (mean, components) so the refit endpoint can
    /// persist them and a restart can reload without refitting.
    whitening_store: WhiteningStore,
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
    /// Brute-force throttle for `POST /oauth/login`. See `LoginLimiter`.
    login_limiter: LoginLimiter,
    /// Request-rate throttle for the public, unauthenticated OAuth
    /// endpoints (`/oauth/guest`, `/oauth/device_authorization`,
    /// `/oauth/revoke`). See `RateLimiter`.
    public_oauth_limiter: RateLimiter,
    /// Single-permit gate serialising `POST /v1/recommend/refit_whitening`.
    /// A refit is an expensive whole-corpus power-iteration plus
    /// embedder prompt-corpus calls; concurrent refits would duplicate
    /// that work and race on the persisted transform. `try_acquire`
    /// turns a second concurrent request into a fast 429.
    refit_gate: Arc<Semaphore>,
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
        // Events + play history both live in the same SQLite file as
        // the embedding store — recommender state, shared migrations.
        let event_store = EventStore::new(embedding_store.pool().clone());
        let play_history = PlayHistoryStore::new(embedding_store.pool().clone());
        let feedback = FeedbackStore::new(embedding_store.pool().clone());
        let recommendation_log = RecommendationLogStore::new(embedding_store.pool().clone());
        let track_affinity = TrackAffinityStore::new(embedding_store.pool().clone());
        let ratings = RatingStore::new(embedding_store.pool().clone());
        let projection = ProjectionStore::new(embedding_store.pool().clone());
        let sessions = SessionStore::new(embedding_store.pool().clone());
        let whitening_store = WhiteningStore::new(embedding_store.pool().clone());
        // Playlists live in the OAuth pool's DB (gateway-state.sqlite),
        // built here from the same pool so they share the migrated schema.
        let playlists = PlaylistStore::new(oauth.pool().clone());
        Self {
            inner: Arc::new(Inner {
                config,
                http: build_http_client(),
                cache,
                oauth,
                playlists,
                setup_token,
                sync: SyncStore::with_sessions(sessions.clone()),
                embedder,
                embedding_store,
                metadata_store,
                event_store,
                play_history,
                feedback,
                recommendation_log,
                track_affinity,
                ratings,
                projection,
                sessions,
                ann,
                whitening_store,
                recommend_model_version,
                trace_store,
                placeholder_etags: RwLock::new(HashSet::new()),
                placeholder_revalidations: Mutex::new(HashMap::new()),
                login_limiter: LoginLimiter::default(),
                public_oauth_limiter: RateLimiter::default(),
                refit_gate: Arc::new(Semaphore::new(1)),
            }),
        }
    }

    pub fn login_limiter(&self) -> &LoginLimiter {
        &self.inner.login_limiter
    }

    pub fn public_oauth_limiter(&self) -> &RateLimiter {
        &self.inner.public_oauth_limiter
    }

    pub fn refit_gate(&self) -> &Arc<Semaphore> {
        &self.inner.refit_gate
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

    pub fn playlists(&self) -> &PlaylistStore {
        &self.inner.playlists
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

    pub fn whitening_store(&self) -> &WhiteningStore {
        &self.inner.whitening_store
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

    pub fn play_history(&self) -> &PlayHistoryStore {
        &self.inner.play_history
    }

    pub fn feedback(&self) -> &FeedbackStore {
        &self.inner.feedback
    }

    pub fn recommendation_log(&self) -> &RecommendationLogStore {
        &self.inner.recommendation_log
    }

    /// Whether to persist recommendation provenance for model training.
    /// Gated so it can be turned off without a redeploy of the capture
    /// call sites.
    pub fn provenance_enabled(&self) -> bool {
        self.inner.config.recommend.log_provenance
    }

    pub fn track_affinity(&self) -> &TrackAffinityStore {
        &self.inner.track_affinity
    }

    pub fn ratings(&self) -> &RatingStore {
        &self.inner.ratings
    }

    /// Additive relevance bonus applied to a liked candidate when
    /// rescoring recommendations. Always-on (not gated by
    /// `preference_enabled`); from the `[recommend] like_bonus` config
    /// knob, defaulting to [`music_recommend::LIKE_BONUS`].
    pub fn like_bonus(&self) -> f32 {
        self.inner.config.recommend.like_bonus
    }

    /// Additive relevance bonus a candidate earns for belonging to a liked
    /// *album*. Always-on; from `[recommend] like_bonus_album`. Lower than
    /// [`Self::like_bonus`] (track > album > artist contribution order).
    pub fn like_bonus_album(&self) -> f32 {
        self.inner.config.recommend.like_bonus_album
    }

    /// Additive relevance bonus a candidate earns for belonging to a liked
    /// *artist*. Always-on; from `[recommend] like_bonus_artist`. The
    /// smallest of the three boosts.
    pub fn like_bonus_artist(&self) -> f32 {
        self.inner.config.recommend.like_bonus_artist
    }

    /// Default anchor-leash parameters from `[recommend] leash_tau /
    /// leash_lambda`. The `/from-seeds` handler uses these when the request
    /// omits its own `leash_tau` / `leash_lambda`, so ops can retune the leash
    /// without a web rebuild. The leash only engages when the request also
    /// supplies `anchor_track_ids`.
    pub fn leash_params(&self) -> music_recommend::LeashParams {
        music_recommend::LeashParams {
            tau: self.inner.config.recommend.leash_tau,
            lambda: self.inner.config.recommend.leash_lambda,
        }
    }

    /// Autoplay recency-exclusion window in milliseconds, or `None` when the
    /// feature is disabled (`recently_played_exclude_hours <= 0`). Tracks
    /// played within this window are hard-excluded from recommendation
    /// candidates. `Some(ms)` is the lookback the handler subtracts from now.
    pub fn recently_played_exclude_ms(&self) -> Option<i64> {
        hours_to_ms(self.inner.config.recommend.recently_played_exclude_hours)
    }

    /// Autoplay serve-cooldown window in milliseconds, or `None` when disabled
    /// (`served_cooldown_hours <= 0`). Tracks served within this window are
    /// suppressed from the next refills. Only meaningful when provenance
    /// logging is on — see [`Self::provenance_enabled`].
    pub fn served_cooldown_ms(&self) -> Option<i64> {
        hours_to_ms(self.inner.config.recommend.served_cooldown_hours)
    }

    /// Exploration temperature for the final autoplay pick (Gumbel-max
    /// sampling). `0` (or negative) ⇒ deterministic argmax. See
    /// [`music_recommend::explore`].
    pub fn explore_temperature(&self) -> f32 {
        self.inner.config.recommend.explore_temperature
    }

    /// Affinity decay half-life (ms) from config. Available regardless of
    /// `preference_enabled`: the affinity counter is captured on every
    /// play / skip / vote so the feature has full history the moment it's
    /// switched on. Only the *read* (the MMR bonus) is gated — see
    /// [`Self::preference_params`].
    pub fn affinity_half_life_ms(&self) -> i64 {
        music_recommend::half_life_days_to_ms(self.inner.config.recommend.affinity_half_life_days)
    }

    /// Resolved user-preference re-scoring parameters, or `None` when the
    /// feature is disabled in config. `Some` carries the MMR affinity
    /// weight `β` and the decay half-life in milliseconds — everything a
    /// recommend handler needs to turn a stored affinity into a relevance
    /// bonus. Centralised here so handlers don't re-read config knobs.
    pub fn preference_params(&self) -> Option<PreferenceParams> {
        let r = &self.inner.config.recommend;
        if !r.preference_enabled {
            return None;
        }
        Some(PreferenceParams {
            weight: r.preference_weight,
            half_life_ms: music_recommend::half_life_days_to_ms(r.affinity_half_life_days),
        })
    }

    pub fn projection(&self) -> &ProjectionStore {
        &self.inner.projection
    }

    pub fn sessions(&self) -> &SessionStore {
        &self.inner.sessions
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

/// Convert an hours knob into a millisecond window, or `None` when the knob
/// is zero/negative (feature disabled). Centralised so the recency-exclusion
/// and serve-cooldown accessors share one rounding rule.
fn hours_to_ms(hours: f32) -> Option<i64> {
    if hours <= 0.0 {
        return None;
    }
    // Realistic config values (single- to double-digit hours) are far inside
    // i64 ms range; the truncation the lint warns about can't occur here.
    #[allow(clippy::cast_possible_truncation)]
    Some((f64::from(hours) * 3_600_000.0) as i64)
}
