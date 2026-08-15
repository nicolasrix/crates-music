//! The `y` pane: the now-playing track's lyrics over the main area.
//!
//! Deliberately drawn into the *body* rect rather than `f.area()`, unlike
//! the help/picker overlays. The sidebar and the now-playing bar stay
//! visible because the pane is meant to be left open while you listen —
//! covering the transport would make it a mode, not a pane.
//!
//! Nothing here is stateful: the active line is recomputed from the
//! playback snapshot on every frame. At 250 ms that is four cheap binary
//! searches a second over a list that is rarely longer than a hundred
//! entries, which is not worth caching or invalidating.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use crate::api::{LyricsDoc, LyricsOutcome};

use super::super::lyrics::window_top;
use super::super::state::{App, Loadable};
use super::super::theme::{Theme, symbols};
// The reducer owns follow-vs-cursor; the view only asks it where to look.
use super::super::update::lyrics as pane;

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let title = match &app.playback.track_id {
        Some(_) => app
            .queue
            .current()
            .map_or_else(|| " lyrics ".to_owned(), |t| format!(" {} ", t.title)),
        None => " lyrics ".to_owned(),
    };
    let block = Block::new()
        .title(Span::styled(title, theme.accent))
        .title_bottom(Span::styled(hint(app), theme.dim))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border);
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    // The attribution footer only earns a row when there is something to
    // attribute; a message screen gets the whole pane.
    let footer = attribution(app);
    let [body, foot] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(u16::from(footer.is_some())),
    ])
    .areas(inner);

    draw_body(f, body, app, theme);
    if let Some(text) = footer {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(text, theme.dim))),
            foot,
        );
    }
}

/// Bottom-border hint, which doubles as the pane's only affordance for the
/// keys it claims. Reflects follow state so "why isn't it moving?" answers
/// itself.
fn hint(app: &App) -> String {
    if app.lyrics.refreshing {
        return " looking again… ".to_owned();
    }
    if app.lyrics.lines.is_empty() {
        return " esc close · R look again ".to_owned();
    }
    if app.lyrics.following {
        " j k read · R look again · esc close ".to_owned()
    } else {
        " enter play from here · esc close ".to_owned()
    }
}

fn attribution(app: &App) -> Option<String> {
    match &app.lyrics.doc {
        Loadable::Ready(LyricsOutcome::Doc(doc)) => {
            doc.attribution().map(|a| format!(" {a}"))
        }
        _ => None,
    }
}

fn draw_body(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // Idle / loading / transport failure — the shared renderer, so the
    // spinner and error styling match every other remote-data pane.
    if super::draw_not_ready(f, area, theme, &app.lyrics.doc, "no track", app.tick) {
        return;
    }
    let Loadable::Ready(outcome) = &app.lyrics.doc else {
        return;
    };

    match outcome {
        // Each message is its own, because they call for different actions:
        // a config problem an admin fixes, an outage worth retrying, a
        // confirmed absence worth one more look, and an instrumental track
        // where the right answer is to say so and stop.
        LyricsOutcome::Disabled => {
            note(f, area, theme, "lyrics are turned off on this gateway.");
        }
        LyricsOutcome::Unavailable => note(
            f,
            area,
            theme,
            "couldn't reach a lyrics source just now — R to try again.",
        ),
        LyricsOutcome::Doc(doc) if doc.instrumental => {
            note(f, area, theme, "instrumental — no lyrics.");
        }
        LyricsOutcome::Doc(doc) if doc.is_absent() => {
            note(f, area, theme, "no lyrics found — R to look again.");
        }
        LyricsOutcome::Doc(_) if !app.lyrics.lines.is_empty() => {
            draw_timed(f, area, app, theme);
        }
        LyricsOutcome::Doc(doc) => draw_plain(f, area, theme, doc),
    }
}

fn note(f: &mut Frame, area: Rect, theme: &Theme, text: &str) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {text}"), theme.dim)))
            .wrap(Wrap { trim: true }),
        area,
    );
}

/// Timed lyrics: one row per line, scrolled so the focused line sits in the
/// middle of the pane.
fn draw_timed(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let lines = &app.lyrics.lines;
    let height = usize::from(area.height);
    let active = pane::active_index(app);
    let focus = pane::focus_index(app).unwrap_or(0);
    let top = window_top(focus, lines.len(), height);

    let rendered: Vec<Line> = lines
        .iter()
        .enumerate()
        .skip(top)
        .take(height)
        .map(|(idx, line)| {
            // Three states, and they have to stay distinguishable: the line
            // being sung, the line the reader has selected (only ever
            // different while not following), and everything else.
            let is_active = Some(idx) == active;
            let is_cursor = !app.lyrics.following && idx == focus;
            let style = match (is_cursor, is_active) {
                (true, _) => theme.selected,
                (false, true) => theme.playing,
                (false, false) => theme.dim,
            };
            let marker = if is_active { symbols::FOCUS } else { " " };
            // A timed line can legitimately be empty — an instrumental gap.
            // Keep the row so the highlight still travels through it.
            let text = if line.text.is_empty() { " " } else { &line.text };
            Line::from(vec![
                Span::styled(format!("{marker} "), theme.accent),
                Span::styled(text.to_owned(), style),
            ])
        })
        .collect();

    f.render_widget(Paragraph::new(rendered), area);
}

/// Unsynced lyrics: static text, no highlight, nothing to seek to.
fn draw_plain(f: &mut Frame, area: Rect, theme: &Theme, doc: &LyricsDoc) {
    let Some(plain) = doc.plain.as_deref() else {
        note(f, area, theme, "no lyrics found — R to look again.");
        return;
    };
    let mut out = vec![Line::from(Span::styled(
        " no timings for this one — text only.",
        theme.dim,
    ))];
    out.extend(
        plain
            .lines()
            .map(|l| Line::from(Span::styled(format!("  {l}"), theme.text))),
    );
    f.render_widget(Paragraph::new(out).wrap(Wrap { trim: false }), area);
}
