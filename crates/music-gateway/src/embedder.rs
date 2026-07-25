//! Boot-time probe + background failover for the optional embedder sidecar.
//!
//! The gateway can be configured with an ordered list of embedder
//! endpoints (a primary `url` plus `fallback_urls`). A background loop
//! re-probes them every `probe_interval_seconds` and points the *active*
//! client at the first one whose `/healthz` reports the model loaded. This
//! gives free failover for the request paths that resolve the client live
//! (`/v1/recommend/station`, text queries): when the GPU box goes down and
//! a CPU fallback comes up, the gateway switches with no restart.
//!
//! States:
//!
//! - `disabled` — no `[embedder]` block. Recommender runs in tag-only
//!   degraded mode; no clients, no probing.
//! - configured — one or more clients in priority order. `active` indexes
//!   the client the last probe found healthy (or `NONE_ACTIVE` when none
//!   were), and `last_health` mirrors that probe so `ready()` reflects
//!   live state rather than a latched boot-time value.
//!
//! All "can the recommender embed right now?" decisions read
//! `EmbedderHandle::ready()`; all "which sidecar do I call?" decisions read
//! `EmbedderHandle::client()`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use music_recommend::embedder::{EmbedderClient, EmbedderConfig, EmbedderHealth};
use url::Url;

use crate::config::EmbedderConfigSection;

/// Sentinel for `Inner::active` meaning "no endpoint is currently healthy".
const NONE_ACTIVE: usize = usize::MAX;

#[derive(Debug, Clone)]
pub struct EmbedderHandle {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Embedder clients in priority order (primary first, then fallbacks).
    /// Empty when no `[embedder]` block was configured — the gateway then
    /// runs permanently in degraded mode.
    clients: Vec<EmbedderClient>,
    /// Index into `clients` of the endpoint the last probe found healthy,
    /// or `NONE_ACTIVE` if none were. `client()` falls back to the primary
    /// (index 0) when this is `NONE_ACTIVE`, so a configured-but-unhealthy
    /// handle still reports "not ready" rather than "not configured".
    active: AtomicUsize,
    /// Last observed health of the *active* endpoint. `Some(h)` after a
    /// probe reached a sidecar, `None` when nothing is reachable.
    last_health: RwLock<Option<EmbedderHealth>>,
}

impl EmbedderHandle {
    fn from_clients(
        clients: Vec<EmbedderClient>,
        active: usize,
        health: Option<EmbedderHealth>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                clients,
                active: AtomicUsize::new(active),
                last_health: RwLock::new(health),
            }),
        }
    }

    pub fn disabled() -> Self {
        Self::from_clients(Vec::new(), NONE_ACTIVE, None)
    }

    /// Single-endpoint constructor. `Some(client)` builds a one-element
    /// handle (active when `health` reports the model loaded); `None` is
    /// disabled. Kept for callers/tests that have exactly one endpoint.
    pub fn new(client: Option<EmbedderClient>, health: Option<EmbedderHealth>) -> Self {
        match client {
            None => Self::disabled(),
            Some(c) => {
                let active = if health.as_ref().is_some_and(|h| h.reachable && h.model_loaded) {
                    0
                } else {
                    NONE_ACTIVE
                };
                Self::from_clients(vec![c], active, health)
            }
        }
    }

    /// The currently-active client, or `None` when no `[embedder]` block
    /// was configured. When configured but no endpoint is healthy this
    /// returns the primary (index 0) so callers attempt it and surface a
    /// transport error rather than a misleading "not configured".
    pub fn client(&self) -> Option<&EmbedderClient> {
        if self.inner.clients.is_empty() {
            return None;
        }
        let i = self.inner.active.load(Ordering::Relaxed);
        let i = if i < self.inner.clients.len() { i } else { 0 };
        self.inner.clients.get(i)
    }

    /// True iff an endpoint is configured AND the last probe saw the model
    /// loaded. False covers: not configured, never reached, reachable but
    /// loading, all endpoints down.
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

    fn set_health(&self, h: Option<EmbedderHealth>) {
        if let Ok(mut guard) = self.inner.last_health.write() {
            *guard = h;
        }
    }

    pub fn record_health(&self, h: EmbedderHealth) {
        self.set_health(Some(h));
    }

    /// Probe every endpoint in priority order, point `active` at the first
    /// healthy one, and update `last_health`. Logs only on a *transition*
    /// (active endpoint changed, or recovered from / fell into degraded
    /// mode) to keep a 20-second loop quiet in steady state.
    async fn probe(&self) {
        // Remember a reachable-but-loading sidecar so degraded health still
        // carries device/version for diagnostics, rather than blanking it.
        let mut loading_health: Option<EmbedderHealth> = None;

        for (i, client) in self.inner.clients.iter().enumerate() {
            match client.healthz().await {
                Ok(h) if h.model_loaded => {
                    let prev = self.inner.active.swap(i, Ordering::Relaxed);
                    if prev != i {
                        tracing::info!(
                            index = i,
                            url = %client.base_url(),
                            device = h.device.as_deref().unwrap_or("unknown"),
                            model_version = %h.model_version,
                            "embedder: active endpoint switched (now healthy)"
                        );
                    }
                    self.record_health(h);
                    return;
                }
                Ok(h) => {
                    if loading_health.is_none() {
                        loading_health = Some(h);
                    }
                }
                Err(_) => {}
            }
        }

        // No endpoint reported the model loaded.
        let prev = self.inner.active.swap(NONE_ACTIVE, Ordering::Relaxed);
        if prev != NONE_ACTIVE {
            tracing::warn!(
                endpoints = self.inner.clients.len(),
                "embedder: no endpoint healthy — degraded mode (recommend/station 503 until one recovers)"
            );
        }
        // A reachable-but-loading sidecar keeps device/version visible;
        // total unreachability is the honest `None`.
        self.set_health(loading_health);
    }
}

/// Build clients for every configured endpoint and run one boot-time probe
/// pass. Always returns a usable handle — a sidecar that's down at boot
/// must not crash-loop the gateway, since recommend endpoints degrade
/// gracefully and the background loop will pick a sidecar up later.
pub async fn boot_probe(cfg: Option<&EmbedderConfigSection>) -> EmbedderHandle {
    let Some(cfg) = cfg else {
        tracing::info!("embedder: disabled (no [embedder] block in config)");
        return EmbedderHandle::disabled();
    };

    let urls = cfg.effective_urls();
    if urls.is_empty() {
        tracing::warn!("embedder: [embedder] block has no usable url — degraded mode");
        return EmbedderHandle::disabled();
    }

    let mut clients = Vec::with_capacity(urls.len());
    for raw in &urls {
        let url = match raw.parse::<Url>() {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(error = %e, url = %raw, "embedder: skipping invalid url");
                continue;
            }
        };
        match EmbedderClient::new(EmbedderConfig {
            url,
            timeout: Duration::from_secs(cfg.timeout_seconds),
            bearer_token: cfg.bearer_token.clone(),
        }) {
            Ok(c) => clients.push(c),
            Err(e) => {
                tracing::warn!(error = %e, url = %raw, "embedder: failed to build client, skipping");
            }
        }
    }

    if clients.is_empty() {
        tracing::warn!("embedder: no valid endpoints — degraded mode");
        return EmbedderHandle::disabled();
    }

    tracing::info!(
        endpoints = clients.len(),
        primary = %urls[0],
        "embedder: probing endpoints"
    );
    // Start with the primary as the default active so an unprobed handle
    // still hands ingest a client; `probe` then corrects it.
    let handle = EmbedderHandle::from_clients(clients, 0, None);
    handle.probe().await;
    match handle.last_health() {
        Some(h) if h.reachable && h.model_loaded => tracing::info!(
            model_version = %h.model_version,
            dim = h.dim,
            device = h.device.as_deref().unwrap_or("unknown"),
            "embedder: ready"
        ),
        Some(_) => {
            tracing::warn!("embedder: reachable but model not loaded — degraded mode (will retry)");
        }
        None => tracing::warn!("embedder: unreachable — degraded mode (will retry)"),
    }
    handle
}

/// Spawn the background re-probe loop. No-op when probing is disabled
/// (`interval == 0`) or there are no clients to probe. The first interval
/// tick fires immediately and is skipped, since `boot_probe` already ran
/// one pass.
pub fn spawn_probe_loop(handle: EmbedderHandle, interval: Duration) {
    if interval.is_zero() || handle.inner.clients.is_empty() {
        tracing::info!("embedder: background re-probe disabled");
        return;
    }
    tracing::info!(
        interval_secs = interval.as_secs(),
        "embedder: background re-probe enabled"
    );
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tick.tick().await; // immediate first tick — boot already probed
        loop {
            tick.tick().await;
            handle.probe().await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use music_recommend::ModelVersion;

    fn health(reachable: bool, loaded: bool) -> EmbedderHealth {
        EmbedderHealth {
            reachable,
            model_loaded: loaded,
            model_version: ModelVersion::from("test-model".to_string()),
            dim: 768,
            device: Some("cpu".to_string()),
        }
    }

    fn client(url: &str) -> EmbedderClient {
        EmbedderClient::new(EmbedderConfig {
            url: url.parse().unwrap(),
            timeout: Duration::from_secs(1),
            bearer_token: None,
        })
        .unwrap()
    }

    #[test]
    fn disabled_handle_has_no_client_and_is_not_ready() {
        let h = EmbedderHandle::disabled();
        assert!(h.client().is_none());
        assert!(!h.ready());
    }

    #[test]
    fn configured_handle_reports_not_ready_until_health_recorded() {
        // A configured handle returns the primary client (so the error is
        // "not ready", not "not configured") but ready() stays false until
        // a probe records loaded health.
        let h = EmbedderHandle::from_clients(vec![client("http://a:9000")], 0, None);
        assert!(h.client().is_some());
        assert!(!h.ready());

        h.record_health(health(true, true));
        assert!(h.ready());
    }

    #[test]
    fn reachable_but_not_loaded_is_not_ready() {
        let h = EmbedderHandle::from_clients(vec![client("http://a:9000")], 0, None);
        h.record_health(health(true, false));
        assert!(!h.ready());
    }

    #[test]
    fn client_falls_back_to_primary_when_no_active() {
        let h = EmbedderHandle::from_clients(
            vec![
                client("http://primary:9000"),
                client("http://fallback:9000"),
            ],
            NONE_ACTIVE,
            None,
        );
        // NONE_ACTIVE → primary (index 0), so callers still attempt it.
        assert_eq!(
            h.client().unwrap().base_url().as_str(),
            "http://primary:9000/"
        );
    }

    #[test]
    fn active_index_selects_the_fallback_client() {
        let h = EmbedderHandle::from_clients(
            vec![
                client("http://primary:9000"),
                client("http://fallback:9000"),
            ],
            1,
            Some(health(true, true)),
        );
        assert_eq!(
            h.client().unwrap().base_url().as_str(),
            "http://fallback:9000/"
        );
        assert!(h.ready());
    }
}
