//! Plain-text table formatting for the CLI. No colour, no extra deps —
//! the goal is "useful in a pipe" first.

use std::fmt::Write;

use music_core::{Album, Artist, Track};

/// One-line album header: `Name — Artist (Year)`, omitting absent parts.
pub fn album_header(album: &Album) -> String {
    let artist = album
        .artist_name
        .as_deref()
        .map(|a| format!(" — {a}"))
        .unwrap_or_default();
    let year = album.year.map(|y| format!(" ({y})")).unwrap_or_default();
    format!("{}{artist}{year}", album.name)
}

/// One-line artist header: `Name (N albums)`, omitting the count if unknown.
pub fn artist_header(artist: &Artist) -> String {
    let albums = artist
        .album_count
        .map(|n| format!(" ({n} albums)"))
        .unwrap_or_default();
    format!("{}{albums}", artist.name)
}

pub fn artists_table(artists: &[Artist]) -> String {
    if artists.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let id_w = artists
        .iter()
        .map(|a| a.id.as_str().len())
        .max()
        .unwrap_or(2)
        .max(2);
    let name_w = artists
        .iter()
        .map(|a| a.name.len())
        .max()
        .unwrap_or(4)
        .max(4);

    let _ = writeln!(
        out,
        "{:<id_w$}  {:<name_w$}  {:>6}",
        "ID",
        "NAME",
        "ALBUMS",
        id_w = id_w,
        name_w = name_w,
    );
    for a in artists {
        let albums = a.album_count.map(|n| n.to_string()).unwrap_or_default();
        let _ = writeln!(
            out,
            "{:<id_w$}  {:<name_w$}  {:>6}",
            a.id.as_str(),
            a.name,
            albums,
            id_w = id_w,
            name_w = name_w,
        );
    }
    out
}

pub fn albums_table(albums: &[Album]) -> String {
    if albums.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let id_w = albums
        .iter()
        .map(|a| a.id.as_str().len())
        .max()
        .unwrap_or(2)
        .max(2);
    let name_w = albums
        .iter()
        .map(|a| a.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let artist_w = albums
        .iter()
        .map(|a| a.artist_name.as_deref().unwrap_or("").len())
        .max()
        .unwrap_or(6)
        .max(6);

    let _ = writeln!(
        out,
        "{:<id_w$}  {:<name_w$}  {:<artist_w$}  {:>4}  {:>5}",
        "ID",
        "NAME",
        "ARTIST",
        "YEAR",
        "TRACKS",
        id_w = id_w,
        name_w = name_w,
        artist_w = artist_w,
    );
    for a in albums {
        let year = a.year.map(|y| y.to_string()).unwrap_or_default();
        let artist = a.artist_name.as_deref().unwrap_or("");
        let _ = writeln!(
            out,
            "{:<id_w$}  {:<name_w$}  {:<artist_w$}  {:>4}  {:>5}",
            a.id.as_str(),
            a.name,
            artist,
            year,
            a.song_count,
            id_w = id_w,
            name_w = name_w,
            artist_w = artist_w,
        );
    }
    out
}

pub fn tracks_table(tracks: &[Track]) -> String {
    if tracks.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let id_w = tracks
        .iter()
        .map(|t| t.id.as_str().len())
        .max()
        .unwrap_or(2)
        .max(2);
    let title_w = tracks
        .iter()
        .map(|t| t.title.len())
        .max()
        .unwrap_or(5)
        .max(5);

    let _ = writeln!(
        out,
        "{:>3}  {:<id_w$}  {:<title_w$}  {:>5}",
        "#",
        "ID",
        "TITLE",
        "MM:SS",
        id_w = id_w,
        title_w = title_w,
    );
    for t in tracks {
        let track_no = t.track_number.map(|n| n.to_string()).unwrap_or_default();
        let mmss = t.duration_seconds.map(format_mmss).unwrap_or_default();
        let _ = writeln!(
            out,
            "{:>3}  {:<id_w$}  {:<title_w$}  {:>5}",
            track_no,
            t.id.as_str(),
            t.title,
            mmss,
            id_w = id_w,
            title_w = title_w,
        );
    }
    out
}

fn format_mmss(seconds: u32) -> String {
    let m = seconds / 60;
    let s = seconds % 60;
    format!("{m}:{s:02}")
}
