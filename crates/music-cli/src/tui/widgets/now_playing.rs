//! Bottom bar: status/title line, progress gauge, hint line. Three rows.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Paragraph};

use super::super::state::{App, FeedbackVote, Rating};
use super::super::theme::{Theme, symbols};
use super::mmss;

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let [title_row, gauge_row, hints_row] =
        Layout::vertical([Constraint::Length(1); 3]).areas(area);

    draw_title_line(f, title_row, app, theme);
    draw_gauge(f, gauge_row, app, theme);
    draw_hints(f, hints_row, app, theme);
}

/// Line 1: transient status if present, else `► Title — Artist ♥`.
fn draw_title_line(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if let Some(status) = &app.status {
        let style = if status.is_error { theme.error } else { theme.dim };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {}", status.text), style))),
            area,
        );
        return;
    }

    let mut spans: Vec<Span> = Vec::new();
    if let Some(cur) = app.queue.current() {
        let glyph = if app.pending_load.is_some() {
            symbols::SPINNER[usize::try_from(app.tick).unwrap_or(0) % symbols::SPINNER.len()]
        } else if app.playback.playing {
            symbols::PLAYING
        } else {
            symbols::PAUSED
        };
        spans.push(Span::styled(format!(" {glyph} "), theme.playing));
        spans.push(Span::styled(cur.title.clone(), theme.playing));
        if let Some(artist) = &cur.artist {
            spans.push(Span::styled(format!(" — {artist}"), theme.text));
        }
        match app.ratings.get(&cur.id) {
            Some(Rating::Like) => {
                spans.push(Span::styled(format!("  {}", symbols::LIKE), theme.like));
            }
            Some(Rating::Dislike) => {
                spans.push(Span::styled(format!("  {}", symbols::DISLIKE), theme.dislike));
            }
            None => {}
        }
        // Autoplay pick: show the feedback thumb it carries (f / F rate it).
        if app.autoplay.recommended.contains(&cur.id) {
            match app.autoplay.votes.get(&cur.id) {
                Some(FeedbackVote::Up) => {
                    spans.push(Span::styled(format!("  {}", symbols::THUMB_UP), theme.like));
                }
                Some(FeedbackVote::Down) => {
                    spans.push(Span::styled(format!("  {}", symbols::THUMB_DOWN), theme.dislike));
                }
                None => {}
            }
        }
    } else if app.no_audio_device {
        spans.push(Span::styled(
            " no audio device — playback disabled",
            theme.dim,
        ));
    } else {
        spans.push(Span::styled(" nothing playing", theme.dim));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_gauge(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let position = app.playback.position;
    let duration = app.playback.duration;
    let ratio = duration
        .filter(|d| !d.is_zero())
        .map_or(0.0, |d| (position.as_secs_f64() / d.as_secs_f64()).clamp(0.0, 1.0));
    let label = match duration {
        Some(d) => format!("{} ╱ {}", mmss(position), mmss(d)),
        None if app.playback.track_id.is_some() => mmss(position),
        None => String::new(),
    };
    let gauge = Gauge::default()
        .ratio(ratio)
        .label(Span::styled(label, theme.text))
        .gauge_style(theme.accent)
        .use_unicode(true);
    f.render_widget(gauge, area);
}

fn draw_hints(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let volume = format!("vol {:>3.0}%", f64::from(app.playback.volume) * 100.0);
    let hints = " space pause · n/p track · ,/. seek · -/= vol · ? help";
    // Autoplay badge sits between the hints and the volume readout (accent
    // when on, so its presence reads at a glance).
    let autoplay = if app.autoplay.enabled {
        format!("{} auto  ", symbols::AUTOPLAY)
    } else {
        String::new()
    };
    let pad = usize::from(area.width).saturating_sub(
        hints.chars().count() + autoplay.chars().count() + volume.chars().count() + 1,
    );
    let line = Line::from(vec![
        Span::styled(hints, theme.dim),
        Span::raw(" ".repeat(pad)),
        Span::styled(autoplay, theme.accent),
        Span::styled(volume, theme.dim),
    ]);
    f.render_widget(Paragraph::new(line), area);
}
