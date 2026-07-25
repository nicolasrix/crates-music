//! Config is plain TOML on disk: `[server]` block with url/username/password,
//! plus an optional `[gateway]` block to route through music-gateway.

use std::path::PathBuf;

use music_cli::config::{
    CacheConfig, Config, GatewayConfig, PlaybackConfig, Quality, ServerConfig, TuiConfig,
};

#[test]
fn config_roundtrips_toml() {
    let original = Config {
        server: ServerConfig {
            url: "https://nav.example.com".into(),
            username: "alice".into(),
            password: "sesame".into(),
        },
        gateway: None,
        cache: CacheConfig::default(),
        playback: PlaybackConfig::default(),
        tui: TuiConfig::default(),
        source_path: PathBuf::new(),
    };
    let serialized = toml::to_string(&original).unwrap();
    let back: Config = toml::from_str(&serialized).unwrap();
    assert_eq!(back, original);
}

#[test]
fn config_save_round_trips_through_disk() {
    // The Settings view persists edits with `Config::save`; loading the file
    // back must reproduce them (and the atomic temp file must be gone).
    let dir = std::env::temp_dir().join(format!("crates-cfg-save-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    let mut config = Config {
        server: ServerConfig {
            url: "http://nav".into(),
            username: "alice".into(),
            password: "sesame".into(),
        },
        gateway: None,
        cache: CacheConfig::default(),
        playback: PlaybackConfig::default(),
        tui: TuiConfig::default(),
        source_path: path.clone(),
    };
    config.playback.download_quality = Quality::Opus128;
    config.cache.regular_budget_bytes = 42;
    config.save().unwrap();

    let loaded = Config::load(&path).unwrap();
    assert_eq!(loaded.playback.download_quality, Quality::Opus128);
    assert_eq!(loaded.cache.regular_budget_bytes, 42);
    assert!(!path.with_extension("toml.tmp").exists());
    std::fs::remove_dir_all(&dir).ok();
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
fn config_with_gateway_block_parses_gateway_url() {
    // A legacy `bearer_token` key is now unused; it must still parse
    // (serde ignores the unknown field) so old configs don't break — the
    // user just needs to run `crates-cli auth login`.
    let raw = r#"
        [server]
        url = "https://nav.example.com"
        username = "alice"
        password = "sesame"

        [gateway]
        url = "https://gateway.local:8443"
        bearer_token = "legacy-ignored"
    "#;
    let config: Config = toml::from_str(raw).unwrap();
    let gw = config.gateway.expect("gateway block parsed");
    assert_eq!(gw.url, "https://gateway.local:8443");
}

#[test]
fn config_cache_block_parses_explicit_path_and_budgets() {
    let raw = r#"
        [server]
        url = "https://nav.example.com"
        username = "alice"
        password = "sesame"

        [cache]
        path = "/var/cache/crates-music/audio"
        regular_budget_bytes = 21474836480
        pinned_budget_bytes = 5368709120
    "#;
    let config: Config = toml::from_str(raw).unwrap();
    assert_eq!(
        config.cache.path,
        Some(std::path::PathBuf::from("/var/cache/crates-music/audio"))
    );
    assert_eq!(config.cache.regular_budget_bytes, 20 * 1024 * 1024 * 1024);
    assert_eq!(config.cache.pinned_budget_bytes, 5 * 1024 * 1024 * 1024);
}

#[test]
fn config_without_cache_block_uses_defaults() {
    let raw = r#"
        [server]
        url = "https://nav.example.com"
        username = "alice"
        password = "sesame"
    "#;
    let config: Config = toml::from_str(raw).unwrap();
    assert!(config.cache.path.is_none());
    assert_eq!(config.cache.regular_budget_bytes, 10 * 1024 * 1024 * 1024);
    assert_eq!(config.cache.pinned_budget_bytes, 5 * 1024 * 1024 * 1024);
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
            ca_cert_path: None,
            insecure_tls: false,
        }),
        cache: CacheConfig::default(),
        playback: PlaybackConfig::default(),
        tui: TuiConfig::default(),
        source_path: PathBuf::new(),
    };
    let serialized = toml::to_string(&original).unwrap();
    let back: Config = toml::from_str(&serialized).unwrap();
    assert_eq!(back, original);
}
