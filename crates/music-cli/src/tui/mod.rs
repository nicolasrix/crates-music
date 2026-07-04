//! Interactive full-screen mode.
//!
//! Architecture (Elm-style): the event loop owns the terminal and a message
//! channel. Keys are translated to semantic [`msg::Msg`]s by [`keymap`],
//! the pure-ish reducer in [`update`] mutates [`state::App`] and returns
//! [`msg::Effect`] descriptions, and [`effects`] runs each effect as a
//! detached tokio task whose completion is just another `Msg`. Drawing
//! never awaits network or disk; slow I/O can never freeze a keypress.

mod effects;
mod keymap;
mod msg;
mod render;
mod state;
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

    let ctx = Ctx::new(Arc::clone(&config), client, bearer, cache, msg_tx.clone());

    terminal::install_panic_hook();
    let _guard = terminal::TerminalGuard::enter()?;
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let theme = Theme::detect();

    let mut app = App::new(player, no_audio_device);
    // Kick off the initial library load.
    for effect in update::update(&mut app, Msg::GoSection(Section::Library)) {
        effects::spawn(effect, &ctx);
    }

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while !app.should_quit {
        if let Some(p) = &app.player {
            app.playback = p.snapshot();
        }
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
    Ok(())
    // _guard drops here → terminal restored, even on the `?` paths above.
}
