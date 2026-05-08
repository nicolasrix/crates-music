//! Gateway binary. Loads config, wires the router, terminates TLS via
//! axum-server + rustls.
//!
//! TLS is wired here, not in `app.rs`, so unit/integration tests can exercise
//! the router over plain HTTP via `tower::ServiceExt::oneshot`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use music_cache::Cache;
use music_gateway::oauth::{NewClient, OauthStore, SetupToken};
use music_gateway::{AppState, Config, build_router};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "music-gateway", about = "Music gateway for Navidrome")]
struct Args {
    /// Path to the gateway config TOML.
    #[arg(short, long, env = "MUSIC_GATEWAY_CONFIG")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install rustls ring crypto provider");

    let args = Args::parse();
    let config = Config::load(&args.config)
        .with_context(|| format!("loading config from {}", args.config.display()))?;

    let listen = config.server.listen;
    let tls = RustlsConfig::from_pem_file(&config.server.tls_cert, &config.server.tls_key)
        .await
        .context("loading TLS cert + key (mkcert PEM expected)")?;

    let cache = Cache::open(&config.cache.path)
        .await
        .with_context(|| format!("opening cache at {}", config.cache.path.display()))?;

    let oauth = OauthStore::open(&config.oauth.state_db)
        .await
        .with_context(|| {
            format!(
                "opening oauth state DB at {}",
                config.oauth.state_db.display()
            )
        })?;

    // Register pre-declared OAuth clients (idempotent: skip when the
    // client_id is already in the DB).
    for client in &config.oauth.clients {
        if oauth.find_client(&client.client_id).await?.is_none() {
            oauth
                .register_client(NewClient {
                    client_id: client.client_id.clone(),
                    name: client.name.clone(),
                    redirect_uris: client.redirect_uris.clone(),
                })
                .await
                .with_context(|| {
                    format!("registering pre-declared oauth client {}", client.client_id)
                })?;
            tracing::info!(client = %client.client_id, "registered oauth client");
        }
    }

    let setup_token = if oauth.master_password_hash().await?.is_none() {
        let token = SetupToken::generate();
        if let Some(value) = token.value() {
            tracing::warn!(
                "gateway is unconfigured — visit https://{}/oauth/setup with token: {}",
                listen,
                value
            );
        }
        token
    } else {
        SetupToken::none()
    };

    let state = AppState::new(config, cache, oauth, setup_token);
    let router = build_router(state);

    tracing::info!(%listen, "music-gateway listening");
    axum_server::bind_rustls(listen, tls)
        .serve(router.into_make_service())
        .await
        .context("axum-server")?;

    Ok(())
}
