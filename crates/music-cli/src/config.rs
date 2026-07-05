use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

// `Eq` is intentionally not derived: `TuiConfig`'s autoplay drift params are
// floats (which are only `PartialEq`). Nothing keys a map/set on `Config`, so
// `PartialEq` is all that's used (test `assert_eq!`s).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    /// When set, the CLI talks to a `music-gateway` instead of Navidrome
    /// directly. Auth is the OAuth 2.1 Device Authorization Grant — run
    /// `crates-cli auth login` once; tokens are kept in a sibling
    /// `cli-tokens.json`. The `[server]` credentials are unused in this
    /// mode but kept for clean fallback to direct mode without rewriting
    /// the config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayConfig>,
    #[serde(default)]
    pub cache: CacheConfig,
    /// Interactive-TUI settings (autoplay drift params, etc.). Per-client by
    /// design, like the web client's localStorage — see [`TuiConfig`].
    #[serde(default)]
    pub tui: TuiConfig,
    /// Path this config was loaded from. Not part of the on-disk format
    /// (skipped by serde) — `Config::load` stamps it so siblings of the
    /// config file (e.g. the `cli-tokens.json` token store) can be located.
    #[serde(skip)]
    pub source_path: PathBuf,
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

// Derived `Debug` is fine now that the gateway holds no secret — the CLI
// authenticates with the Device Authorization Grant and keeps tokens in a
// separate `cli-tokens.json` store, not in this config.
impl std::fmt::Debug for GatewayConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayConfig")
            .field("url", &self.url)
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

/// Interactive-TUI configuration. Currently just the autoplay drift
/// parameters; Phase 7's Settings view will edit these in place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TuiConfig {
    #[serde(default)]
    pub autoplay: AutoplayConfig,
}

/// Tethered-drift autoplay parameters. Defaults mirror the web client's
/// `DEFAULT_AUTOPLAY_SETTINGS` so the TUI and PWA drift identically. The
/// frontier weights are folded into the seed list client-side; the leash and
/// MMR params ride the `from-seeds` request body and are applied server-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutoplayConfig {
    /// Start with autoplay already on. The web persists the last toggle in
    /// localStorage; the TUI reads the initial state from here (runtime
    /// toggling isn't persisted yet — that's Phase 7).
    #[serde(default)]
    pub enabled: bool,
    /// Refill the queue whenever fewer than this many tracks sit after the
    /// now-playing cursor (web `MIN_UPCOMING`).
    #[serde(default = "default_min_upcoming")]
    pub min_upcoming: usize,
    /// Boundary leash radius τ (whitened cosine) and strength λ — how hard a
    /// candidate is demoted for straying from the anchor set.
    #[serde(default = "default_leash_tau")]
    pub leash_tau: f32,
    #[serde(default = "default_leash_lambda")]
    pub leash_lambda: f32,
    /// Travel frontier: base weight β, per-step decay, and how many recently
    /// played items seed the direction of drift.
    #[serde(default = "default_frontier_weight")]
    pub frontier_weight: f32,
    #[serde(default = "default_frontier_decay")]
    pub frontier_decay: f32,
    #[serde(default = "default_frontier_window")]
    pub frontier_window: usize,
    /// MMR relevance/novelty tradeoff λ for the server-side diversity walk.
    #[serde(default = "default_mmr_lambda")]
    pub mmr_lambda: f32,
}

impl Default for AutoplayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_upcoming: default_min_upcoming(),
            leash_tau: default_leash_tau(),
            leash_lambda: default_leash_lambda(),
            frontier_weight: default_frontier_weight(),
            frontier_decay: default_frontier_decay(),
            frontier_window: default_frontier_window(),
            mmr_lambda: default_mmr_lambda(),
        }
    }
}

fn default_min_upcoming() -> usize {
    5
}
fn default_leash_tau() -> f32 {
    0.28
}
fn default_leash_lambda() -> f32 {
    16.0
}
fn default_frontier_weight() -> f32 {
    0.15
}
fn default_frontier_decay() -> f32 {
    0.55
}
fn default_frontier_window() -> usize {
    3
}
fn default_mmr_lambda() -> f32 {
    0.8
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
        let mut config: Self = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parsing config at {}: {e}", path.display()))?;
        config.source_path = path.to_path_buf();
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
            ca_cert_path: Some(PathBuf::from("/home/alice/rootCA.pem")),
            insecure_tls: false,
        };

        let server_dbg = format!("{server:?}");
        assert!(!server_dbg.contains("hunter2"));
        assert!(server_dbg.contains("[REDACTED]"));
        // Non-secret fields stay visible for diagnostics.
        assert!(server_dbg.contains("alice"));

        // The gateway block no longer holds a secret; the CA path is not
        // sensitive and stays visible.
        let gateway_dbg = format!("{gateway:?}");
        assert!(gateway_dbg.contains("rootCA.pem"));
    }
}
