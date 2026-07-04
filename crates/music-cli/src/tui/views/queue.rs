//! Queue: the local play queue with the playing marker.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::state::App;
use super::super::theme::Theme;
use super::{TrackRowData, track_table};

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if app.queue.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " queue is empty — enter plays, e enqueues from any list",
                theme.dim,
            ))),
            area,
        );
        return;
    }

    let current = app.queue.current_index();
    let rows = app
        .queue
        .items()
        .iter()
        .enumerate()
        .map(|(i, t)| TrackRowData {
            id: t.id.as_str(),
            title: &t.title,
            artist: t.artist.as_deref(),
            album: t.album.as_deref(),
            duration: t.duration,
            is_current: current == Some(i),
        });
    let table = track_table(rows, &app.ratings, theme, true);
    f.render_stateful_widget(table, area, &mut app.queue_table);
}
