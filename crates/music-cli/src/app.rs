//! Runtime: dispatches CLI commands against a Subsonic client.

use std::path::Path;

use anyhow::Context;
use bytes::Bytes;
use music_cache::{AudioCache, AudioKey, PinOutcome, UnpinOutcome};
use music_core::{AlbumId, ArtistId, TrackId};
use music_player::{play_queue_blocking, read_cached, resolve_source};
use music_subsonic::{Client, Credentials, SearchResult3};

use crate::cli::{CacheAction, Cli, Command, SyncAction};
use crate::config::{Config, resolve_cache_root};
use crate::format::{album_header, albums_table, artist_header, artists_table, tracks_table};

pub async fn run(cli: Cli, config_path_override: Option<&Path>) -> anyhow::Result<()> {
    let config = load_config(config_path_override.or(cli.config.as_deref()))?;
    let client = build_client(&config).context("constructing Subsonic client")?;

    match cli.command {
        Command::Ping => {
            client.ping().await.context("ping failed")?;
            println!("ok");
        }
        Command::Albums { size, kind } => {
            let albums = client
                .get_album_list2(kind.into(), Some(size), None)
                .await?;
            print!("{}", albums_table(&albums));
        }
        Command::Album { id } => {
            let result = client.get_album(&AlbumId::from(id)).await?;
            println!("{}", album_header(&result.album));
            println!();
            print!("{}", tracks_table(&result.tracks));
        }
        Command::Artists => {
            let artists = client.get_artists().await?;
            print!("{}", artists_table(&artists));
        }
        Command::Artist { id } => {
            let result = client.get_artist(&ArtistId::from(id)).await?;
            println!("{}", artist_header(&result.artist));
            println!();
            print!("{}", albums_table(&result.albums));
        }
        Command::Tracks { size, offset } => {
            // Empty query matches the whole library on Navidrome; paging is
            // via search3's shared offset.
            let result = client.search3("", size, offset).await?;
            print!("{}", tracks_table(&result.tracks));
        }
        Command::Search { query, limit } => {
            let result = client.search3(&query, limit, 0).await?;
            print_search(&result);
        }
        Command::Like { id, kind } => {
            crate::ratings::run_set(&config, kind, &id, Some("like")).await?;
        }
        Command::Dislike { id, kind } => {
            crate::ratings::run_set(&config, kind, &id, Some("dislike")).await?;
        }
        Command::Unrate { id, kind } => {
            crate::ratings::run_set(&config, kind, &id, None).await?;
        }
        Command::Liked => {
            crate::ratings::run_liked(&config).await?;
        }
        Command::Play { track_ids, offline } => {
            let cache = open_audio_cache(&config).await?;
            let ids: Vec<TrackId> = track_ids.into_iter().map(TrackId::from).collect();
            play_tracks(&client, &cache, &ids, offline).await?;
        }
        Command::Pin { track_id } => {
            let cache = open_audio_cache(&config).await?;
            run_pin(&client, &cache, &TrackId::from(track_id)).await?;
        }
        Command::Unpin { track_id } => {
            let cache = open_audio_cache(&config).await?;
            run_unpin(&cache, &TrackId::from(track_id)).await?;
        }
        Command::Pinned => {
            let cache = open_audio_cache(&config).await?;
            run_pinned(&cache).await?;
        }
        Command::Cache { action } => {
            let cache = open_audio_cache(&config).await?;
            match action {
                CacheAction::Stats => run_cache_stats(&cache).await?,
                CacheAction::Evict => run_cache_evict(&cache).await?,
            }
        }
        Command::Sync { action } => match action {
            SyncAction::State => crate::sync::run_state(&config).await?,
            SyncAction::Push { track_ids } => crate::sync::run_push(&config, &track_ids).await?,
            SyncAction::Watch => crate::sync::run_watch(&config).await?,
        },
    }
    Ok(())
}

/// Render a `search3` result as three labelled sections. Empty buckets are
/// skipped so a song-only match doesn't print bare "ARTISTS"/"ALBUMS"
/// headers. If nothing matched at all, say so on stderr-free stdout.
fn print_search(result: &SearchResult3) {
    let mut printed = false;
    if !result.artists.is_empty() {
        println!("ARTISTS");
        print!("{}", artists_table(&result.artists));
        printed = true;
    }
    if !result.albums.is_empty() {
        if printed {
            println!();
        }
        println!("ALBUMS");
        print!("{}", albums_table(&result.albums));
        printed = true;
    }
    if !result.tracks.is_empty() {
        if printed {
            println!();
        }
        println!("TRACKS");
        print!("{}", tracks_table(&result.tracks));
        printed = true;
    }
    if !printed {
        println!("(no matches)");
    }
}

fn audio_key(track_id: &TrackId) -> AudioKey {
    AudioKey {
        track_id: track_id.as_str().to_string(),
        bitrate: None,
        codec: "stream".to_string(),
    }
}

async fn run_pin(client: &Client, cache: &AudioCache, track_id: &TrackId) -> anyhow::Result<()> {
    let key = audio_key(track_id);
    // Ensure the bytes are present (fetch if not).
    if cache.get(&key).await?.is_none() {
        tracing::info!(track = track_id.as_str(), "pin: track not cached, fetching");
        fetch_into_cache(client, cache, track_id, &key).await?;
    }
    match cache.pin(&key).await? {
        PinOutcome::Pinned => println!("pinned {}", track_id.as_str()),
        PinOutcome::AlreadyPinned => println!("already pinned: {}", track_id.as_str()),
        PinOutcome::NotInCache => {
            anyhow::bail!(
                "internal: track {} not in cache after fetch",
                track_id.as_str()
            );
        }
        PinOutcome::WouldExceedBudget { over_by } => {
            anyhow::bail!(
                "pinning {} would exceed pinned budget by {} bytes — \
                 unpin something first or raise pinned_budget_bytes",
                track_id.as_str(),
                over_by
            );
        }
    }
    Ok(())
}

async fn run_unpin(cache: &AudioCache, track_id: &TrackId) -> anyhow::Result<()> {
    let key = audio_key(track_id);
    match cache.unpin(&key).await? {
        UnpinOutcome::Unpinned => println!("unpinned {}", track_id.as_str()),
        UnpinOutcome::NotPinned => println!("not pinned: {}", track_id.as_str()),
        UnpinOutcome::NotInCache => println!("not in cache: {}", track_id.as_str()),
    }
    Ok(())
}

async fn run_pinned(cache: &AudioCache) -> anyhow::Result<()> {
    let entries = cache.list_pinned().await?;
    if entries.is_empty() {
        println!("(no pinned tracks)");
        return Ok(());
    }
    for e in entries {
        println!("{:>10} bytes  {}", e.bytes, e.key.track_id);
    }
    Ok(())
}

async fn run_cache_stats(cache: &AudioCache) -> anyhow::Result<()> {
    let s = cache.stats().await?;
    println!(
        "regular: {} entries, {} / {} bytes",
        s.regular_count, s.regular_bytes, s.regular_budget_bytes
    );
    println!(
        "pinned:  {} entries, {} / {} bytes",
        s.pinned_count, s.pinned_bytes, s.pinned_budget_bytes
    );
    Ok(())
}

async fn run_cache_evict(cache: &AudioCache) -> anyhow::Result<()> {
    let total = cache.evict_lru_to_fit().await?;
    println!("regular total after eviction: {total} bytes");
    Ok(())
}

async fn fetch_into_cache(
    client: &Client,
    cache: &AudioCache,
    track_id: &TrackId,
    key: &AudioKey,
) -> anyhow::Result<()> {
    let url = client.stream_url(track_id)?;
    let http = client.http().clone();
    let _ = resolve_source(cache, key, || async move {
        let response = http
            .get(url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("stream request failed: {e}"))?
            .error_for_status()
            .map_err(|e| anyhow::anyhow!("stream returned error status: {e}"))?;
        let bytes: Bytes = response
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!("reading stream body: {e}"))?;
        Ok::<Bytes, anyhow::Error>(bytes)
    })
    .await
    .with_context(|| format!("could not fetch track {} from upstream", track_id.as_str()))?;
    Ok(())
}

async fn open_audio_cache(config: &Config) -> anyhow::Result<AudioCache> {
    let root = resolve_cache_root(&config.cache)
        .context("could not determine audio cache root (no XDG cache dir)")?;
    AudioCache::open(
        &root,
        config.cache.regular_budget_bytes,
        config.cache.pinned_budget_bytes,
    )
    .await
    .with_context(|| format!("opening audio cache at {}", root.display()))
}

fn build_client(config: &Config) -> music_subsonic::Result<Client> {
    let creds = Credentials {
        username: config.server.username.clone(),
        password: config.server.password.clone(),
    };
    if let Some(gateway) = &config.gateway {
        // Gateway mode: target the gateway URL with a bearer token. The
        // upstream Subsonic auth params (u/t/s/…) are still appended by
        // `Client`, but the gateway strips them and uses its own creds.
        Client::new(&gateway.url, creds)?.with_bearer(&gateway.bearer_token)
    } else {
        Client::new(&config.server.url, creds)
    }
}

fn load_config(path_override: Option<&Path>) -> anyhow::Result<Config> {
    let path = match path_override {
        Some(p) => p.to_path_buf(),
        None => crate::config::default_config_path()
            .context("could not determine default config path")?,
    };
    Config::load(&path)
}

async fn play_tracks(
    client: &Client,
    cache: &AudioCache,
    track_ids: &[TrackId],
    offline: bool,
) -> anyhow::Result<()> {
    // Resolve every track *before* starting playback. This is the gapless
    // pre-roll: by the time the first track's last sample is consumed,
    // decoder N+1's bytes are already in RAM and rodio's `Sink::append`
    // queue is fed back-to-back.
    let mut queue: Vec<Bytes> = Vec::with_capacity(track_ids.len());
    for track_id in track_ids {
        let key = audio_key(track_id);
        let bytes = if offline {
            match read_cached(cache, &key).await? {
                Some(bytes) => bytes,
                None => anyhow::bail!(
                    "track {} is not in the local cache; remove --offline to fetch from the server",
                    track_id.as_str()
                ),
            }
        } else {
            let url = client.stream_url(track_id)?;
            let http = client.http().clone();
            resolve_source(cache, &key, || async move {
                let response = http
                    .get(url)
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("stream request failed: {e}"))?
                    .error_for_status()
                    .map_err(|e| anyhow::anyhow!("stream returned error status: {e}"))?;
                let bytes: Bytes = response
                    .bytes()
                    .await
                    .map_err(|e| anyhow::anyhow!("reading stream body: {e}"))?;
                Ok::<Bytes, anyhow::Error>(bytes)
            })
            .await
            .with_context(|| {
                format!(
                    "could not resolve audio for track {}: server unreachable and \
                     not in local cache (try --offline to play only what's cached)",
                    track_id.as_str()
                )
            })?
        };
        tracing::info!(track = track_id.as_str(), bytes = bytes.len(), "queued");
        queue.push(bytes);
    }

    tracing::info!(queue_len = queue.len(), "starting gapless playback");
    tokio::task::spawn_blocking(move || play_queue_blocking(queue))
        .await
        .context("playback task panicked")?
        .context("playback failed")?;
    Ok(())
}
