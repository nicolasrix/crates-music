//! `music sync` subcommand: small client over the gateway sync surface.
//!
//! Read:
//! - `state`: GET /v1/sync/snapshot, pretty-print as JSON.
//! - `queue`: GET /v1/sync/snapshot, render as a readable table with
//!   resolved track titles and the item ids `remove`/`move` need.
//! - `watch`: WS /v1/sync, print every server frame as one JSON line.
//!
//! Mutate (all POST /v1/sync/ops, a single `SyncOp`):
//! - `push`:   `Push`        — append, idempotent on item_id.
//! - `remove`: `Remove`      — drop a queue item by id.
//! - `move`:   `Reorder`     — move an item to a new index.
//! - `jump`:   `SetNowPlaying`— set the shared now-playing cursor.
//! - `clear`:  `Clear`       — empty the queue and reset playback state.
//!
//! The CLI doesn't drive its own playback off sync state — that's a future
//! integration. For now, this inspects what other devices are doing and
//! lets the terminal act as a cross-device queue remote.

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use futures_util::future::join_all;
use music_core::{QueueItemId, TrackId};
use music_subsonic::Client;
use music_sync::{ServerMessage, SyncOp, SyncState};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::config::{Config, GatewayConfig};
use crate::gateway::{http_client, require_gateway, ws_url_for};

pub async fn run_state(config: &Config) -> Result<()> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = format!("{}/v1/sync/snapshot", gw.url.trim_end_matches('/'));
    let body: serde_json::Value = http_client(gw)?
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .context("requesting snapshot")?
        .error_for_status()
        .context("snapshot returned error status")?
        .json()
        .await
        .context("parsing snapshot body")?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

pub async fn run_push(config: &Config, track_ids: &[String]) -> Result<()> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let client = http_client(gw)?;
    for track_id in track_ids {
        let item_id = new_item_id();
        let op = SyncOp::Push {
            item_id: QueueItemId::from(item_id.clone()),
            track_id: TrackId::from(track_id.clone()),
        };
        let version = submit_op(gw, &client, &token, &op)
            .await
            .with_context(|| format!("pushing track {track_id}"))?;
        println!("pushed {track_id} as item {item_id} (version {version})");
    }
    Ok(())
}

/// Remove a queue item by id (no-op server-side if absent).
pub async fn run_remove(config: &Config, item_id: &str) -> Result<()> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let op = SyncOp::Remove {
        item_id: QueueItemId::from(item_id.to_string()),
    };
    let version = submit_op(gw, &http_client(gw)?, &token, &op).await?;
    println!("removed item {item_id} (version {version})");
    Ok(())
}

/// Move a queue item to `new_index` (clamped server-side). The now-playing
/// cursor follows the moved track, not the index.
pub async fn run_move(config: &Config, item_id: &str, new_index: usize) -> Result<()> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let op = SyncOp::Reorder {
        item_id: QueueItemId::from(item_id.to_string()),
        new_index,
    };
    let version = submit_op(gw, &http_client(gw)?, &token, &op).await?;
    println!("moved item {item_id} to index {new_index} (version {version})");
    Ok(())
}

/// Set the now-playing cursor to a queue position. Shared playback state —
/// an out-of-bounds index is rejected by the gateway (422).
pub async fn run_jump(config: &Config, index: usize) -> Result<()> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let op = SyncOp::SetNowPlaying { index: Some(index) };
    let version = submit_op(gw, &http_client(gw)?, &token, &op).await?;
    println!("now-playing set to index {index} (version {version})");
    Ok(())
}

/// Fetch the snapshot and print the queue as a readable table: now-playing
/// marker, 0-based position, item id (the handle for `remove`/`move`), and
/// the resolved track title — ids that fail to resolve fall back to the raw
/// track id so the row count always matches the real queue.
pub async fn run_queue(config: &Config, client: &Client) -> Result<()> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let url = format!("{}/v1/sync/snapshot", gw.url.trim_end_matches('/'));
    let state: SyncState = http_client(gw)?
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .context("requesting snapshot")?
        .error_for_status()
        .context("snapshot returned error status")?
        .json()
        .await
        .context("parsing snapshot")?;

    let items = &state.playback.queue.items;
    if items.is_empty() {
        println!("(queue empty)");
        return Ok(());
    }
    let now = state.playback.now_playing_index;

    // Resolve titles concurrently; order is preserved by `zip` below.
    let resolved = join_all(items.iter().map(|it| client.get_song(&it.track_id))).await;

    let item_w = items
        .iter()
        .map(|it| it.item_id.as_str().len())
        .max()
        .unwrap_or(4)
        .max(4);

    println!(
        "{:<2} {:>3}  {:<item_w$}  TRACK",
        "", "POS", "ITEM",
        item_w = item_w
    );
    for (i, (item, song)) in items.iter().zip(resolved).enumerate() {
        let marker = if now == Some(i) { "▶" } else { "" };
        let track = match song {
            Ok(t) => match t.artist_name {
                Some(artist) => format!("{} — {artist}", t.title),
                None => t.title,
            },
            Err(_) => format!("(unresolved {})", item.track_id.as_str()),
        };
        println!(
            "{marker:<2} {i:>3}  {:<item_w$}  {track}",
            item.item_id.as_str(),
            item_w = item_w
        );
    }
    let state_word = if state.playback.is_playing {
        "playing"
    } else {
        "paused"
    };
    println!("\n{} item(s), {state_word} (version {})", items.len(), state.version);
    Ok(())
}

/// Empty the queue and reset shared playback (cursor, position, playing
/// flag, session anchor). Always succeeds — `Clear` takes no arguments and
/// the gateway never rejects it.
pub async fn run_clear(config: &Config) -> Result<()> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let version = submit_op(gw, &http_client(gw)?, &token, &SyncOp::Clear).await?;
    println!("cleared queue (version {version})");
    Ok(())
}

/// POST a single op to `/v1/sync/ops` and return the new state version.
/// Centralises the success/error handling for every op-submitting command.
async fn submit_op(
    gw: &GatewayConfig,
    client: &reqwest::Client,
    token: &str,
    op: &SyncOp,
) -> Result<u64> {
    let url = format!("{}/v1/sync/ops", gw.url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(op)
        .send()
        .await
        .context("submitting sync op")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("op rejected ({status}): {body}");
    }
    let ack: serde_json::Value = resp.json().await.context("parsing op ack")?;
    Ok(ack["version"].as_u64().unwrap_or_default())
}

pub async fn run_watch(config: &Config) -> Result<()> {
    let gw = require_gateway(config)?;
    let token = crate::auth::resolve_bearer(config, gw).await?;
    let mut ws_url = ws_url_for(&gw.url, "/v1/sync")?;
    // Browser parity: pass the bearer token as `access_token=` rather
    // than via the Authorization header. The gateway accepts both;
    // query-string auth keeps the WS handshake plumbing trivial.
    ws_url
        .query_pairs_mut()
        .append_pair("access_token", &token);

    let (mut ws, _resp) = connect_async(ws_url.as_str())
        .await
        .context("connecting WS")?;
    while let Some(frame) = ws.next().await {
        match frame.context("WS protocol error")? {
            Message::Text(t) => {
                // Validate the frame so a malformed payload surfaces,
                // but emit the raw text so the CLI's stdout remains a
                // faithful tee of the server's output.
                let _: ServerMessage = serde_json::from_str(&t).unwrap_or(ServerMessage::OpError {
                    message: "unknown frame".into(),
                });
                println!("{t}");
            }
            Message::Close(_) => break,
            _ => {} // ignore Ping/Pong/Binary
        }
    }
    Ok(())
}

/// Short, lexically-sortable item id. Server doesn't care about the
/// shape — clients pick one and Push is idempotent on collisions.
/// Uniqueness within a single CLI invocation is sufficient (there's no
/// concurrent CLI usage from one terminal); the millisecond prefix
/// covers cross-invocation uniqueness for the rare case of two
/// invocations within the same millisecond.
fn new_item_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0u64, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    format!("cli-{ms:x}-{n:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_ids_are_unique_within_a_burst() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(new_item_id()), "duplicate item id");
        }
    }
}
