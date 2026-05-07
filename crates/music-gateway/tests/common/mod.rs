//! Shared test fixtures.
#![allow(dead_code, unreachable_pub)]

use std::net::SocketAddr;
use std::path::PathBuf;

use music_cache::Cache;
use music_gateway::Config;
use music_gateway::config::{CacheConfig, ServerConfig, UpstreamConfig};
use music_gateway::state::AppState;

pub const TEST_BEARER: &str = "test-bearer-token";

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
        },
        upstream: UpstreamConfig {
            navidrome_url: url.to_string(),
            username: username.to_string(),
            password: password.to_string(),
        },
        cache: CacheConfig::default(),
    }
}

pub async fn build_state(config: Config) -> AppState {
    let cache = Cache::open_in_memory()
        .await
        .expect("in-memory cache opens cleanly");
    AppState::new(config, cache)
}

pub fn build_state_with_cache(config: Config, cache: Cache) -> AppState {
    AppState::new(config, cache)
}
