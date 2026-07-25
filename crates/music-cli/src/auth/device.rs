//! `crates-cli auth login|logout|status`: the Device Authorization Grant flow
//! (RFC 8628) and token-store management.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::auth::store::{self, StoredTokens};
use crate::auth::{CLIENT_ID, DEVICE_GRANT, TokenError, TokenResponse, token_store_path};
use crate::config::{Config, GatewayConfig};
use crate::gateway::{endpoint, http_client};

/// RFC 8628 §3.2 device-authorization response (the fields we use).
#[derive(Debug, Deserialize)]
struct DeviceAuthResponse {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: String,
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_interval() -> u64 {
    5
}

/// `crates-cli auth login` — run the device flow and persist the tokens.
pub async fn run_login(config: &Config, gw: &GatewayConfig) -> Result<()> {
    let http = http_client(gw)?;

    // 1. Ask the gateway for a device code.
    let da: DeviceAuthResponse = {
        let resp = http
            .post(endpoint(gw, "/oauth/device_authorization"))
            .form(&[("client_id", CLIENT_ID)])
            .send()
            .await
            .context("requesting a device code")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("device authorization rejected ({status}): {body}");
        }
        serde_json::from_str(&body).context("parsing device authorization response")?
    };

    // 2. Show the user where to go. We build the verification URL from the
    //    gateway URL we already know rather than trusting the server's
    //    (which may be relative or carry a proxy-internal host).
    let verify = format!("{}/oauth/device", gw.url.trim_end_matches('/'));
    println!("To authorize this device, open:\n");
    println!("    {verify}\n");
    println!("and enter the code:\n");
    println!("    {}\n", da.user_code);
    if !da.verification_uri.is_empty() && da.verification_uri != verify {
        // Surface the server's view too, in case the deployment differs.
        println!("(server reports verification_uri: {})\n", da.verification_uri);
    }
    println!("Waiting for approval…");

    // 3. Poll the token endpoint until approval, denial, or expiry.
    let path = token_store_path(config);
    let deadline = store::now_ms() + i64::try_from(da.expires_in.saturating_mul(1000)).unwrap_or(i64::MAX);
    let mut interval = da.interval.max(1);
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if store::now_ms() > deadline {
            bail!("the device code expired before approval — run `crates-cli auth login` again");
        }

        let resp = http
            .post(endpoint(gw, "/oauth/token"))
            .form(&[
                ("grant_type", DEVICE_GRANT),
                ("client_id", CLIENT_ID),
                ("device_code", &da.device_code),
            ])
            .send()
            .await
            .context("polling the token endpoint")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if status.is_success() {
            let tr: TokenResponse =
                serde_json::from_str(&body).context("parsing token response")?;
            let tokens = StoredTokens::from_response(
                CLIENT_ID,
                tr.access_token,
                tr.refresh_token,
                tr.expires_in,
            );
            store::save(&path, &tokens)?;
            println!("\nAuthenticated. Tokens saved to {}", path.display());
            return Ok(());
        }

        let err: TokenError = serde_json::from_str(&body).unwrap_or_default();
        match err.error.as_str() {
            "authorization_pending" => {} // keep waiting
            "slow_down" => interval = interval.saturating_add(5), // RFC 8628 §3.5
            "access_denied" => bail!("the authorization request was denied in the browser"),
            "expired_token" => {
                bail!("the device code expired — run `crates-cli auth login` again")
            }
            other => bail!("token endpoint error ({status}): {other} {}", err.error_description),
        }
    }
}

/// `crates-cli auth logout` — best-effort revoke the refresh token, then drop
/// the local store.
pub async fn run_logout(config: &Config, gw: &GatewayConfig) -> Result<()> {
    let path = token_store_path(config);
    let Some(tokens) = store::load(&path)? else {
        println!("not logged in");
        return Ok(());
    };

    // Best effort: a failed revoke (gateway down) still clears local creds.
    // Guests have no refresh token — nothing to revoke, just drop the store.
    match (http_client(gw), tokens.refresh_token.as_deref()) {
        (Ok(http), Some(refresh_token)) => {
            let _ = http
                .post(endpoint(gw, "/oauth/revoke"))
                .form(&[("token", refresh_token)])
                .send()
                .await;
        }
        (Err(e), _) => eprintln!("warning: could not build client to revoke remotely: {e}"),
        (Ok(_), None) => {} // guest: no refresh to revoke
    }

    store::delete(&path)?;
    println!("logged out (token store removed)");
    Ok(())
}

/// `crates-cli auth status` — report whether we hold tokens and when the access
/// token expires. Purely local — never touches the gateway.
pub fn run_status(config: &Config) -> Result<()> {
    let path = token_store_path(config);
    match store::load(&path)? {
        None => {
            println!("not authenticated — run `crates-cli auth login`");
        }
        Some(tokens) => {
            let remaining_ms = tokens.access_expires_at_ms - store::now_ms();
            if tokens.is_guest() {
                println!("authenticated as a guest (client '{}')", tokens.client_id);
                println!("token store: {}", path.display());
                if remaining_ms > 0 {
                    println!("guest session valid for ~{}m (no refresh)", remaining_ms / 60_000);
                } else {
                    println!("guest session expired — redeem a new code with `crates-cli auth guest <code>`");
                }
            } else {
                println!("authenticated as client '{}'", tokens.client_id);
                println!("token store: {}", path.display());
                if remaining_ms > 0 {
                    println!("access token valid for ~{}m", remaining_ms / 60_000);
                } else {
                    println!("access token expired (refreshes automatically on next use)");
                }
            }
        }
    }
    Ok(())
}
