//! Stations: natural-language prompt → ranked tracks from the recommender.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::state::App;
use super::super::theme::Theme;
use super::{TrackRowData, draw_not_ready, track_table};

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let [input_row, hint_row, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);

    f.render_widget(
        Paragraph::new(
            app.stations
                .input
                .line(" station ▸ ", app.stations.focused, theme),
        ),
        input_row,
    );
    let hint = if app.stations.last_prompt.is_empty() {
        Line::from(Span::styled(
            " describe a mood or genre — \"rainy sunday\", \"boom bap hip hop\"",
            theme.dim,
        ))
    } else {
        Line::from(Span::styled(
            format!(" playing on: {}", app.stations.last_prompt),
            theme.dim,
        ))
    };
    f.render_widget(Paragraph::new(hint), hint_row);

    if draw_not_ready(f, body, theme, &app.stations.results, "", app.tick) {
        return;
    }
    let Some(tracks) = app.stations.results.ready() else {
        return;
    };

    let current_id = app.queue.current().map(|t| t.id.clone());
    let rows = tracks.iter().map(|t| TrackRowData {
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
    f.render_stateful_widget(table, body, &mut app.stations.table);
}
