//! `GET /v1/search` — typo-tolerant catalog search.
//!
//! Serves a Subsonic `search3` envelope (so the web parser is unchanged),
//! but relevance-ordered by the in-process fuzzy index. When the index
//! isn't built yet (boot window) or is disabled, falls back to proxying
//! Navidrome's own `search3` — search always works, just without typo
//! correction until the index is ready.

use std::collections::HashMap;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use music_subsonic::{Client as SubsonicClient, Credentials};
use serde::{Deserialize, Serialize};

use super::index::{Kind, Record, SearchIndex};
use crate::config::UpstreamConfig;
use crate::principal::AuthPrincipal;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchParams {
    /// Accept both `q` and Subsonic's `query` spelling.
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    query: Option<String>,
    /// `artistCount` / `albumCount` / `songCount` on the wire (camelCase,
    /// matching Subsonic `search3` and the web client).
    #[serde(default)]
    artist_count: Option<usize>,
    #[serde(default)]
    album_count: Option<usize>,
    #[serde(default)]
    song_count: Option<usize>,
}

impl SearchParams {
    fn text(&self) -> &str {
        self.q.as_deref().or(self.query.as_deref()).unwrap_or("")
    }
}

const DEFAULT_ARTIST_COUNT: usize = 20;
const DEFAULT_ALBUM_COUNT: usize = 40;
const DEFAULT_SONG_COUNT: usize = 60;

// ── Response DTOs (Subsonic `searchResult3` shape, camelCase) ────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ArtistDto {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cover_art: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AlbumDto {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artist_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cover_art: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SongDto {
    id: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artist_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    album_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cover_art: Option<String>,
    /// Seconds, as `search3` reports it. Subsonic names this `duration`,
    /// and the web track table reads it straight off the song row — omit
    /// it and every result renders "0:00".
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<u32>,
}

#[derive(Serialize, Default)]
struct SearchResult3Dto {
    artist: Vec<ArtistDto>,
    album: Vec<AlbumDto>,
    song: Vec<SongDto>,
}

#[derive(Serialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    subsonic_response: EnvelopeBody,
}

#[derive(Serialize)]
struct EnvelopeBody {
    status: &'static str,
    version: &'static str,
    #[serde(rename = "searchResult3")]
    search_result3: SearchResult3Dto,
}

fn envelope(result: SearchResult3Dto) -> Json<Envelope> {
    Json(Envelope {
        subsonic_response: EnvelopeBody {
            status: "ok",
            version: "1.16.1",
            search_result3: result,
        },
    })
}

pub async fn search(
    State(state): State<AppState>,
    _principal: AuthPrincipal,
    Query(params): Query<SearchParams>,
) -> Response {
    let text = params.text();
    let artist_count = params.artist_count.unwrap_or(DEFAULT_ARTIST_COUNT);
    let album_count = params.album_count.unwrap_or(DEFAULT_ALBUM_COUNT);
    let song_count = params.song_count.unwrap_or(DEFAULT_SONG_COUNT);

    if let Some(index) = state.search_index() {
        let result = ranked(&index, text, artist_count, album_count, song_count);
        return envelope(result).into_response();
    }

    // Index not built yet (boot window) or disabled → proxy Navidrome.
    match fallback(&state.config().upstream, text, artist_count, album_count, song_count).await {
        Ok(result) => envelope(result).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "search fallback to Navidrome failed");
            (StatusCode::BAD_GATEWAY, "search upstream unavailable").into_response()
        }
    }
}

/// Build a relevance-ordered result set from the fuzzy index.
fn ranked(
    index: &SearchIndex,
    text: &str,
    artist_count: usize,
    album_count: usize,
    song_count: usize,
) -> SearchResult3Dto {
    let album_hits = index.query(text, Kind::Album, album_count);
    let song_hits = index.query(text, Kind::Track, song_count);
    let artist_hits = index.query(text, Kind::Artist, artist_count);

    let albums: Vec<AlbumDto> =
        album_hits.iter().filter_map(|h| index.record(h.record_index)).map(album_dto).collect();
    let songs: Vec<SongDto> =
        song_hits.iter().filter_map(|h| index.record(h.record_index)).map(song_dto).collect();

    // Artists: name-matched hits first, then artists *derived* from the
    // album/track hits (mirrors searchRanking.ts — someone whose name
    // didn't match but whose work did, e.g. Duke Ellington for a "take
    // the a train" query). Dedup by id, cap at artist_count.
    let mut artists: Vec<ArtistDto> =
        artist_hits.iter().filter_map(|h| index.record(h.record_index)).map(artist_dto).collect();
    let mut seen: std::collections::HashSet<String> =
        artists.iter().map(|a| a.id.clone()).collect();

    let derived = derive_artists(
        album_hits.iter().filter_map(|h| index.record(h.record_index)),
        song_hits.iter().filter_map(|h| index.record(h.record_index)),
    );
    for d in derived {
        if artists.len() >= artist_count {
            break;
        }
        if seen.insert(d.id.clone()) {
            artists.push(d);
        }
    }

    SearchResult3Dto { artist: artists, album: albums, song: songs }
}

/// Tally artists appearing across the matched albums/tracks, ranked by
/// appearance count. Borrows a cover from the first appearance so the
/// artist card isn't a bare placeholder.
fn derive_artists<'a>(
    albums: impl Iterator<Item = &'a Record>,
    songs: impl Iterator<Item = &'a Record>,
) -> Vec<ArtistDto> {
    struct Acc {
        name: String,
        hits: u32,
        cover_art: Option<String>,
    }
    let mut by_id: HashMap<String, Acc> = HashMap::new();
    let mut bump = |artist_id: &Option<String>, artist_name: &Option<String>, cover: Option<String>| {
        let (Some(id), Some(name)) = (artist_id, artist_name) else { return };
        let acc = by_id.entry(id.clone()).or_insert_with(|| Acc {
            name: name.clone(),
            hits: 0,
            cover_art: None,
        });
        acc.hits += 1;
        if acc.cover_art.is_none() {
            acc.cover_art = cover;
        }
    };
    for a in albums {
        bump(&a.artist_id, &a.artist, a.cover_art.clone());
    }
    for s in songs {
        // A track's cover resolves via its album id (Subsonic cover ids are
        // polymorphic), so lend that as the derived artist's cover.
        bump(&s.artist_id, &s.artist, s.album_id.clone());
    }
    let mut out: Vec<(u32, ArtistDto)> = by_id
        .into_iter()
        .map(|(id, acc)| (acc.hits, ArtistDto { id, name: acc.name, cover_art: acc.cover_art }))
        .collect();
    // Most-appearing first; stable tiebreak by id for determinism.
    out.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    out.into_iter().map(|(_, dto)| dto).collect()
}

fn artist_dto(rec: &Record) -> ArtistDto {
    ArtistDto { id: rec.id.clone(), name: rec.name.clone(), cover_art: rec.cover_art.clone() }
}

fn album_dto(rec: &Record) -> AlbumDto {
    AlbumDto {
        id: rec.id.clone(),
        name: rec.name.clone(),
        artist: rec.artist.clone(),
        artist_id: rec.artist_id.clone(),
        cover_art: rec.cover_art.clone(),
    }
}

fn song_dto(rec: &Record) -> SongDto {
    SongDto {
        id: rec.id.clone(),
        title: rec.name.clone(),
        artist: rec.artist.clone(),
        artist_id: rec.artist_id.clone(),
        album: rec.album.clone(),
        album_id: rec.album_id.clone(),
        // Track records carry no cover of their own; the album id resolves
        // to the album cover via the polymorphic getCoverArt.
        cover_art: rec.album_id.clone(),
        duration: rec.duration_seconds,
    }
}

/// Proxy Navidrome `search3` and map it into the same DTO shape, so the
/// client can't tell a boot-window fallback from an indexed result.
async fn fallback(
    upstream: &UpstreamConfig,
    text: &str,
    artist_count: usize,
    album_count: usize,
    song_count: usize,
) -> anyhow::Result<SearchResult3Dto> {
    let creds = Credentials {
        username: upstream.username.clone(),
        password: upstream.password.clone(),
    };
    let client = SubsonicClient::new(&upstream.navidrome_url, creds)?;
    // search3 caps all three buckets by one count; use the max so no bucket
    // is under-filled, then trust Navidrome's own order.
    let count = u32::try_from(artist_count.max(album_count).max(song_count)).unwrap_or(u32::MAX);
    let res = client.search3(text, count, 0).await?;
    Ok(SearchResult3Dto {
        artist: res
            .artists
            .into_iter()
            .take(artist_count)
            .map(|a| ArtistDto { id: a.id.as_str().to_string(), name: a.name, cover_art: None })
            .collect(),
        album: res
            .albums
            .into_iter()
            .take(album_count)
            .map(|al| AlbumDto {
                id: al.id.as_str().to_string(),
                name: al.name,
                artist: al.artist_name,
                artist_id: al.artist_id.map(|id| id.as_str().to_string()),
                cover_art: al.cover_art_id,
            })
            .collect(),
        song: res
            .tracks
            .into_iter()
            .take(song_count)
            .map(|t| {
                let album_id = t.album_id.map(|id| id.as_str().to_string());
                SongDto {
                    id: t.id.as_str().to_string(),
                    title: t.title,
                    artist: t.artist_name,
                    artist_id: t.artist_id.map(|id| id.as_str().to_string()),
                    album: t.album_name,
                    cover_art: album_id.clone(),
                    album_id,
                    duration: t.duration_seconds,
                }
            })
            .collect(),
    })
}
