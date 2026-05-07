//! CLI argument parsing. Subcommands are flat (no nesting beyond one level).

use clap::Parser;
use music_cli::cli::{AlbumListArg, Cli, Command};

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
fn parses_play() {
    let cli = Cli::try_parse_from(["music", "play", "t-99"]).unwrap();
    let Command::Play { track_id } = cli.command else {
        panic!("expected Play");
    };
    assert_eq!(track_id, "t-99");
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
