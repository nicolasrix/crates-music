//! Process-wide shared state cloned into every request handler.
//!
//! `AppState` is intentionally trivially `Clone` (Arc-backed where needed) so
//! axum's extractor system can hand it to handlers cheaply. The reqwest client
//! and L2 cache live here so connection pooling and the SQLite pool are
//! per-process, not per-request.

use std::sync::Arc;

use music_cache::Cache;

use crate::config::Config;
use crate::proxy::build_http_client;

#[derive(Debug, Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    config: Config,
    http: reqwest::Client,
    cache: Cache,
}

impl AppState {
    pub fn new(config: Config, cache: Cache) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                http: build_http_client(),
                cache,
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
}
