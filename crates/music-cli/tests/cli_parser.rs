//! CLI argument parsing. Subcommands are flat (no nesting beyond one level).

use clap::Parser;
use music_cli::cli::{AlbumListArg, CacheAction, Cli, Command};

#[test]
fn parses_ping() {
    let cli = Cli::try_parse_from(["music", "ping"]).unwrap();
    assert!(matches!(cli.command, Command::Ping));
}

#[test]
fn parses_albums_with_default_size_and_kind() {
    let cli = Cli::try_parse_from(["music", "albums"]).unwrap();
    let Command::Albums { size, kind } = cli.command else {
        panic!("expected Albums");
    };
    assert_eq!(size, 20);
    assert!(matches!(kind, AlbumListArg::Newest));
}

#[test]
fn parses_albums_with_overrides() {
    let cli = Cli::try_parse_from(["music", "albums", "--size", "50", "--kind", "random"]).unwrap();
    let Command::Albums { size, kind } = cli.command else {
        panic!("expected Albums");
    };
    assert_eq!(size, 50);
    assert!(matches!(kind, AlbumListArg::Random));
}

#[test]
fn parses_album_show() {
    let cli = Cli::try_parse_from(["music", "album", "al-1"]).unwrap();
    let Command::Album { id } = cli.command else {
        panic!("expected Album");
    };
    assert_eq!(id, "al-1");
}

#[test]
fn parses_play_single_track() {
    let cli = Cli::try_parse_from(["music", "play", "t-99"]).unwrap();
    let Command::Play { track_ids, offline } = cli.command else {
        panic!("expected Play");
    };
    assert_eq!(track_ids, vec!["t-99".to_string()]);
    assert!(!offline, "default is online");
}

#[test]
fn parses_play_multiple_tracks_for_gapless() {
    let cli = Cli::try_parse_from(["music", "play", "t-1", "t-2", "t-3"]).unwrap();
    let Command::Play { track_ids, .. } = cli.command else {
        panic!("expected Play");
    };
    assert_eq!(track_ids, vec!["t-1", "t-2", "t-3"]);
}

#[test]
fn parses_play_offline_flag() {
    let cli = Cli::try_parse_from(["music", "play", "t-99", "--offline"]).unwrap();
    let Command::Play { track_ids, offline } = cli.command else {
        panic!("expected Play");
    };
    assert_eq!(track_ids, vec!["t-99".to_string()]);
    assert!(offline);
}

#[test]
fn rejects_play_without_any_track_id() {
    let result = Cli::try_parse_from(["music", "play"]);
    assert!(result.is_err(), "play requires at least one track id");
}

#[test]
fn parses_pin() {
    let cli = Cli::try_parse_from(["music", "pin", "t-1"]).unwrap();
    let Command::Pin { track_id } = cli.command else {
        panic!("expected Pin");
    };
    assert_eq!(track_id, "t-1");
}

#[test]
fn parses_unpin() {
    let cli = Cli::try_parse_from(["music", "unpin", "t-1"]).unwrap();
    let Command::Unpin { track_id } = cli.command else {
        panic!("expected Unpin");
    };
    assert_eq!(track_id, "t-1");
}

#[test]
fn parses_pinned() {
    let cli = Cli::try_parse_from(["music", "pinned"]).unwrap();
    assert!(matches!(cli.command, Command::Pinned));
}

#[test]
fn parses_cache_stats() {
    let cli = Cli::try_parse_from(["music", "cache", "stats"]).unwrap();
    let Command::Cache { action } = cli.command else {
        panic!("expected Cache");
    };
    assert!(matches!(action, CacheAction::Stats));
}

#[test]
fn parses_cache_evict() {
    let cli = Cli::try_parse_from(["music", "cache", "evict"]).unwrap();
    let Command::Cache { action } = cli.command else {
        panic!("expected Cache");
    };
    assert!(matches!(action, CacheAction::Evict));
}

#[test]
fn rejects_no_subcommand() {
    let result = Cli::try_parse_from(["music"]);
    assert!(result.is_err());
}

#[test]
fn config_flag_is_optional_and_global() {
    let cli = Cli::try_parse_from(["music", "--config", "/tmp/x.toml", "ping"]).unwrap();
    assert_eq!(
        cli.config.as_deref(),
        Some(std::path::Path::new("/tmp/x.toml"))
    );
}
