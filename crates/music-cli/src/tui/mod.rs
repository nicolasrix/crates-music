//! Interactive full-screen mode.
//!
//! Architecture (Elm-style): the event loop owns the terminal and a message
//! channel. Keys are translated to semantic [`msg::Msg`]s by [`keymap`],
//! the pure-ish reducer in [`update`] mutates [`state::App`] and returns
//! [`msg::Effect`] descriptions, and [`effects`] runs each effect as a
//! detached tokio task whose completion is just another `Msg`. Drawing
//! never awaits network or disk; slow I/O can never freeze a keypress.

mod autoplay;
mod effects;
mod keymap;
mod mpris;
mod msg;
mod render;
mod section_state;
mod signal;
mod state;
mod sync_ws;
mod terminal;
mod theme;
mod update;
mod views;
mod widgets;

use std::sync::Arc;

use anyhow::Context;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::StreamExt;
use music_player::Player;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::config::Config;

use self::effects::Ctx;
use self::msg::Msg;
use self::state::{App, Section};
use self::theme::Theme;

/// Frame/tick cadence: fast enough for a smooth progress bar, slow enough
/// to be invisible in `top`.
const TICK: std::time::Duration = std::time::Duration::from_millis(250);

/// Budget for the best-effort event flush on quit — long enough for one
/// LAN round-trip, short enough that a dead gateway can't hold the shell.
const FINAL_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

pub async fn run(config: Config) -> anyhow::Result<()> {
    let config = Arc::new(config);

    // Build shared plumbing *before* touching the terminal so config/auth
    // errors print like any other CLI error instead of flashing a TUI.
    let client = crate::app::build_client(&config)
        .await
        .context("constructing Subsonic client")?;
    let bearer = match &config.gateway {
        Some(gw) => Some(crate::auth::resolve_bearer(&config, gw).await?),
        None => None,
    };
    let cache = Arc::new(crate::app::open_audio_cache(&config).await?);

    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Msg>();
    let (player_tx, mut player_rx) = mpsc::unbounded_channel();
    let (player, no_audio_device) = match Player::spawn(player_tx) {
        Ok(p) => (Some(p), false),
        Err(e) => {
            tracing::warn!(error = %e, "no audio output — browse-only mode");
            (None, true)
        }
    };

    // The sync-room WS task runs for the whole session when a gateway is
    // configured: it owns the connection, reconnects on its own, and feeds
    // frames into the msg channel. In direct mode there's no room to join.
    let sync_ops = config
        .gateway
        .is_some()
        .then(|| sync_ws::spawn(Arc::clone(&config), msg_tx.clone()));

    let live_settings = effects::LiveSettings {
        stream_quality: config.playback.stream_quality,
        download_quality: config.playback.download_quality,
        autoplay: config.tui.autoplay.clone(),
    };
    let ctx = Ctx::new(
        Arc::clone(&config),
        client,
        bearer,
        cache,
        live_settings,
        msg_tx.clone(),
        sync_ops,
    );

    // The terminal session lives in this block so its guard drops (and the
    // terminal is restored) before the final event flush below — a slow
    // gateway on exit must not hold the user's shell hostage in raw mode.
    let session = {
        terminal::install_panic_hook();
        let _guard = terminal::TerminalGuard::enter()?;
        let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
        let theme = Theme::detect();

        let mut app = App::new(player, no_audio_device, config.gateway.is_some());
        app.configure_autoplay(&config.tui.autoplay);
        app.configure_settings(&config);
        // Kick off the initial library load.
        for effect in update::update(&mut app, Msg::GoSection(Section::Library)) {
            effects::spawn(effect, &ctx);
        }
        // Boot-time identity fetch (role-gated UI). Cosmetic; the server
        // stays the enforcement point. Skipped in direct mode.
        if config.gateway.is_some() {
            effects::spawn(msg::Effect::LoadWhoami, &ctx);
        }

        // Desktop media keys + now-playing metadata over D-Bus (Linux). A
        // no-op elsewhere / when there's no session bus. Dropped on quit.
        let mpris = mpris::Bridge::spawn(msg_tx.clone());

        let mut events = EventStream::new();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        while !app.should_quit {
            if let Some(p) = &app.player {
                app.playback = p.snapshot();
            }
            mpris.publish(&app);
            term.draw(|f| render::draw(f, &mut app, &theme))?;

            let msg = tokio::select! {
                ev = events.next() => match ev {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        match keymap::action_for(&app, key) {
                            Some(msg) => msg,
                            None => continue,
                        }
                    }
                    // Resize (and ignored key kinds / mouse) → just redraw.
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(e).context("reading terminal events"),
                    None => break,
                },
                Some(pe) = player_rx.recv() => Msg::Player(pe),
                Some(m) = msg_rx.recv() => m,
                _ = tick.tick() => Msg::Tick,
            };

            for effect in update::update(&mut app, msg) {
                effects::spawn(effect, &ctx);
            }
        }
        (std::mem::take(&mut app.events_outbox), app.exit_message.take())
        // _guard drops here → terminal restored, even on the `?` paths above.
    };
    let (leftover_events, exit_message) = session;

    flush_leftover_events(&config, &leftover_events).await;

    // The sign-out "auth-needed screen": a shell line printed after the
    // terminal is restored, telling the user how to sign back in.
    if let Some(msg) = exit_message {
        println!("{msg}");
    }
    Ok(())
}

/// Best-effort tail flush of whatever the tick cadence hadn't sent yet.
/// Quitting mid-track is not a skip (mirrors the web: closing the tab doesn't
/// emit one). Known accepted gap: a batch already in flight at quit is not
/// retried if its POST fails — retrying would risk double-sending on success,
/// and the events are advisory.
async fn flush_leftover_events(config: &Config, leftover: &[signal::PendingEvent]) {
    if leftover.is_empty() {
        return;
    }
    let outgoing: Vec<_> = leftover.iter().map(signal::PendingEvent::to_outgoing).collect();
    let flush = crate::api::post_events(config, &outgoing);
    match tokio::time::timeout(FINAL_FLUSH_TIMEOUT, flush).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::debug!(error = %e, "final event flush failed"),
        Err(_) => tracing::debug!("final event flush timed out"),
    }
}
