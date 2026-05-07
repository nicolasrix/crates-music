use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use music_subsonic::AlbumListType;

#[derive(Parser, Debug)]
#[command(name = "music", version, about = "Subsonic / Navidrome client")]
pub struct Cli {
    /// Path to config file (default: platform XDG config dir).
    #[arg(long, global = true, env = "MUSIC_CONFIG")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Ping the configured Subsonic server.
    Ping,

    /// List albums.
    Albums {
        /// Number of albums to return.
        #[arg(long, default_value_t = 20)]
        size: u32,

        /// Sort order / list kind.
        #[arg(long, value_enum, default_value_t = AlbumListArg::Newest)]
        kind: AlbumListArg,
    },

    /// Show an album with its tracks.
    Album {
        /// Album ID (e.g. from `albums`).
        id: String,
    },

    /// Stream and play a single track.
    Play {
        /// Track ID (e.g. from `album <id>`).
        track_id: String,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlbumListArg {
    Newest,
    Recent,
    Frequent,
    Random,
    AlphabeticalByName,
    AlphabeticalByArtist,
}

impl From<AlbumListArg> for AlbumListType {
    fn from(value: AlbumListArg) -> Self {
        match value {
            AlbumListArg::Newest => Self::Newest,
            AlbumListArg::Recent => Self::Recent,
            AlbumListArg::Frequent => Self::Frequent,
            AlbumListArg::Random => Self::Random,
            AlbumListArg::AlphabeticalByName => Self::AlphabeticalByName,
            AlbumListArg::AlphabeticalByArtist => Self::AlphabeticalByArtist,
        }
    }
}
