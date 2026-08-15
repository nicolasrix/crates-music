//! Shared test fixtures.
#![allow(dead_code, unreachable_pub)]

use std::net::SocketAddr;
use std::path::PathBuf;

use std::sync::Arc;

use music_cache::Cache;
use music_gateway::Config;
use music_gateway::config::{
    CacheConfig, DiscoveryConfig, LyricsConfig, OauthConfig, RecommendConfig, ServerConfig,
    UpstreamConfig,
};
use music_gateway::diagnostics::TraceStore;
use music_gateway::embedder::EmbedderHandle;
use music_gateway::oauth::{OauthStore, SetupToken};
use music_gateway::state::AppState;
use music_recommend::ann::AnnIndex;
use music_recommend::metadata::MetadataStore;
use music_recommend::store::EmbeddingStore;
use music_recommend::types::ModelVersion;

const TEST_DIM: usize = 8;

pub const TEST_BEARER: &str = "test-bearer-token";

/// Poll the cache until `key` is present, or panic after a second. The
/// browse-cache write is a fire-and-forget `tokio::spawn` in
/// `proxy::browse_proxy` (intentional — saves ~13–20 ms p99 per request
/// at the cost of a benign one-time upstream-redundancy on a tight race),
/// so a test that populates the cache via one request and then asserts
/// about the cached state of a *second* request must wait for the
/// background write to land or it flakes. Use this whenever the test
/// pattern is "fire request, then assert cache hit / 304 / wiremock
/// `.expect(1)`".
pub async fn wait_for_cache_entry(cache: &music_cache::Cache, key: &str) {
    for _ in 0..50 {
        if cache.get(key).await.unwrap().is_some() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("cache entry for {key} did not commit within 1s");
}

pub fn test_config() -> Config {
    test_config_with_upstream("http://nav.test", "alice", "sesame")
}

pub fn test_config_with_upstream(url: &str, username: &str, password: &str) -> Config {
    Config {
        server: ServerConfig {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            tls_cert: PathBuf::from("/dev/null"),
            tls_key: PathBuf::from("/dev/null"),
            bearer_token: TEST_BEARER.to_string(),
            static_dir: None,
        },
        upstream: UpstreamConfig {
            navidrome_url: url.to_string(),
            username: username.to_string(),
            password: password.to_string(),
        },
        cache: CacheConfig::default(),
        oauth: OauthConfig::default(),
        // Deterministic recommend baseline: the autoplay anti-repetition knobs
        // (recency exclusion, serve-cooldown, exploration jitter) are runtime
        // behaviours that would make exact-ranking assertions flaky or
        // stateful across calls. Tests that exercise those features opt back
        // in explicitly (see the recency/exploration tests in recommend.rs).
        recommend: RecommendConfig {
            recently_played_exclude_hours: 0.0,
            served_cooldown_hours: 0.0,
            explore_temperature: 0.0,
            ..RecommendConfig::default()
        },
        embedder: None,
        search: Default::default(),
        // Discovery is a background timer against the upstream; tests
        // that want a scan drive `CatalogWatcher` directly so nothing
        // races with their wiremock expectations.
        discovery: DiscoveryConfig {
            enabled: false,
            ..DiscoveryConfig::default()
        },
        // Lyrics resolution stays on (the routes need a resolver), but the
        // external provider is off by default so no test can reach the
        // real internet. Tests that exercise the external tiers point
        // `provider_url` at their own wiremock and flip this back on.
        lyrics: LyricsConfig {
            external_lookup: false,
            ..LyricsConfig::default()
        },
    }
}

pub async fn build_state(config: Config) -> AppState {
    let cache = Cache::open_in_memory()
        .await
        .expect("in-memory cache opens cleanly");
    let oauth = OauthStore::open_in_memory()
        .await
        .expect("in-memory oauth store opens cleanly");
    build_state_full(
        config,
        cache,
        oauth,
        SetupToken::none(),
        EmbedderHandle::disabled(),
    )
    .await
}

/// Build state with a caller-provided embedder handle. Used by tests
/// that wire wiremock-backed clients (recommend station, etc.).
pub async fn build_state_with_embedder(config: Config, embedder: EmbedderHandle) -> AppState {
    let cache = Cache::open_in_memory()
        .await
        .expect("in-memory cache opens cleanly");
    let oauth = OauthStore::open_in_memory()
        .await
        .expect("in-memory oauth store opens cleanly");
    build_state_full(config, cache, oauth, SetupToken::none(), embedder).await
}

pub async fn build_state_with_cache(config: Config, cache: Cache) -> AppState {
    let oauth = OauthStore::open_in_memory()
        .await
        .expect("in-memory oauth store opens cleanly");
    build_state_full(
        config,
        cache,
        oauth,
        SetupToken::none(),
        EmbedderHandle::disabled(),
    )
    .await
}

pub async fn build_state_with_oauth(
    config: Config,
    oauth: OauthStore,
    setup_token: SetupToken,
) -> AppState {
    let cache = Cache::open_in_memory()
        .await
        .expect("in-memory cache opens cleanly");
    build_state_full(
        config,
        cache,
        oauth,
        setup_token,
        EmbedderHandle::disabled(),
    )
    .await
}

async fn build_state_full(
    config: Config,
    cache: Cache,
    oauth: OauthStore,
    setup_token: SetupToken,
    embedder: EmbedderHandle,
) -> AppState {
    // EmbeddingStore lives in its own SQLite file in production; tests
    // use an in-memory variant so we never touch disk.
    let embedding_store = EmbeddingStore::open_in_memory()
        .await
        .expect("in-memory embedding store opens");
    // Sibling store on the same in-memory pool — production wires both
    // against the recommend DB (see boot_recommender in main.rs).
    let metadata_store = MetadataStore::new(embedding_store.pool().clone());
    let ann = Arc::new(AnnIndex::open_in_memory(TEST_DIM, 16).expect("ann opens"));
    let trace_store = TraceStore::open_in_memory()
        .await
        .expect("in-memory trace store opens");
    AppState::new(
        config,
        cache,
        oauth,
        setup_token,
        embedder,
        embedding_store,
        metadata_store,
        ann,
        ModelVersion::from("test-v1"),
        trace_store,
    )
}
