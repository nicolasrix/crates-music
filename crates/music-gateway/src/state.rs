//! Process-wide shared state cloned into every request handler.
//!
//! `AppState` is intentionally trivially `Clone` (Arc-backed where needed) so
//! axum's extractor system can hand it to handlers cheaply. The reqwest client,
//! L2 cache, and OAuth state DB live here so connection pooling and the SQLite
//! pools are per-process, not per-request.

use std::sync::Arc;

use music_cache::Cache;

use crate::config::Config;
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
}

impl AppState {
    pub fn new(config: Config, cache: Cache, oauth: OauthStore, setup_token: SetupToken) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                http: build_http_client(),
                cache,
                oauth,
                setup_token,
                sync: SyncStore::new(),
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
}
