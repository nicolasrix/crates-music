pub(crate) mod diagnostics;
pub(crate) mod downloads;
pub(crate) mod help;
pub(crate) mod library;
pub(crate) mod liked;
pub(crate) mod lyrics;
pub(crate) mod playlists;
pub(crate) mod queue;
pub(crate) mod search;
pub(crate) mod settings;
pub(crate) mod stations;

use std::collections::HashMap;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table};

use super::state::{Loadable, Rating};
use super::theme::{Theme, symbols};
use super::widgets::mmss;

/// Standard "not Ready" rendering for a remote-data slot. Returns `true`
/// when it drew something (the caller should stop).
pub(crate) fn draw_not_ready<T>(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    loadable: &Loadable<T>,
    idle_hint: &str,
    tick: u64,
) -> bool {
    let (text, style) = match loadable {
        Loadable::Ready(_) => return false,
        Loadable::Idle => (idle_hint.to_owned(), theme.dim),
        Loadable::Loading => {
            let spin =
                symbols::SPINNER[usize::try_from(tick).unwrap_or(0) % symbols::SPINNER.len()];
            (format!("{spin} loading…"), theme.dim)
        }
        Loadable::Failed(e) => (e.clone(), theme.error),
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {text}"), style))).wrap(
            ratatui::widgets::Wrap { trim: true },
        ),
        area,
    );
    true
}

/// Everything a track row needs to render, independent of source type.
pub(crate) struct TrackRowData<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub artist: Option<&'a str>,
    pub album: Option<&'a str>,
    pub duration: Option<Duration>,
    pub is_current: bool,
}

/// Shared track table: marker column (playing / rating glyph), title,
/// artist, album, duration.
pub(crate) fn track_table<'a>(
    rows: impl Iterator<Item = TrackRowData<'a>>,
    ratings: &HashMap<String, Rating>,
    theme: &Theme,
    show_album: bool,
) -> Table<'a> {
    let body = rows.map(|r| {
        let (marker, marker_style) = if r.is_current {
            (symbols::PLAYING, theme.playing)
        } else {
            match ratings.get(r.id) {
                Some(Rating::Like) => (symbols::LIKE, theme.like),
                Some(Rating::Dislike) => (symbols::DISLIKE, theme.dislike),
                None => (" ", theme.text),
            }
        };
        let row_style = if r.is_current { theme.playing } else { theme.text };
        let mut cells = vec![
            Cell::from(Span::styled(marker.to_owned(), marker_style)),
            Cell::from(r.title.to_owned()),
            Cell::from(r.artist.unwrap_or("—").to_owned()),
        ];
        if show_album {
            cells.push(Cell::from(r.album.unwrap_or("—").to_owned()));
        }
        cells.push(Cell::from(
            r.duration.map_or_else(|| "—".to_owned(), mmss),
        ));
        Row::new(cells).style(row_style)
    });

    let mut widths = vec![Constraint::Length(2), Constraint::Fill(3), Constraint::Fill(2)];
    if show_album {
        widths.push(Constraint::Fill(2));
    }
    widths.push(Constraint::Length(8));

    Table::new(body, widths)
        .row_highlight_style(theme.selected)
        .column_spacing(1)
}
