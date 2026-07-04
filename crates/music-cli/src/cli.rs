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

    /// List all artists (alphabetical).
    Artists,

    /// Show an artist with their albums.
    Artist {
        /// Artist ID (e.g. from `artists` or `search`).
        id: String,
    },

    /// List tracks from the library (paginated, server order).
    Tracks {
        /// Number of tracks to return.
        #[arg(long, default_value_t = 50)]
        size: u32,

        /// Skip this many tracks (for paging).
        #[arg(long, default_value_t = 0)]
        offset: u32,
    },

    /// Search artists, albums and tracks by free text.
    Search {
        /// Query string.
        query: String,

        /// Max results per category (artists / albums / tracks).
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },

    /// Stream and play one or more tracks. Multiple IDs play gaplessly.
    Play {
        /// Track IDs (in playback order).
        #[arg(required = true)]
        track_ids: Vec<String>,

        /// Don't reach the network. Plays only if every track is in the local
        /// audio cache; errors otherwise.
        #[arg(long)]
        offline: bool,
    },

    /// Like a track (or album/artist via `--kind`). Boosts it in
    /// recommendations. Gateway-owned; no Navidrome writeback.
    Like {
        /// Entity ID.
        id: String,
        #[arg(long, value_enum, default_value_t = RatingKind::Track)]
        kind: RatingKind,
    },

    /// Dislike a track (or album/artist via `--kind`). Excludes it from
    /// play and auto-skips it. Gateway-owned; no Navidrome writeback.
    Dislike {
        /// Entity ID.
        id: String,
        #[arg(long, value_enum, default_value_t = RatingKind::Track)]
        kind: RatingKind,
    },

    /// Clear a like/dislike, returning the entity to neutral.
    Unrate {
        /// Entity ID.
        id: String,
        #[arg(long, value_enum, default_value_t = RatingKind::Track)]
        kind: RatingKind,
    },

    /// List your liked (and disliked) tracks, albums and artists.
    Liked,

    /// Build a station from a natural-language prompt, e.g.
    /// "rainy sunday afternoon". Prints ranked tracks. Requires `[gateway]`
    /// config and a ready recommender.
    Station {
        /// Free-text prompt.
        prompt: String,
        /// Number of tracks to return.
        #[arg(short = 'n', long, default_value_t = 20)]
        n: usize,
    },

    /// Content-based recommendations from the gateway. Requires `[gateway]`
    /// config and a ready recommender.
    Recommend {
        #[command(subcommand)]
        action: RecommendAction,
    },

    /// Pin a track to the cache so it's never LRU-evicted. Fetches first if
    /// not yet cached.
    Pin { track_id: String },

    /// Unpin a track. The entry returns to the regular budget and may be
    /// LRU-evicted if it pushes regular bytes over the limit.
    Unpin { track_id: String },

    /// List all currently-pinned tracks.
    Pinned,

    /// Inspect or manage the audio cache.
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },

    /// Interact with the gateway sync state (queue + playback) shared
    /// across devices. Requires `[gateway]` config.
    Sync {
        #[command(subcommand)]
        action: SyncAction,
    },

    /// Authenticate with the gateway via the OAuth 2.1 Device Authorization
    /// Grant (RFC 8628). Requires `[gateway]` config.
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },

    /// Interactive full-screen mode (hidden: the supported entry point is
    /// running the binary with no subcommand on a TTY).
    #[command(hide = true)]
    Tui,
}

#[derive(Subcommand, Debug)]
pub enum AuthAction {
    /// Start the device flow: prints a code + URL, then waits for you to
    /// approve it in a logged-in browser. Saves the resulting tokens.
    Login,
    /// Revoke the stored refresh token and delete the local token store.
    Logout,
    /// Show whether the CLI is authenticated and when the access token
    /// expires.
    Status,
}

#[derive(Subcommand, Debug)]
pub enum RecommendAction {
    /// Tracks acoustically similar to a seed track.
    Next {
        /// Seed track ID.
        seed: String,
        /// Number of tracks to return.
        #[arg(short = 'n', long, default_value_t = 20)]
        n: usize,
    },
}

#[derive(Subcommand, Debug)]
pub enum SyncAction {
    /// Print the current snapshot as JSON.
    State,
    /// Show the synced queue as a readable table: position, now-playing
    /// marker, item id (for `remove`/`move`), and resolved track title.
    Queue,
    /// Append one or more tracks to the synced queue.
    Push {
        /// Track IDs to push (in order).
        #[arg(required = true)]
        track_ids: Vec<String>,
    },
    /// Remove a queue item by its item id (see `sync queue`). No-op if
    /// the id isn't in the queue.
    Remove {
        /// Queue item id (the ITEM column of `sync queue`).
        item_id: String,
    },
    /// Move a queue item to a new position. `new_index` is the 0-based
    /// slot in the queue after the item is lifted out; it's clamped to
    /// the valid range. The now-playing cursor follows the track.
    #[command(alias = "reorder")]
    Move {
        /// Queue item id to move (the ITEM column of `sync queue`).
        item_id: String,
        /// Target 0-based position.
        new_index: usize,
    },
    /// Set the now-playing cursor to a queue position (0-based). This is
    /// shared playback state — it changes what plays on every device.
    Jump {
        /// 0-based queue position to make current.
        index: usize,
    },
    /// Empty the whole queue and reset shared playback: clears the
    /// now-playing cursor and position, stops playback, and drops the
    /// session anchor. Affects every device.
    Clear,
    /// Open the WebSocket and stream every server message to stdout
    /// as JSON, one frame per line. Run with Ctrl-C to exit.
    Watch,
}

#[derive(Subcommand, Debug)]
pub enum CacheAction {
    /// Print cache usage: regular and pinned bytes against their budgets.
    Stats,
    /// Force-evict regular entries until total bytes fit the budget.
    Evict,
}

/// Which library entity a rating applies to. Maps to the gateway's
/// lowercase `kind` wire value.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RatingKind {
    Track,
    Album,
    Artist,
}

impl RatingKind {
    pub fn wire(self) -> &'static str {
        match self {
            RatingKind::Track => "track",
            RatingKind::Album => "album",
            RatingKind::Artist => "artist",
        }
    }
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
