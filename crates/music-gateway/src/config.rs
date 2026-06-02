//! Gateway configuration loaded from TOML.
//!
//! The gateway is single-tenant for now (single Navidrome upstream, single
//! shared bearer token). OAuth — and per-device tokens — arrive in a later phase.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
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

// Hand-rolled `Debug` so the shared bearer token never lands in logs or a
// panic dump. The derived impl would print it verbatim; this redacts it
// while leaving the non-secret fields visible for diagnostics.
impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("listen", &self.listen)
            .field("tls_cert", &self.tls_cert)
            .field("tls_key", &self.tls_key)
            .field("bearer_token", &"[REDACTED]")
            .field("static_dir", &self.static_dir)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpstreamConfig {
    /// Base URL of the Navidrome instance (e.g. `http://nav.lan:4533`).
    pub navidrome_url: String,
    pub username: String,
    pub password: String,
}

// Hand-rolled `Debug` so the upstream Navidrome password is never printed.
impl std::fmt::Debug for UpstreamConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamConfig")
            .field("navidrome_url", &self.navidrome_url)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
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
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
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

// Hand-rolled `Debug` so the embedder bearer token never lands in logs.
// Distinguishes "set" from "unset" without revealing the value.
impl std::fmt::Debug for EmbedderConfigSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbedderConfigSection")
            .field("url", &self.url)
            .field("timeout_seconds", &self.timeout_seconds)
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

fn default_embedder_timeout_secs() -> u64 {
    30
}

/// Recommender configuration. The embedding dim has to be fixed at boot
/// because the ANN index commits to its dim on open — a mismatch between
/// the config and the on-disk ANN file will fail loudly at startup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecommendConfig {
    /// Shared dim of the audio + text embedding space. Must match what
    /// the embedder sidecar produces. LAION-CLAP is 512; CLaMP 3 is 768.
    /// Changing this requires deleting the ANN sidecar so it can be
    /// rebuilt from SQLite at the new dim.
    #[serde(default = "default_embedding_dim")]
    pub embedding_dim: usize,

    /// All-but-the-Top whitening of the content ANN. When true, the
    /// gateway fits (or loads) a per-model whitening transform at boot and
    /// the ANN stores + queries de-coned vectors — fixing CLaMP 3's
    /// embedding anisotropy that made text-query stations cluster. When
    /// false, the ANN holds raw vectors (legacy behaviour). Toggling this
    /// triggers a one-time ANN rebuild at the next boot.
    #[serde(default = "default_whitening_enabled")]
    pub whitening_enabled: bool,

    /// User-preference re-scoring. When true, the MMR relevance term is
    /// tilted by each candidate's affinity (likes/plays raise it,
    /// dislikes/skips lower it) — see `music_recommend::preference`.
    /// Default false: the feature ships dark until explicitly enabled,
    /// and a fresh install with no listening history is a no-op anyway
    /// (every affinity is 0).
    #[serde(default = "default_preference_enabled")]
    pub preference_enabled: bool,

    /// Weight `β` on the affinity bonus: `relevance += β · affinity`,
    /// with `affinity ∈ [-1, 1]`. Kept on the order of the artist
    /// penalty (~0.15) so preference reorders near-ties without
    /// overriding a clear acoustic-relevance gap. Clamped `>= 0` at use.
    #[serde(default = "default_preference_weight")]
    pub preference_weight: f32,

    /// Half-life (days) of the affinity decayed counter. Older signal
    /// fades toward zero with this half-life, so taste can drift.
    #[serde(default = "default_affinity_half_life_days")]
    pub affinity_half_life_days: f32,

    /// Additive relevance bonus a *liked* track earns when rescoring
    /// recommendations (`relevance += like_bonus`). Unlike
    /// `preference_weight` this is always-on — durable likes/dislikes are
    /// an explicit signal that applies regardless of `preference_enabled`.
    /// Same order as the preference weight: enough to pull a liked track in
    /// from just outside the raw top-N without swamping acoustic similarity.
    #[serde(default = "default_like_bonus")]
    pub like_bonus: f32,

    /// Additive relevance bonus a candidate earns for belonging to a
    /// *liked album*. Always-on, same channel as `like_bonus`. Lower than
    /// `like_bonus` so a directly-liked track always outranks a same-album
    /// sibling — the track > album > artist contribution hierarchy.
    #[serde(default = "default_like_bonus_album")]
    pub like_bonus_album: f32,

    /// Additive relevance bonus a candidate earns for belonging to a
    /// *liked artist*. The smallest of the three (broadest signal).
    /// `like_bonus_album + like_bonus_artist` is kept below `like_bonus`.
    #[serde(default = "default_like_bonus_artist")]
    pub like_bonus_artist: f32,

    /// Anchor-leash radius `τ` (cosine, whitened space). A `/from-seeds`
    /// candidate whose nearest-anchor similarity is `>= τ` pays no leash
    /// penalty; below it the penalty grows quadratically — keeping a
    /// *travelling* autoplay station within a soft boundary of the user's
    /// anchored tracks. Only takes effect when the request supplies
    /// `anchor_track_ids`; the per-request `leash_tau` overrides this default.
    #[serde(default = "default_leash_tau")]
    pub leash_tau: f32,

    /// Anchor-leash strength `λ`. Scales the quadratic penalty past `τ`;
    /// higher = tighter leash. `<= 0` disables the leash entirely. The
    /// per-request `leash_lambda` overrides this default.
    #[serde(default = "default_leash_lambda")]
    pub leash_lambda: f32,

    /// Recommendation provenance logging. When true, every served
    /// recommendation (the request context + the ordered slate + per-item
    /// scores) is persisted to the `recommendation` / `recommendation_item`
    /// tables — the training substrate for future learning-to-rank /
    /// supervised-metric models. Default on: it is pure append-only data
    /// capture with no effect on what gets recommended. Set false to stop
    /// capturing (e.g. to bound disk on a long-running deploy).
    #[serde(default = "default_log_provenance")]
    pub log_provenance: bool,
}

impl Default for RecommendConfig {
    fn default() -> Self {
        Self {
            embedding_dim: default_embedding_dim(),
            whitening_enabled: default_whitening_enabled(),
            preference_enabled: default_preference_enabled(),
            preference_weight: default_preference_weight(),
            affinity_half_life_days: default_affinity_half_life_days(),
            like_bonus: default_like_bonus(),
            like_bonus_album: default_like_bonus_album(),
            like_bonus_artist: default_like_bonus_artist(),
            leash_tau: default_leash_tau(),
            leash_lambda: default_leash_lambda(),
            log_provenance: default_log_provenance(),
        }
    }
}

fn default_embedding_dim() -> usize {
    512
}

fn default_whitening_enabled() -> bool {
    true
}

fn default_preference_enabled() -> bool {
    false
}

fn default_preference_weight() -> f32 {
    0.15
}

fn default_affinity_half_life_days() -> f32 {
    30.0
}

fn default_like_bonus() -> f32 {
    music_recommend::LIKE_BONUS
}

fn default_like_bonus_album() -> f32 {
    music_recommend::LIKE_BONUS_ALBUM
}

fn default_like_bonus_artist() -> f32 {
    music_recommend::LIKE_BONUS_ARTIST
}

fn default_leash_tau() -> f32 {
    music_recommend::DEFAULT_LEASH_TAU
}

fn default_leash_lambda() -> f32 {
    music_recommend::DEFAULT_LEASH_LAMBDA
}

fn default_log_provenance() -> bool {
    true
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
        // The config holds the bearer token and the upstream Navidrome
        // password in cleartext, so it should be owner-only (chmod 600).
        // Warn — but don't refuse to boot — if it's group/world-accessible;
        // failing hard here would be a footgun on a fresh deploy.
        #[cfg(unix)]
        warn_if_world_readable(path);
        let cfg = toml::from_str(&raw)?;
        Ok(cfg)
    }

    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        Ok(toml::from_str(s)?)
    }
}

/// True if any group or "other" permission bit is set — i.e. the file is
/// readable (or worse) by someone other than its owner. `0o077` masks the
/// group+other rwx bits; owner bits (`0o700`) are intentionally ignored.
#[cfg(unix)]
fn mode_is_group_or_world_accessible(mode: u32) -> bool {
    mode & 0o077 != 0
}

/// Log a warning if the config file is accessible beyond its owner. Best
/// effort: a stat failure is downgraded to debug rather than escalated,
/// since the file was just read successfully.
#[cfg(unix)]
fn warn_if_world_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::metadata(path) {
        Ok(meta) => {
            let mode = meta.permissions().mode();
            if mode_is_group_or_world_accessible(mode) {
                tracing::warn!(
                    path = %path.display(),
                    mode = format!("{:o}", mode & 0o777),
                    "gateway config is group/world-accessible but holds the \
                     bearer token and upstream password in cleartext; \
                     restrict it with: chmod 600 {}",
                    path.display(),
                );
            }
        }
        Err(e) => {
            tracing::debug!(
                path = %path.display(),
                error = %e,
                "could not stat gateway config for a permission check",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_secrets() {
        let server = ServerConfig {
            listen: "0.0.0.0:8443".parse().unwrap(),
            tls_cert: PathBuf::from("/etc/cert.pem"),
            tls_key: PathBuf::from("/etc/key.pem"),
            bearer_token: "super-secret-bearer".to_string(),
            static_dir: None,
        };
        let upstream = UpstreamConfig {
            navidrome_url: "http://nav.lan:4533".to_string(),
            username: "alice".to_string(),
            password: "hunter2".to_string(),
        };
        let embedder = EmbedderConfigSection {
            url: "http://gpu.lan:9000".to_string(),
            timeout_seconds: 30,
            bearer_token: Some("embedder-secret".to_string()),
        };

        let server_dbg = format!("{server:?}");
        assert!(!server_dbg.contains("super-secret-bearer"));
        assert!(server_dbg.contains("[REDACTED]"));
        // Non-secret fields stay visible for diagnostics.
        assert!(server_dbg.contains("/etc/cert.pem"));

        let upstream_dbg = format!("{upstream:?}");
        assert!(!upstream_dbg.contains("hunter2"));
        assert!(upstream_dbg.contains("[REDACTED]"));
        assert!(upstream_dbg.contains("alice"));

        let embedder_dbg = format!("{embedder:?}");
        assert!(!embedder_dbg.contains("embedder-secret"));
        assert!(embedder_dbg.contains("[REDACTED]"));
    }

    #[cfg(unix)]
    #[test]
    fn permission_predicate_flags_group_and_world_access() {
        // Owner-only is fine.
        assert!(!mode_is_group_or_world_accessible(0o600));
        assert!(!mode_is_group_or_world_accessible(0o400));
        assert!(!mode_is_group_or_world_accessible(0o700));
        // Any group or other bit trips it.
        assert!(mode_is_group_or_world_accessible(0o640)); // group read
        assert!(mode_is_group_or_world_accessible(0o604)); // other read
        assert!(mode_is_group_or_world_accessible(0o644));
        assert!(mode_is_group_or_world_accessible(0o660));
        assert!(mode_is_group_or_world_accessible(0o666));
    }

    #[test]
    fn debug_distinguishes_unset_embedder_token() {
        let embedder = EmbedderConfigSection {
            url: "http://gpu.lan:9000".to_string(),
            timeout_seconds: 30,
            bearer_token: None,
        };
        // Unset reads as `None`, not `[REDACTED]`, so the absence of a
        // token stays diagnosable.
        let dbg = format!("{embedder:?}");
        assert!(dbg.contains("None"));
        assert!(!dbg.contains("[REDACTED]"));
    }
}
