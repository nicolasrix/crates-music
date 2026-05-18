//! Boot-time probe + handle for the optional embedder sidecar.
//!
//! Three terminal states for the gateway after boot:
//!
//! - `Disabled` — no `[embedder]` block in config. Recommender works
//!   in tag-only degraded mode. No periodic probing.
//! - `Configured(client, status)` — config present, client built. The
//!   `status` field captures whether the last health probe found the
//!   model loaded; the recommender re-probes lazily before each ingest
//!   batch and updates this. P6.3 just sets the boot-time value.
//!
//! All "is the recommender able to embed right now?" decisions read
//! `EmbedderHandle::ready()`.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use music_recommend::embedder::{EmbedderClient, EmbedderConfig, EmbedderHealth};
use url::Url;

use crate::config::EmbedderConfigSection;

#[derive(Debug, Clone)]
pub struct EmbedderHandle {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// `None` when no `[embedder]` block was configured; the gateway
    /// runs permanently in degraded mode.
    client: Option<EmbedderClient>,
    /// Last observed health. `Some(h)` after a successful probe (loaded
    /// or not), `None` if we've never reached the sidecar.
    last_health: RwLock<Option<EmbedderHealth>>,
}

impl EmbedderHandle {
    /// Build an `EmbedderHandle` from optional config and a
    /// previously-observed health (typically the result of
    /// `boot_probe`).
    pub fn new(client: Option<EmbedderClient>, health: Option<EmbedderHealth>) -> Self {
        Self {
            inner: Arc::new(Inner {
                client,
                last_health: RwLock::new(health),
            }),
        }
    }

    pub fn disabled() -> Self {
        Self::new(None, None)
    }

    pub fn client(&self) -> Option<&EmbedderClient> {
        self.inner.client.as_ref()
    }

    /// True iff the embedder is configured AND the last probe saw the
    /// model loaded. False covers: not configured, never probed,
    /// probed-but-loading, probed-but-unreachable.
    pub fn ready(&self) -> bool {
        self.inner
            .last_health
            .read()
            .ok()
            .and_then(|guard| guard.clone())
            .is_some_and(|h| h.reachable && h.model_loaded)
    }

    pub fn last_health(&self) -> Option<EmbedderHealth> {
        self.inner.last_health.read().ok()?.clone()
    }

    pub fn record_health(&self, h: EmbedderHealth) {
        if let Ok(mut guard) = self.inner.last_health.write() {
            *guard = Some(h);
        }
    }
}

/// Build a client + run a single boot-time probe. Logs the outcome and
/// always returns a usable handle: a sidecar that's down at boot time
/// shouldn't crash-loop the gateway, since recommend endpoints can run
/// in degraded mode.
pub async fn boot_probe(cfg: Option<&EmbedderConfigSection>) -> EmbedderHandle {
    let Some(cfg) = cfg else {
        tracing::info!("embedder: disabled (no [embedder] block in config)");
        return EmbedderHandle::disabled();
    };
    let url = match cfg.url.parse::<Url>() {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(
                error = %e,
                url = %cfg.url,
                "embedder: invalid url, running in degraded mode"
            );
            return EmbedderHandle::disabled();
        }
    };
    let client = match EmbedderClient::new(EmbedderConfig {
        url,
        timeout: Duration::from_secs(cfg.timeout_seconds),
        bearer_token: cfg.bearer_token.clone(),
    }) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "embedder: failed to build client, degraded mode");
            return EmbedderHandle::disabled();
        }
    };

    match client.healthz().await {
        Ok(h) if h.model_loaded => {
            tracing::info!(
                model_version = %h.model_version,
                dim = h.dim,
                device = h.device.as_deref().unwrap_or("unknown"),
                "embedder: ready"
            );
            EmbedderHandle::new(Some(client), Some(h))
        }
        Ok(h) => {
            tracing::warn!(
                model_version = %h.model_version,
                device = h.device.as_deref().unwrap_or("unknown"),
                "embedder: reachable but model not loaded — degraded mode (will retry)"
            );
            EmbedderHandle::new(Some(client), Some(h))
        }
        Err(e) => {
            tracing::warn!(error = %e, "embedder: unreachable — degraded mode");
            // Keep the client around so the ingest pipeline can retry
            // later without re-parsing config.
            EmbedderHandle::new(Some(client), None)
        }
    }
}
