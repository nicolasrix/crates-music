//! Config is plain TOML on disk: `[server]` block with url/username/password.

use music_cli::config::{Config, ServerConfig};

#[test]
fn config_roundtrips_toml() {
    let original = Config {
        server: ServerConfig {
            url: "https://nav.example.com".into(),
            username: "alice".into(),
            password: "sesame".into(),
        },
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
