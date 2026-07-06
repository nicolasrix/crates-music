//! `playlist list | show | create | rename | delete | add | remove | play` —
//! the gateway-owned playlist surface on the CLI.
//!
//! Playlists live in the gateway (not Navidrome) and store only ordered
//! Navidrome track ids, so `show`/`play` hydrate those ids back to `Track`s
//! through the Subsonic client — the same two-step every recommend command
//! uses. HTTP + parsing live in [`crate::api`] (shared with the TUI); this
//! module only renders. Membership edits replace against the raw stored ids
//! ([`crate::api::PlaylistDetail::track_ids`]), never a hydrated subset, so an
//! id that failed to resolve this load isn't dropped from the playlist.

use anyhow::{Result, anyhow};
use music_core::TrackId;
use music_subsonic::Client;

use crate::api::{self, ApiError, PlaylistSummary};
use crate::config::Config;
use crate::format::tracks_table;

/// Map an `ApiError` to a user-facing classic-command error. A guest 403
/// becomes a plain "not permitted" rather than an HTTP dump (decision D3).
fn friendly(e: ApiError) -> anyhow::Error {
    match e {
        ApiError::Forbidden => {
            anyhow!("not permitted — guests can't modify playlists (sign in as a user)")
        }
        ApiError::RecommenderUnavailable => anyhow!("the gateway recommender is not ready"),
        ApiError::Http(e) => e,
    }
}

/// `GET /v1/playlists` → a table: id, name, track count, visibility (with a
/// `*` marking playlists you don't own — shared ones you can view but not
/// edit).
pub async fn run_list(config: &Config) -> Result<()> {
    let playlists = api::list_playlists(config).await.map_err(friendly)?;
    if playlists.is_empty() {
        println!("(no playlists)");
        return Ok(());
    }
    print!("{}", crate::style::table(&playlists_table(&playlists)));
    Ok(())
}

/// `GET /v1/playlists/:id` → a header line plus the hydrated tracks table.
pub async fn run_show(config: &Config, client: &Client, id: &str) -> Result<()> {
    let detail = api::get_playlist(config, id).await.map_err(friendly)?;
    println!("{}", playlist_header(&detail.summary));

    if detail.track_ids.is_empty() {
        println!("(empty playlist)");
        return Ok(());
    }
    println!();
    let ids: Vec<TrackId> = detail.track_ids.iter().map(|s| TrackId::from(s.clone())).collect();
    let (tracks, failed) = api::resolve_tracks(client, &ids).await;
    print!("{}", tracks_table(&tracks));
    if !failed.is_empty() {
        eprintln!(
            "(could not resolve {} track(s): {})",
            failed.len(),
            failed.join(", ")
        );
    }
    Ok(())
}

pub async fn run_create(config: &Config, name: &str) -> Result<()> {
    let summary = api::create_playlist(config, name).await.map_err(friendly)?;
    println!("created playlist {} ({})", summary.name, summary.id);
    Ok(())
}

pub async fn run_rename(config: &Config, id: &str, name: &str) -> Result<()> {
    api::rename_playlist(config, id, name).await.map_err(friendly)?;
    println!("renamed {id} to {name}");
    Ok(())
}

pub async fn run_delete(config: &Config, id: &str) -> Result<()> {
    api::delete_playlist(config, id).await.map_err(friendly)?;
    println!("deleted playlist {id}");
    Ok(())
}

pub async fn run_add(config: &Config, id: &str, track_ids: &[String]) -> Result<()> {
    api::put_playlist_tracks(config, id, track_ids, true)
        .await
        .map_err(friendly)?;
    println!("added {} track(s) to {id}", track_ids.len());
    Ok(())
}

/// Remove every occurrence of `track_id` from a playlist. Fetches the raw
/// stored ids, filters, and replaces — so an id that fails to hydrate for
/// display is still preserved in the stored membership.
pub async fn run_remove(config: &Config, id: &str, track_id: &str) -> Result<()> {
    let detail = api::get_playlist(config, id).await.map_err(friendly)?;
    let before = detail.track_ids.len();
    let next: Vec<String> = detail
        .track_ids
        .into_iter()
        .filter(|t| t != track_id)
        .collect();
    if next.len() == before {
        println!("track {track_id} is not in playlist {id}");
        return Ok(());
    }
    api::put_playlist_tracks(config, id, &next, false)
        .await
        .map_err(friendly)?;
    println!("removed {track_id} from {id} ({} track(s) left)", next.len());
    Ok(())
}

/// Resolve a playlist to its ordered `TrackId`s for local playback,
/// optionally shuffled. The caller ([`crate::app`]) feeds these to the same
/// gapless `play` path the `play` command uses.
pub async fn resolve_play_ids(config: &Config, id: &str, shuffle: bool) -> Result<Vec<TrackId>> {
    let detail = api::get_playlist(config, id).await.map_err(friendly)?;
    if detail.track_ids.is_empty() {
        return Err(anyhow!("playlist {id} is empty"));
    }
    let mut ids: Vec<TrackId> = detail
        .track_ids
        .into_iter()
        .map(TrackId::from)
        .collect();
    if shuffle {
        use rand::seq::SliceRandom;
        ids.shuffle(&mut rand::thread_rng());
    }
    Ok(ids)
}

/// One-line header: `Name (N tracks) · visibility`.
fn playlist_header(p: &PlaylistSummary) -> String {
    let vis = if p.visibility.is_empty() {
        String::new()
    } else {
        format!(" · {}", p.visibility)
    };
    format!("{} ({} tracks){vis}", p.name, p.song_count)
}

/// A plain aligned table (header line first, so `style::table` can bold it).
/// The leading marker column shows `*` for playlists you don't own (shared
/// ones you can view but not edit).
fn playlists_table(playlists: &[PlaylistSummary]) -> String {
    use std::fmt::Write;

    let id_w = playlists
        .iter()
        .map(|p| p.id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    let name_w = playlists
        .iter()
        .map(|p| p.name.len())
        .max()
        .unwrap_or(4)
        .max(4);

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<1}  {:<id_w$}  {:<name_w$}  {:>6}  {:<7}",
        " ", "ID", "NAME", "TRACKS", "VIS",
        id_w = id_w,
        name_w = name_w,
    );
    for p in playlists {
        let marker = if p.owned { " " } else { "*" };
        let _ = writeln!(
            out,
            "{marker:<1}  {:<id_w$}  {:<name_w$}  {:>6}  {:<7}",
            p.id, p.name, p.song_count, p.visibility,
            id_w = id_w,
            name_w = name_w,
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pl(id: &str, name: &str, owned: bool, vis: &str, n: u32) -> PlaylistSummary {
        PlaylistSummary {
            id: id.to_owned(),
            name: name.to_owned(),
            visibility: vis.to_owned(),
            owned,
            song_count: n,
        }
    }

    #[test]
    fn header_includes_count_and_visibility() {
        assert_eq!(
            playlist_header(&pl("ab", "Roadtrip", true, "shared", 12)),
            "Roadtrip (12 tracks) · shared"
        );
        // Missing visibility (older gateway) drops the suffix cleanly.
        assert_eq!(
            playlist_header(&pl("ab", "Focus", true, "", 3)),
            "Focus (3 tracks)"
        );
    }

    #[test]
    fn table_has_header_first_and_marks_unowned() {
        let t = playlists_table(&[
            pl("ab12", "Roadtrip", true, "private", 12),
            pl("cd34", "Shared Mix", false, "shared", 5),
        ]);
        let mut lines = t.lines();
        // Header's first non-space column is ID (leading marker slot blank).
        assert!(lines.next().unwrap().trim_start().starts_with("ID"));
        let owned_row = lines.next().unwrap();
        let shared_row = lines.next().unwrap();
        assert!(owned_row.contains("Roadtrip"));
        // The leading marker column: blank for owned, `*` for shared.
        assert!(!owned_row.starts_with('*'));
        assert!(shared_row.starts_with('*'));
    }
}
