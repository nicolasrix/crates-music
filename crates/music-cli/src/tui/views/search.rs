//! Search: query input (gateway typo-tolerant search), three result
//! buckets cycled with h/l.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Span;
use ratatui::widgets::{Cell, Paragraph, Row, Table, Tabs};

use super::super::state::{App, SearchBucket};
use super::super::theme::Theme;
use super::{TrackRowData, draw_not_ready, track_table};

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let [input_row, tabs_row, body] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .areas(area);

    f.render_widget(
        Paragraph::new(
            app.search
                .input
                .line(" search ▸ ", app.search.focused, theme),
        ),
        input_row,
    );

    let counts = app
        .search
        .results
        .ready()
        .map_or([0, 0, 0], |r| [r.tracks.len(), r.albums.len(), r.artists.len()]);
    let labels = ["tracks", "albums", "artists"];
    let tabs = Tabs::new(
        labels
            .iter()
            .zip(counts)
            .map(|(l, c)| format!("{l} ({c})")),
    )
    .select(app.search.bucket % 3)
    .style(theme.dim)
    .highlight_style(theme.accent)
    .divider(Span::styled("·", theme.dim));
    f.render_widget(tabs, tabs_row);

    if draw_not_ready(
        f,
        body,
        theme,
        &app.search.results,
        "type / then a query — typos are fine",
        app.tick,
    ) {
        return;
    }
    let Some(results) = app.search.results.ready() else {
        return;
    };

    let idx = app.search.bucket % 3;
    match app.search.bucket() {
        SearchBucket::Tracks => {
            let current_id = app.queue.current().map(|t| t.id.clone());
            let rows = results.tracks.iter().map(|t| TrackRowData {
                id: t.id.as_str(),
                title: &t.title,
                artist: t.artist_name.as_deref(),
                album: t.album_name.as_deref(),
                duration: t
                    .duration_seconds
                    .map(u64::from)
                    .map(std::time::Duration::from_secs),
                is_current: current_id.as_deref() == Some(t.id.as_str()),
            });
            let table = track_table(rows, &app.ratings, theme, true);
            f.render_stateful_widget(table, body, &mut app.search.tables[idx]);
        }
        SearchBucket::Albums => {
            let rows = results.albums.iter().map(|a| {
                Row::new(vec![
                    Cell::from(a.name.clone()),
                    Cell::from(a.artist_name.clone().unwrap_or_else(|| "—".to_owned())),
                    Cell::from(a.year.map_or_else(|| "—".to_owned(), |y| y.to_string())),
                ])
                .style(theme.text)
            });
            let table = Table::new(
                rows,
                [Constraint::Fill(3), Constraint::Fill(2), Constraint::Length(5)],
            )
            .row_highlight_style(theme.selected);
            f.render_stateful_widget(table, body, &mut app.search.tables[idx]);
        }
        SearchBucket::Artists => {
            let rows = results.artists.iter().map(|a| {
                Row::new(vec![
                    Cell::from(a.name.clone()),
                    Cell::from(
                        a.album_count
                            .map_or_else(String::new, |c| format!("{c} albums")),
                    ),
                ])
                .style(theme.text)
            });
            let table = Table::new(rows, [Constraint::Fill(3), Constraint::Length(12)])
                .row_highlight_style(theme.selected);
            f.render_stateful_widget(table, body, &mut app.search.tables[idx]);
        }
    }
}
