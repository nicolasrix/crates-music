//! `crates-cli auth guest <code> [--name <display>]`: redeem a shared guest
//! code for an ephemeral, refresh-less guest session (`POST /oauth/guest`).
//!
//! Unlike the device flow, guesting needs no prior authentication and no
//! browser — the code *is* the credential. The minted principal joins the
//! host's sync room; when the session lapses (`guest_session_ttl_seconds`),
//! the visitor redeems the code again (see D4 in the parity plan).

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::auth::store::{self, StoredTokens};
use crate::auth::{CLIENT_ID, token_store_path};
use crate::config::{Config, GatewayConfig};
use crate::gateway::{endpoint, http_client};

/// `POST /oauth/guest` success body (mirrors the gateway's
/// `GuestTokenResponse`). No refresh token — guests are transient.
#[derive(Debug, Deserialize)]
struct GuestResponse {
    access_token: String,
    expires_in: u64,
    /// Always `"guest"`; surfaced for the confirmation line.
    #[serde(default)]
    role: String,
    /// The room the guest joined (the host's user id).
    host_user_id: i64,
}

/// `crates-cli auth guest <code> [--name <display>]` — redeem the code and
/// persist the resulting guest token.
pub async fn run_guest(
    config: &Config,
    gw: &GatewayConfig,
    code: &str,
    display_name: Option<&str>,
) -> Result<()> {
    let http = http_client(gw)?;

    // The form the gateway expects: the code, the redeeming client, and an
    // optional friendly name shown in the host's "who's here".
    let mut form: Vec<(&str, &str)> = vec![("code", code), ("client_id", CLIENT_ID)];
    if let Some(name) = display_name {
        form.push(("display_name", name));
    }

    let resp = http
        .post(endpoint(gw, "/oauth/guest"))
        .form(&form)
        .send()
        .await
        .context("redeeming the guest code")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("guest code rejected ({status}): {body}");
    }

    let gr: GuestResponse =
        serde_json::from_str(&body).context("parsing guest token response")?;

    let path = token_store_path(config);
    let tokens = StoredTokens::from_guest(CLIENT_ID, gr.access_token, gr.expires_in);
    store::save(&path, &tokens)?;

    let role = if gr.role.is_empty() { "guest" } else { &gr.role };
    println!("Joined room {} as {role}.", gr.host_user_id);
    println!("Session valid for ~{}m (no refresh — redeem the code again when it lapses).", gr.expires_in / 60);
    println!("Tokens saved to {}", path.display());
    Ok(())
}
