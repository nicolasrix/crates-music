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
