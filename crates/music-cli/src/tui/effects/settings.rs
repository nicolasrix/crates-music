//! Settings effects: persist edits to the config file and apply them live,
//! plus the two account actions (sign-out, admin cache-invalidate). Kept out
//! of `effects.rs` so that file stays under the size cap.

use crate::api;
use crate::config::{AutoplayConfig, Config, Quality};
use crate::tui::msg::{Msg, SettingsSave};

use super::Ctx;

/// The slice of config the Settings view must apply to *running* effect tasks
/// without a restart: the transcode qualities (read on every audio fetch) and
/// the autoplay drift params (read on every refill). Held behind a mutex on
/// [`Ctx`] and swapped wholesale by the `SaveSettings` effect. Budgets are
/// *not* here — those live on the `AudioCache` itself (`set_budgets`), and the
/// App-mirrored knobs (min-upcoming, output device) are reducer state.
#[derive(Debug, Clone)]
pub(crate) struct LiveSettings {
    pub stream_quality: Quality,
    pub download_quality: Quality,
    pub autoplay: AutoplayConfig,
}

impl Ctx {
    /// Quality for tracks fetched to play. Read fresh on every fetch so a
    /// Settings change hits the next stream request.
    pub(crate) fn stream_quality(&self) -> Quality {
        self.live().stream_quality
    }

    /// Quality for tracks fetched to save offline (pin / bulk / warm).
    pub(crate) fn download_quality(&self) -> Quality {
        self.live().download_quality
    }

    /// A snapshot of the autoplay drift params for one refill.
    pub(crate) fn autoplay(&self) -> AutoplayConfig {
        self.live().autoplay.clone()
    }

    /// Overwrite the live settings (the `SaveSettings` effect, after a Settings
    /// edit) so the next fetch/refill sees the new values.
    fn set_live_settings(&self, next: LiveSettings) {
        *self.settings.lock().expect("live settings mutex poisoned") = next;
    }

    fn live(&self) -> LiveSettings {
        self.settings
            .lock()
            .expect("live settings mutex poisoned")
            .clone()
    }
}

/// Persist the settings to disk and apply them to the running session. The
/// config on disk is the boot base (`ctx.config`) with the edited fields
/// overwritten, so untouched sections (`[server]`, `[gateway]`) round-trip.
pub(super) async fn save(ctx: &Ctx, save: SettingsSave) -> Msg {
    // Apply live first — even if the disk write fails, the session should
    // honour what the user just set (the next fetch/refill/put reads these).
    let autoplay = save.autoplay();
    ctx.set_live_settings(LiveSettings {
        stream_quality: save.stream_quality,
        download_quality: save.download_quality,
        autoplay: autoplay.clone(),
    });
    ctx.cache
        .set_budgets(save.regular_budget_bytes, save.pinned_budget_bytes);
    if save.evict_now && let Err(e) = ctx.cache.evict_lru_to_fit().await {
        tracing::debug!(error = %e, "settings: eviction after budget change failed");
    }

    // Then persist. Build the on-disk config from the boot base + edits.
    let mut config = Config::clone(&ctx.config);
    config.playback = save.playback();
    config.cache.regular_budget_bytes = save.regular_budget_bytes;
    config.cache.pinned_budget_bytes = save.pinned_budget_bytes;
    config.tui.autoplay = autoplay;
    let result = config.save().map_err(|e| e.to_string());
    Msg::SettingsSaved { result }
}

/// Revoke + clear the token store, then report back. On success the reducer
/// exits the TUI to the auth-needed shell line.
pub(super) async fn sign_out(ctx: &Ctx) -> Msg {
    let result = match &ctx.config.gateway {
        Some(gw) => crate::auth::sign_out(&ctx.config, gw)
            .await
            .map_err(|e| e.to_string()),
        None => Err("not in gateway mode — nothing to sign out of".to_owned()),
    };
    Msg::SignedOut { result }
}

/// `POST /v1/admin/cache/invalidate` (admin-only).
pub(super) async fn invalidate_cache(ctx: &Ctx) -> Msg {
    let result = api::invalidate_cache(&ctx.config)
        .await
        .map_err(|e| e.to_string());
    Msg::CacheInvalidated { result }
}
