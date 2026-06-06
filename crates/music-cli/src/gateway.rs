//! Shared gateway HTTP plumbing: config lookup, a TLS-configured reqwest
//! client, and the ws/wss URL helper. Extracted from `sync.rs` so every
//! gateway-backed command (`sync`, ratings, stations, recommend) reuses one
//! bearer-auth + mkcert-CA path instead of duplicating it.

use anyhow::{Context, Result, bail};
use url::Url;

use crate::config::{Config, GatewayConfig};

/// Borrow the `[gateway]` config block, or fail with a clear message. All
/// gateway-backed commands require it (the direct-Subsonic mode has no
/// gateway to talk to).
pub fn require_gateway(config: &Config) -> Result<&GatewayConfig> {
    config
        .gateway
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("this command requires a [gateway] block in the CLI config"))
}

/// `{base}{path}` with exactly one slash between — `path` must start with
/// `/`. Used to build `/v1/...` endpoint URLs.
pub fn endpoint(gw: &GatewayConfig, path: &str) -> String {
    debug_assert!(path.starts_with('/'), "endpoint path must start with '/'");
    format!("{}{path}", gw.url.trim_end_matches('/'))
}

/// Build the HTTPS client used for the gateway REST calls.
///
/// Verification is **on** by default (system trust store). The gateway's
/// mkcert-issued `gateway.local` cert won't be in every machine's store,
/// so `[gateway].ca_cert_path` can point at the mkcert root CA — it's
/// added as an extra trust anchor, which keeps a real MITM cert (signed
/// by neither the system roots nor that CA) rejected. `insecure_tls` is a
/// loud, opt-in escape hatch that restores the old accept-anything
/// behaviour for throwaway setups.
pub fn http_client(gw: &GatewayConfig) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder();

    if let Some(ca_path) = &gw.ca_cert_path {
        let pem = std::fs::read(ca_path)
            .with_context(|| format!("reading gateway CA cert at {}", ca_path.display()))?;
        // A PEM file may bundle a chain; trust every cert it contains.
        let certs = reqwest::Certificate::from_pem_bundle(&pem)
            .with_context(|| format!("parsing CA cert(s) at {}", ca_path.display()))?;
        for cert in certs {
            builder = builder.add_root_certificate(cert);
        }
    }

    if gw.insecure_tls {
        eprintln!(
            "WARNING: [gateway].insecure_tls is set — TLS certificate \
             verification is DISABLED, so anyone who can intercept the \
             connection can read your bearer token. Set \
             [gateway].ca_cert_path to the mkcert root CA (`mkcert -CAROOT`) \
             instead."
        );
        builder = builder.danger_accept_invalid_certs(true);
    }

    builder.build().context("building gateway HTTP client")
}

/// Convert https://host[:port] → wss://host[:port]<path> (and http→ws).
pub fn ws_url_for(base: &str, path: &str) -> Result<Url> {
    let mut url = Url::parse(base).with_context(|| format!("parsing gateway url {base}"))?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => bail!("unsupported gateway scheme {other:?} (expected http or https)"),
    };
    url.set_scheme(scheme)
        .map_err(|()| anyhow::anyhow!("could not set ws scheme on {base}"))?;
    url.set_path(path);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_url_becomes_wss() {
        let u = ws_url_for("https://gateway.local:8443", "/v1/sync").unwrap();
        assert_eq!(u.scheme(), "wss");
        assert_eq!(u.host_str(), Some("gateway.local"));
        assert_eq!(u.port(), Some(8443));
        assert_eq!(u.path(), "/v1/sync");
    }

    #[test]
    fn http_url_becomes_ws() {
        let u = ws_url_for("http://localhost:4567", "/v1/sync").unwrap();
        assert_eq!(u.scheme(), "ws");
    }

    #[test]
    fn unsupported_scheme_errors() {
        let err = ws_url_for("ftp://nope/", "/v1/sync")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsupported gateway scheme"), "{err}");
    }

    #[test]
    fn endpoint_joins_with_single_slash() {
        let gw = GatewayConfig {
            url: "https://gateway.local:8443/".into(),
            bearer_token: "t".into(),
            ca_cert_path: None,
            insecure_tls: false,
        };
        assert_eq!(
            endpoint(&gw, "/v1/library/ratings"),
            "https://gateway.local:8443/v1/library/ratings"
        );
    }
}
