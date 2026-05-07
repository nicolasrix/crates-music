//! Config is plain TOML on disk: `[server]` block with url/username/password,
//! plus an optional `[gateway]` block to route through music-gateway.

use music_cli::config::{Config, GatewayConfig, ServerConfig};

#[test]
fn config_roundtrips_toml() {
    let original = Config {
        server: ServerConfig {
            url: "https://nav.example.com".into(),
            username: "alice".into(),
            password: "sesame".into(),
        },
        gateway: None,
    };
    let serialized = toml::to_string(&original).unwrap();
    let back: Config = toml::from_str(&serialized).unwrap();
    assert_eq!(back, original);
}

#[test]
fn config_loads_from_realistic_toml() {
    let raw = r#"
        [server]
        url = "https://nav.example.com/"
        username = "alice"
        password = "sesame"
    "#;
    let config: Config = toml::from_str(raw).unwrap();
    assert_eq!(config.server.url, "https://nav.example.com/");
    assert_eq!(config.server.username, "alice");
    assert_eq!(config.server.password, "sesame");
}

#[test]
fn config_rejects_missing_server_section() {
    let raw = r#"username = "alice""#;
    let result: Result<Config, _> = toml::from_str(raw);
    assert!(result.is_err());
}

#[test]
fn config_rejects_missing_password_field() {
    let raw = r#"
        [server]
        url = "https://nav.example.com"
        username = "alice"
    "#;
    let result: Result<Config, _> = toml::from_str(raw);
    assert!(result.is_err());
}

#[test]
fn config_without_gateway_block_has_no_gateway() {
    let raw = r#"
        [server]
        url = "https://nav.example.com"
        username = "alice"
        password = "sesame"
    "#;
    let config: Config = toml::from_str(raw).unwrap();
    assert!(config.gateway.is_none());
}

#[test]
fn config_with_gateway_block_parses_gateway_url_and_bearer() {
    let raw = r#"
        [server]
        url = "https://nav.example.com"
        username = "alice"
        password = "sesame"

        [gateway]
        url = "https://gateway.local:8443"
        bearer_token = "shared-secret-abc"
    "#;
    let config: Config = toml::from_str(raw).unwrap();
    let gw = config.gateway.expect("gateway block parsed");
    assert_eq!(gw.url, "https://gateway.local:8443");
    assert_eq!(gw.bearer_token, "shared-secret-abc");
}

#[test]
fn config_with_gateway_block_roundtrips() {
    let original = Config {
        server: ServerConfig {
            url: "https://nav.example.com".into(),
            username: "alice".into(),
            password: "sesame".into(),
        },
        gateway: Some(GatewayConfig {
            url: "https://gateway.local:8443".into(),
            bearer_token: "abc".into(),
        }),
    };
    let serialized = toml::to_string(&original).unwrap();
    let back: Config = toml::from_str(&serialized).unwrap();
    assert_eq!(back, original);
}
