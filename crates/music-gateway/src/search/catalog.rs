//! Pull the full catalog from Navidrome and flatten it into the search
//! index's [`Record`] list. Runs on the boot/refresh task, not the request
//! path — a handful of paged Subsonic calls, then an in-memory build.
//!
//! Uses `music_subsonic::Client` directly (like ingest) rather than the
//! gateway's own `/rest` proxy, so it neither reads nor pollutes the L2
//! browse cache.

use anyhow::{Context, Result};
use music_subsonic::{AlbumListType, Client as SubsonicClient, Credentials};

use super::index::{Kind, Record};
use crate::config::UpstreamConfig;

/// Page size for the paged endpoints (albums, tracks). Navidrome handles
/// large pages fine; bigger pages mean fewer round-trips on the boot fetch.
const PAGE: u32 = 500;
/// Safety cap on pages so a misbehaving upstream can't loop forever.
/// 400 × 500 = 200k entities, far above any household library.
const MAX_PAGES: u32 = 400;

/// Fetch artists + albums + tracks and assemble the flat record list the
/// index builds from.
pub(super) async fn fetch_records(upstream: &UpstreamConfig) -> Result<Vec<Record>> {
    let creds = Credentials {
        username: upstream.username.clone(),
        password: upstream.password.clone(),
    };
    let client = SubsonicClient::new(&upstream.navidrome_url, creds)
        .context("constructing Subsonic client for search catalog")?;

    let mut records = Vec::new();
    fetch_artists(&client, &mut records).await?;
    fetch_albums(&client, &mut records).await?;
    fetch_tracks(&client, &mut records).await?;
    Ok(records)
}

async fn fetch_artists(client: &SubsonicClient, out: &mut Vec<Record>) -> Result<()> {
    let artists = client.get_artists().await.context("search catalog: get_artists")?;
    out.extend(artists.into_iter().map(|a| Record {
        kind: Kind::Artist,
        id: a.id.as_str().to_string(),
        name: a.name,
        artist: None,
        artist_id: None,
        album: None,
        album_id: None,
        cover_art: None,
        duration_seconds: None,
    }));
    Ok(())
}

async fn fetch_albums(client: &SubsonicClient, out: &mut Vec<Record>) -> Result<()> {
    for page in 0..MAX_PAGES {
        let batch = client
            .get_album_list2(AlbumListType::AlphabeticalByName, Some(PAGE), Some(page * PAGE))
            .await
            .context("search catalog: get_album_list2")?;
        let is_last = batch.len() < PAGE as usize;
        out.extend(batch.into_iter().map(|al| Record {
            kind: Kind::Album,
            id: al.id.as_str().to_string(),
            name: al.name,
            artist: al.artist_name,
            artist_id: al.artist_id.map(|id| id.as_str().to_string()),
            album: None,
            album_id: None,
            cover_art: al.cover_art_id,
            duration_seconds: None,
        }));
        if is_last {
            break;
        }
    }
    Ok(())
}

async fn fetch_tracks(client: &SubsonicClient, out: &mut Vec<Record>) -> Result<()> {
    // Empty-query search3 pages the whole song list (same technique the web
    // client uses). `search3` applies the offset to all three buckets; we
    // only read `tracks` and page on its length.
    for page in 0..MAX_PAGES {
        let result = client
            .search3("", PAGE, page * PAGE)
            .await
            .context("search catalog: search3 (tracks)")?;
        let is_last = result.tracks.len() < PAGE as usize;
        out.extend(result.tracks.into_iter().map(|t| Record {
            kind: Kind::Track,
            id: t.id.as_str().to_string(),
            name: t.title,
            artist: t.artist_name,
            artist_id: t.artist_id.map(|id| id.as_str().to_string()),
            album: t.album_name,
            album_id: t.album_id.map(|id| id.as_str().to_string()),
            cover_art: None,
            duration_seconds: t.duration_seconds,
        }));
        if is_last {
            break;
        }
    }
    Ok(())
}
