//! TOML round-trip for the gateway config.

use music_gateway::Config;

#[test]
fn config_parses_a_realistic_toml() {
    let toml = r#"
[server]
listen = "0.0.0.0:8443"
tls_cert = "/etc/gateway/cert.pem"
tls_key = "/etc/gateway/key.pem"
bearer_token = "super-secret"

[upstream]
navidrome_url = "http://nav.lan:4533"
username = "alice"
password = "wonderland"

[cache]
path = "/var/lib/gateway/cache.sqlite"
browse_ttl_seconds = 3600
"#;
    let cfg = Config::from_toml_str(toml).unwrap();
    assert_eq!(cfg.server.listen.port(), 8443);
    assert_eq!(cfg.server.bearer_token, "super-secret");
    assert_eq!(cfg.upstream.navidrome_url, "http://nav.lan:4533");
    assert_eq!(cfg.cache.browse_ttl_seconds, 3600);
    // `[recommend]` is optional; absent section ⇒ default 512.
    assert_eq!(cfg.recommend.embedding_dim, 512);
    // `[discovery]` likewise — absent means on, which is the shape every
    // pre-discovery deployment's config file has.
    assert!(cfg.discovery.enabled);
    assert_eq!(cfg.discovery.interval_seconds, 300);
}

#[test]
fn config_honours_discovery_overrides() {
    let toml = r#"
[server]
listen = "0.0.0.0:8443"
tls_cert = "/etc/gateway/cert.pem"
tls_key = "/etc/gateway/key.pem"
bearer_token = "x"

[upstream]
navidrome_url = "http://nav.lan:4533"
username = "alice"
password = "wonderland"

[cache]
path = "/var/lib/gateway/cache.sqlite"
browse_ttl_seconds = 3600

[discovery]
enabled = false
interval_seconds = 900
"#;
    let cfg = Config::from_toml_str(toml).unwrap();
    assert!(!cfg.discovery.enabled);
    assert_eq!(cfg.discovery.interval_seconds, 900);
    // Unspecified fields still fall back to their defaults rather than
    // zeroing out.
    assert_eq!(cfg.discovery.recent_albums, 50);
}

#[test]
fn config_rejects_truncated_toml() {
    let toml = "[server]\nlisten = \"0.0.0.0:8443\"";
    assert!(Config::from_toml_str(toml).is_err());
}

#[test]
fn config_honours_recommend_embedding_dim_override() {
    let toml = r#"
[server]
listen = "0.0.0.0:8443"
tls_cert = "/etc/gateway/cert.pem"
tls_key = "/etc/gateway/key.pem"
bearer_token = "x"

[upstream]
navidrome_url = "http://nav.lan:4533"
username = "alice"
password = "wonderland"

[cache]
path = "/var/lib/gateway/cache.sqlite"
browse_ttl_seconds = 3600

[recommend]
embedding_dim = 768
"#;
    let cfg = Config::from_toml_str(toml).unwrap();
    assert_eq!(cfg.recommend.embedding_dim, 768);
}
