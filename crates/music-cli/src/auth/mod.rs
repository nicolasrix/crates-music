//! Gateway authentication: the OAuth 2.1 Device Authorization Grant
//! (RFC 8628) and the token store that backs every gateway request.
//!
//! The CLI has no static bearer token. `music auth login` runs the device
//! flow and persists the resulting tokens (see [`store`]); every
//! gateway-backed command then calls [`resolve_bearer`], which returns the
//! cached access token or silently rotates it via the refresh grant.

pub mod device;
pub mod store;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::cli::AuthAction;
use crate::config::{Config, GatewayConfig, default_config_path};
use crate::gateway::{endpoint, http_client, require_gateway};

/// The OAuth client_id the CLI registers as (matches the pre-declared
/// `[[oauth.clients]] cli` block in the gateway config).
pub const CLIENT_ID: &str = "cli";

/// RFC 8628 grant type URN used when polling the token endpoint.
pub const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Refresh a little before the real deadline so a request never races a
/// just-expired token.
const EXPIRY_SKEW_MS: i64 = 30_000;

/// Token-endpoint success body (shared by the device poll and refresh).
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

/// Token-endpoint error body (RFC 6749 §5.2 / RFC 8628 §3.5).
#[derive(Debug, Deserialize, Default)]
pub struct TokenError {
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub error_description: String,
}

/// Where the token store lives: `cli-tokens.json` next to the config file
/// (falling back to the default config dir, then the cwd).
pub fn token_store_path(config: &Config) -> PathBuf {
    let dir = config
        .source_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(|| default_config_path().and_then(|p| p.parent().map(Path::to_path_buf)))
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("cli-tokens.json")
}

/// Resolve a valid gateway access token, refreshing if the cached one has
/// expired. Errors (telling the user to `music auth login`) when there's
/// no stored token or the refresh grant is rejected.
///
/// Concurrency note: two CLI invocations refreshing at the same instant
/// can both rotate, and the loser's just-rotated refresh token is revoked
/// — the next call simply re-runs the device flow. For a single-user CLI
/// this race is rare and self-healing, so it's left unguarded.
pub async fn resolve_bearer(config: &Config, gw: &GatewayConfig) -> Result<String> {
    let path = token_store_path(config);
    let tokens = store::load(&path)?.ok_or_else(|| {
        anyhow::anyhow!("not authenticated with the gateway — run `music auth login`")
    })?;

    if store::now_ms() < tokens.access_expires_at_ms - EXPIRY_SKEW_MS {
        return Ok(tokens.access_token);
    }

    // Access token expired (or about to): rotate via the refresh grant.
    let refreshed = refresh(gw, &tokens.client_id, &tokens.refresh_token)
        .await
        .context("refreshing the gateway access token — run `music auth login` if this persists")?;
    store::save(&path, &refreshed)?;
    Ok(refreshed.access_token)
}

/// Exchange a refresh token for a fresh pair (rotation). The gateway
/// revokes the presented refresh token, so the new pair must be persisted.
async fn refresh(
    gw: &GatewayConfig,
    client_id: &str,
    refresh_token: &str,
) -> Result<store::StoredTokens> {
    let url = endpoint(gw, "/oauth/token");
    let resp = http_client(gw)?
        .post(&url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .context("token refresh request")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("token refresh rejected ({status}): {body}");
    }
    let tr: TokenResponse = serde_json::from_str(&body).context("parsing token response")?;
    Ok(store::StoredTokens::from_response(
        client_id,
        tr.access_token,
        tr.refresh_token,
        tr.expires_in,
    ))
}

/// Dispatch `music auth <action>`. Runs *before* the Subsonic client is
/// built (login has no token yet), so it only needs the `[gateway]` block.
pub async fn run_auth(config: &Config, action: &AuthAction) -> Result<()> {
    let gw = require_gateway(config)?;
    match action {
        AuthAction::Login => device::run_login(config, gw).await,
        AuthAction::Logout => device::run_logout(config, gw).await,
        AuthAction::Status => device::run_status(config),
    }
}
