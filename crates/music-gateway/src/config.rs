//! Gateway configuration loaded from TOML.
//!
//! The gateway is single-tenant for now (single Navidrome upstream, single
//! shared bearer token). OAuth — and per-device tokens — arrive in a later phase.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub server: ServerConfig,
    pub upstream: UpstreamConfig,
    pub cache: CacheConfig,
    #[serde(default)]
    pub oauth: OauthConfig,
    /// Recommender knobs that have to be known at boot — chiefly the ANN
    /// vector dim, which the ANN index commits to at open time.
    #[serde(default)]
    pub recommend: RecommendConfig,
    /// Optional Python embedder sidecar. Absent / unreachable = degraded mode.
    #[serde(default)]
    pub embedder: Option<EmbedderConfigSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    /// Address the gateway binds to (e.g. `0.0.0.0:8443`).
    pub listen: SocketAddr,
    /// PEM-encoded TLS certificate path (mkcert output).
    pub tls_cert: PathBuf,
    /// PEM-encoded TLS key path.
    pub tls_key: PathBuf,
    /// Single shared bearer token. Static for P1; OAuth in a later phase.
    pub bearer_token: String,
    /// Optional directory of built web SPA assets (`apps/web/dist`). When
    /// set, the gateway serves it as a same-origin static site with SPA
    /// fallback to `index.html`. Defaults to `None` — leave unset in
    /// dev where Vite serves the SPA itself.
    #[serde(default)]
    pub static_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpstreamConfig {
    /// Base URL of the Navidrome instance (e.g. `http://nav.lan:4533`).
    pub navidrome_url: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheConfig {
    /// Path to the SQLite file backing the L2 metadata cache.
    pub path: PathBuf,
    /// TTL applied to browse-endpoint responses (in seconds).
    pub browse_ttl_seconds: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("gateway-cache.sqlite"),
            browse_ttl_seconds: 24 * 60 * 60,
        }
    }
}

/// OAuth 2.1 state DB (users, clients, codes, tokens). Deliberately a
/// separate SQLite file from the L2 cache: the cache is throwaway, this
/// DB holds irreplaceable refresh tokens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OauthConfig {
    pub state_db: PathBuf,
    /// Pre-declared OAuth clients, registered at startup. Keeps the
    /// client list under config control (Git-trackable) instead of
    /// requiring a separate admin endpoint for the simple cases.
    #[serde(default, rename = "clients")]
    pub clients: Vec<OauthClientConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OauthClientConfig {
    pub client_id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
}

/// Embedder sidecar config. Optional: if absent, the gateway boots in
/// degraded mode and never tries to embed. If present but unreachable
/// at boot, same outcome — a warning is logged and recommend endpoints
/// fall back to tag-only similarity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbedderConfigSection {
    pub url: String,
    /// Per-request timeout in seconds. Defaults to 30 — embedding a
    /// 120-second audio clip on CPU can take 10+ seconds.
    #[serde(default = "default_embedder_timeout_secs")]
    pub timeout_seconds: u64,
    /// Optional bearer token for split-host deployments. When set,
    /// every outgoing request to the embedder carries
    /// `Authorization: Bearer <token>`. Must match the embedder's
    /// `EMBEDDER_BEARER_TOKEN`. Omit (or leave null) for same-host
    /// deployments where the docker bridge is the trust boundary.
    #[serde(default)]
    pub bearer_token: Option<String>,
}

fn default_embedder_timeout_secs() -> u64 {
    30
}

/// Recommender configuration. The embedding dim has to be fixed at boot
/// because the ANN index commits to its dim on open — a mismatch between
/// the config and the on-disk ANN file will fail loudly at startup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecommendConfig {
    /// Shared dim of the audio + text embedding space. Must match what
    /// the embedder sidecar produces. LAION-CLAP is 512; CLaMP 3 is 768.
    /// Changing this requires deleting the ANN sidecar so it can be
    /// rebuilt from SQLite at the new dim.
    #[serde(default = "default_embedding_dim")]
    pub embedding_dim: usize,
}

impl Default for RecommendConfig {
    fn default() -> Self {
        Self {
            embedding_dim: default_embedding_dim(),
        }
    }
}

fn default_embedding_dim() -> usize {
    512
}

impl Default for OauthConfig {
    fn default() -> Self {
        Self {
            state_db: PathBuf::from("gateway-state.sqlite"),
            clients: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading config: {0}")]
    Io(#[from] std::io::Error),
    #[error("parsing config: {0}")]
    Toml(#[from] toml::de::Error),
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path)?;
        let cfg = toml::from_str(&raw)?;
        Ok(cfg)
    }

    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        Ok(toml::from_str(s)?)
    }
}
