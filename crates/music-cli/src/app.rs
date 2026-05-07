//! Runtime: dispatches CLI commands against a Subsonic client.

use std::io::Cursor;
use std::path::Path;

use anyhow::Context;
use music_core::{AlbumId, TrackId};
use music_subsonic::{Client, Credentials};

use crate::cli::{Cli, Command};
use crate::config::Config;
use crate::format::{albums_table, tracks_table};

pub async fn run(cli: Cli, config_path_override: Option<&Path>) -> anyhow::Result<()> {
    let config = load_config(config_path_override.or(cli.config.as_deref()))?;
    let client = Client::new(
        &config.server.url,
        Credentials {
            username: config.server.username,
            password: config.server.password,
        },
    )
    .context("constructing Subsonic client")?;

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
            println!(
                "{}{}{}",
                result.album.name,
                result
                    .album
                    .artist_name
                    .as_deref()
                    .map(|a| format!(" — {a}"))
                    .unwrap_or_default(),
                result
                    .album
                    .year
                    .map(|y| format!(" ({y})"))
                    .unwrap_or_default(),
            );
            println!();
            print!("{}", tracks_table(&result.tracks));
        }
        Command::Play { track_id } => {
            play_track(&client, &TrackId::from(track_id)).await?;
        }
    }
    Ok(())
}

fn load_config(path_override: Option<&Path>) -> anyhow::Result<Config> {
    let path = match path_override {
        Some(p) => p.to_path_buf(),
        None => crate::config::default_config_path()
            .context("could not determine default config path")?,
    };
    Config::load(&path)
}

async fn play_track(client: &Client, track_id: &TrackId) -> anyhow::Result<()> {
    let url = client.stream_url(track_id)?;
    tracing::info!(%url, "fetching track");
    let bytes = client
        .http()
        .get(url)
        .send()
        .await
        .context("stream request failed")?
        .error_for_status()
        .context("stream request returned error status")?
        .bytes()
        .await
        .context("reading stream body")?;

    tracing::info!(bytes = bytes.len(), "decoded; starting playback");
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let cursor = Cursor::new(bytes);
        let (_stream, handle) =
            rodio::OutputStream::try_default().context("opening default audio output")?;
        let sink = rodio::Sink::try_new(&handle).context("creating audio sink")?;
        let source = rodio::Decoder::new(cursor).context("decoding audio stream")?;
        sink.append(source);
        sink.sleep_until_end();
        Ok(())
    })
    .await
    .context("playback task panicked")??;
    Ok(())
}
