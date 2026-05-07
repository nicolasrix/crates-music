use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    /// When set, the CLI talks to a `music-gateway` instead of Navidrome
    /// directly. Auth becomes a single bearer token; the `[server]`
    /// credentials are unused in this mode but kept for clean fallback to
    /// direct mode without rewriting the config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayConfig>,
    #[serde(default)]
    pub cache: CacheConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub url: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub url: String,
    pub bearer_token: String,
}

/// On-disk audio cache parameters. Defaults: 10 GB regular, 5 GB pinned,
/// stored under `$XDG_CACHE_HOME/crates-music/audio/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Cache root. `None` → derive from XDG cache dir at runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default = "default_regular_budget")]
    pub regular_budget_bytes: u64,
    #[serde(default = "default_pinned_budget")]
    pub pinned_budget_bytes: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            path: None,
            regular_budget_bytes: default_regular_budget(),
            pinned_budget_bytes: default_pinned_budget(),
        }
    }
}

fn default_regular_budget() -> u64 {
    10 * 1024 * 1024 * 1024 // 10 GB
}

fn default_pinned_budget() -> u64 {
    5 * 1024 * 1024 * 1024 // 5 GB
}

/// Resolve cache root: explicit path if set, otherwise XDG cache.
pub fn resolve_cache_root(config: &CacheConfig) -> Option<PathBuf> {
    if let Some(p) = &config.path {
        return Some(p.clone());
    }
    ProjectDirs::from("dev", "crates-music", "crates-music").map(|d| d.cache_dir().join("audio"))
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading config at {}: {e}", path.display()))?;
        let config: Self = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parsing config at {}: {e}", path.display()))?;
        Ok(config)
    }
}

/// Returns the platform-appropriate default config path:
/// `$XDG_CONFIG_HOME/crates-music/config.toml` on Linux,
/// `~/Library/Application Support/...` on macOS, etc.
pub fn default_config_path() -> Option<PathBuf> {
    ProjectDirs::from("dev", "crates-music", "crates-music")
        .map(|d| d.config_dir().join("config.toml"))
}
