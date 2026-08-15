//! Lyric-pane reducer: opening, following, reading, seeking, re-resolving.
//!
//! The pane has no timer of its own. Which line is highlighted is derived
//! from the playback snapshot at draw time (see `views::lyrics`), and the
//! only thing the reducer watches for is the *track* changing — which it
//! does by comparing the snapshot's id against the document it is holding,
//! on the tick that already runs. That is why there is no "track changed"
//! message to forget to emit.

use crate::api::{LyricsDoc, LyricsOutcome};

use super::super::lyrics::{HIGHLIGHT_LEAD_MS, active_line, sorted_lines};
use super::super::msg::Effect;
use super::super::state::{App, Loadable, Overlay};

/// `y` — open the pane on the now-playing track, or close it.
pub(super) fn toggle(app: &mut App) -> Vec<Effect> {
    if app.overlay == Overlay::Lyrics {
        app.overlay = Overlay::None;
        return vec![];
    }
    let Some(track_id) = app.playback.track_id.clone() else {
        app.set_status("nothing playing", false);
        return vec![];
    };
    // Opening never closes another overlay's state, it replaces the overlay
    // slot — the picker and text prompt both own modal input we would be
    // stealing mid-edit, so they take priority by simply not being here
    // (keymap routes their keys before ours).
    app.overlay = Overlay::Lyrics;
    app.lyrics.following = true;
    app.lyrics.cursor = 0;
    load_if_stale(app, &track_id)
}

/// Fetch when the pane is showing a different track than the player is.
/// Also the reload path after a track change, hence "if stale".
fn load_if_stale(app: &mut App, track_id: &str) -> Vec<Effect> {
    if app.lyrics.track_id.as_deref() == Some(track_id)
        && !matches!(app.lyrics.doc, Loadable::Idle)
    {
        return vec![];
    }
    app.lyrics.track_id = Some(track_id.to_owned());
    app.lyrics.doc = Loadable::Loading;
    app.lyrics.lines = Vec::new();
    vec![Effect::LoadLyrics {
        track_id: track_id.to_owned(),
        force: false,
    }]
}

/// Called from the 250 ms tick while the pane is open. Cheap: a string
/// compare against the snapshot, and only on the frames where the pane is
/// actually visible.
pub(super) fn on_tick(app: &mut App) -> Vec<Effect> {
    if app.overlay != Overlay::Lyrics {
        return vec![];
    }
    let Some(track_id) = app.playback.track_id.clone() else {
        return vec![];
    };
    if app.lyrics.track_id.as_deref() == Some(track_id.as_str()) {
        return vec![];
    }
    // New track: start following again and go back to the top, which is
    // what a listener who left the pane open expects to see.
    app.lyrics.following = true;
    app.lyrics.cursor = 0;
    load_if_stale(app, &track_id)
}

/// The line being sung right now, independent of where the reader is
/// looking. `None` during an intro, or with no timed document.
pub(crate) fn active_index(app: &App) -> Option<usize> {
    active_line(&app.lyrics.lines, position_ms(app))
}

/// The line the pane is focused on — i.e. what it scrolls to keep visible:
/// the playhead's while following, the reader's otherwise.
pub(crate) fn focus_index(app: &App) -> Option<usize> {
    if app.lyrics.lines.is_empty() {
        return None;
    }
    if app.lyrics.following {
        active_index(app)
    } else {
        Some(app.lyrics.cursor.min(app.lyrics.lines.len() - 1))
    }
}

/// Playhead in milliseconds, biased by the highlight lead.
pub(crate) fn position_ms(app: &App) -> i64 {
    let elapsed = i64::try_from(app.playback.position.as_millis()).unwrap_or(i64::MAX);
    elapsed.saturating_add(HIGHLIGHT_LEAD_MS)
}

/// j/k — read ahead or back. The first move seeds the cursor from wherever
/// the highlight currently is, so reading starts where you were looking
/// rather than at the top of the song.
pub(super) fn mv(app: &mut App, delta: i32) -> Vec<Effect> {
    let len = app.lyrics.lines.len();
    if len == 0 {
        return vec![];
    }
    let from = if app.lyrics.following {
        active_line(&app.lyrics.lines, position_ms(app)).unwrap_or(0)
    } else {
        app.lyrics.cursor
    };
    app.lyrics.following = false;
    let next = i64::try_from(from).unwrap_or(0) + i64::from(delta);
    let max = i64::try_from(len - 1).unwrap_or(0);
    app.lyrics.cursor = usize::try_from(next.clamp(0, max)).unwrap_or(0);
    vec![]
}

/// Enter — seek to the focused line and resume following.
///
/// Seeking is local-only, exactly as the scrubber and `,`/`.` are: the sync
/// model has no seek op, so this inherits that rather than introducing a
/// lyrics-specific divergence.
pub(super) fn seek_to_focus(app: &mut App) -> Vec<Effect> {
    let Some(idx) = focus_index(app) else {
        return vec![];
    };
    let Some(line) = app.lyrics.lines.get(idx) else {
        return vec![];
    };
    let target = std::time::Duration::from_millis(line.start_ms.max(0).unsigned_abs());
    if let Some(p) = &app.player {
        p.seek_to(target);
    }
    // Snap back to following: the reason to click a line is to hear it, and
    // after the seek the playhead *is* where the reader was looking.
    app.lyrics.following = true;
    vec![]
}

/// `R` — ask the gateway to resolve again, discarding its cached answer.
pub(super) fn refresh(app: &mut App) -> Vec<Effect> {
    if app.lyrics.refreshing {
        return vec![];
    }
    let Some(track_id) = app.lyrics.track_id.clone() else {
        return vec![];
    };
    app.lyrics.refreshing = true;
    app.set_status("looking for better lyrics…", false);
    vec![Effect::LoadLyrics {
        track_id,
        force: true,
    }]
}

/// A fetch (or refresh) landed.
pub(super) fn loaded(
    app: &mut App,
    track_id: &str,
    result: Result<LyricsOutcome, String>,
) -> Vec<Effect> {
    app.lyrics.refreshing = false;
    // Late arrival for a track we've already moved past. Dropping it is the
    // whole reason the effect echoes the id back.
    if app.lyrics.track_id.as_deref() != Some(track_id) {
        return vec![];
    }
    match result {
        Ok(outcome) => {
            app.lyrics.lines = timed_lines(&outcome);
            app.lyrics.doc = Loadable::Ready(outcome);
        }
        Err(e) => {
            app.lyrics.lines = Vec::new();
            app.lyrics.doc = Loadable::Failed(e);
        }
    }
    vec![]
}

/// The sorted timed lines of an outcome, or empty for anything unsynced.
fn timed_lines(outcome: &LyricsOutcome) -> Vec<crate::api::LyricLine> {
    let LyricsOutcome::Doc(doc) = outcome else {
        return Vec::new();
    };
    let doc: &LyricsDoc = doc;
    match (doc.synced, doc.lines.as_deref()) {
        (true, Some(lines)) => sorted_lines(lines),
        _ => Vec::new(),
    }
}
