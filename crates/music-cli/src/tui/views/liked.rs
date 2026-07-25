//! Liked: every rated entity, likes then dislikes, tracks resolved to
//! titles.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::widgets::{Cell, Row, Table};

use super::super::state::{App, Rating};
use super::super::theme::{Theme, symbols};
use super::draw_not_ready;

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if draw_not_ready(
        f,
        area,
        theme,
        &app.liked.entries,
        "no ratings yet — L likes, D dislikes, from any track list",
        app.tick,
    ) {
        return;
    }
    let Some(entries) = app.liked.entries.ready() else {
        return;
    };

    let rows = entries.iter().map(|e| {
        let (glyph, style) = match e.rating {
            Rating::Like => (symbols::LIKE, theme.like),
            Rating::Dislike => (symbols::DISLIKE, theme.dislike),
        };
        let (title, artist) = match &e.track {
            Some(t) => (
                t.title.clone(),
                t.artist_name.clone().unwrap_or_else(|| "—".to_owned()),
            ),
            // Album/artist rows: their resolved name, else the raw id.
            None => (e.label.clone().unwrap_or_else(|| e.id.clone()), "—".to_owned()),
        };
        Row::new(vec![
            Cell::from(ratatui::text::Span::styled(glyph.to_owned(), style)),
            Cell::from(e.kind.clone()),
            Cell::from(title),
            Cell::from(artist),
        ])
        .style(theme.text)
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Fill(3),
            Constraint::Fill(2),
        ],
    )
    .header(Row::new(vec!["", "kind", "title / id", "artist"]).style(theme.dim))
    .row_highlight_style(theme.selected)
    .column_spacing(1);
    f.render_stateful_widget(table, area, &mut app.liked.table);
}
