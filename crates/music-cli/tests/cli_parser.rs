//! CLI argument parsing. Subcommands are flat (no nesting beyond one level).

use clap::Parser;
use music_cli::cli::{AlbumListArg, CacheAction, Cli, Command};

#[test]
fn parses_ping() {
    let cli = Cli::try_parse_from(["crates-cli", "ping"]).unwrap();
    assert!(matches!(cli.command, Some(Command::Ping)));
}

#[test]
fn parses_albums_with_default_size_and_kind() {
    let cli = Cli::try_parse_from(["crates-cli", "albums"]).unwrap();
    let Some(Command::Albums { size, kind }) = cli.command else {
        panic!("expected Albums");
    };
    assert_eq!(size, 20);
    assert!(matches!(kind, AlbumListArg::Newest));
}

#[test]
fn parses_albums_with_overrides() {
    let cli = Cli::try_parse_from(["crates-cli", "albums", "--size", "50", "--kind", "random"]).unwrap();
    let Some(Command::Albums { size, kind }) = cli.command else {
        panic!("expected Albums");
    };
    assert_eq!(size, 50);
    assert!(matches!(kind, AlbumListArg::Random));
}

#[test]
fn parses_album_show() {
    let cli = Cli::try_parse_from(["crates-cli", "album", "al-1"]).unwrap();
    let Some(Command::Album { id }) = cli.command else {
        panic!("expected Album");
    };
    assert_eq!(id, "al-1");
}

#[test]
fn parses_play_single_track() {
    let cli = Cli::try_parse_from(["crates-cli", "play", "t-99"]).unwrap();
    let Some(Command::Play { track_ids, offline }) = cli.command else {
        panic!("expected Play");
    };
    assert_eq!(track_ids, vec!["t-99".to_string()]);
    assert!(!offline, "default is online");
}

#[test]
fn parses_play_multiple_tracks_for_gapless() {
    let cli = Cli::try_parse_from(["crates-cli", "play", "t-1", "t-2", "t-3"]).unwrap();
    let Some(Command::Play { track_ids, .. }) = cli.command else {
        panic!("expected Play");
    };
    assert_eq!(track_ids, vec!["t-1", "t-2", "t-3"]);
}

#[test]
fn parses_play_offline_flag() {
    let cli = Cli::try_parse_from(["crates-cli", "play", "t-99", "--offline"]).unwrap();
    let Some(Command::Play { track_ids, offline }) = cli.command else {
        panic!("expected Play");
    };
    assert_eq!(track_ids, vec!["t-99".to_string()]);
    assert!(offline);
}

#[test]
fn rejects_play_without_any_track_id() {
    let result = Cli::try_parse_from(["crates-cli", "play"]);
    assert!(result.is_err(), "play requires at least one track id");
}

#[test]
fn parses_pin() {
    let cli = Cli::try_parse_from(["crates-cli", "pin", "t-1"]).unwrap();
    let Some(Command::Pin { track_id }) = cli.command else {
        panic!("expected Pin");
    };
    assert_eq!(track_id, "t-1");
}

#[test]
fn parses_unpin() {
    let cli = Cli::try_parse_from(["crates-cli", "unpin", "t-1"]).unwrap();
    let Some(Command::Unpin { track_id }) = cli.command else {
        panic!("expected Unpin");
    };
    assert_eq!(track_id, "t-1");
}

#[test]
fn parses_pinned() {
    let cli = Cli::try_parse_from(["crates-cli", "pinned"]).unwrap();
    assert!(matches!(cli.command, Some(Command::Pinned)));
}

#[test]
fn parses_cache_stats() {
    let cli = Cli::try_parse_from(["crates-cli", "cache", "stats"]).unwrap();
    let Some(Command::Cache { action }) = cli.command else {
        panic!("expected Cache");
    };
    assert!(matches!(action, CacheAction::Stats));
}

#[test]
fn parses_cache_evict() {
    let cli = Cli::try_parse_from(["crates-cli", "cache", "evict"]).unwrap();
    let Some(Command::Cache { action }) = cli.command else {
        panic!("expected Cache");
    };
    assert!(matches!(action, CacheAction::Evict));
}

#[test]
fn config_flag_is_optional_and_global() {
    let cli = Cli::try_parse_from(["crates-cli", "--config", "/tmp/x.toml", "ping"]).unwrap();
    assert_eq!(
        cli.config.as_deref(),
        Some(std::path::Path::new("/tmp/x.toml"))
    );
}

#[test]
fn bare_invocation_parses_to_no_command() {
    let cli = Cli::try_parse_from(["crates-cli"]).unwrap();
    assert!(cli.command.is_none());
}
