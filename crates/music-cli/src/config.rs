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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub url: String,
    pub username: String,
    pub password: String,
}

// Hand-rolled `Debug` so the Navidrome password is never printed in logs
// or a panic dump. The derived impl would emit it verbatim.
impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub url: String,
    pub bearer_token: String,
    /// Path to a PEM file holding the CA that issued the gateway's TLS
    /// cert — typically the mkcert root CA (`mkcert -CAROOT`/`rootCA.pem`).
    /// Added as an *extra* trust anchor on top of the system store, so the
    /// CLI verifies a `gateway.local` cert without installing the CA
    /// system-wide. When unset, only the system trust store is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_cert_path: Option<PathBuf>,
    /// Escape hatch: disable TLS certificate verification entirely. This
    /// re-exposes the bearer token to anyone who can MITM the connection,
    /// so it's only for throwaway/debug setups — prefer `ca_cert_path`.
    /// Off by default; the CLI prints a loud warning when it's on.
    #[serde(default)]
    pub insecure_tls: bool,
}

// Hand-rolled `Debug` so the gateway bearer token is never printed.
impl std::fmt::Debug for GatewayConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayConfig")
            .field("url", &self.url)
            .field("bearer_token", &"[REDACTED]")
            .field("ca_cert_path", &self.ca_cert_path)
            .field("insecure_tls", &self.insecure_tls)
            .finish()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_secrets() {
        let server = ServerConfig {
            url: "http://nav.lan:4533".to_string(),
            username: "alice".to_string(),
            password: "hunter2".to_string(),
        };
        let gateway = GatewayConfig {
            url: "https://gateway.local:8443".to_string(),
            bearer_token: "super-secret-bearer".to_string(),
            ca_cert_path: Some(PathBuf::from("/home/alice/rootCA.pem")),
            insecure_tls: false,
        };

        let server_dbg = format!("{server:?}");
        assert!(!server_dbg.contains("hunter2"));
        assert!(server_dbg.contains("[REDACTED]"));
        // Non-secret fields stay visible for diagnostics.
        assert!(server_dbg.contains("alice"));

        let gateway_dbg = format!("{gateway:?}");
        assert!(!gateway_dbg.contains("super-secret-bearer"));
        assert!(gateway_dbg.contains("[REDACTED]"));
        // The CA path is not a secret and stays visible.
        assert!(gateway_dbg.contains("rootCA.pem"));
    }
}
