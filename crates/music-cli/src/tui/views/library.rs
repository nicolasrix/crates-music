//! Library: albums list with a kind switcher, and the album-detail pane.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, Tabs};

use super::super::state::{ALBUM_KINDS, App, LibraryPane};
use super::super::theme::Theme;
use super::super::widgets::mmss;
use super::{TrackRowData, draw_not_ready, track_table};

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    match app.library.pane {
        LibraryPane::Albums => draw_albums(f, area, app, theme),
        LibraryPane::AlbumDetail => draw_detail(f, area, app, theme),
    }
}

fn draw_albums(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let [tabs_row, body] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);

    let tabs = Tabs::new(ALBUM_KINDS.iter().map(|(_, label)| *label))
        .select(app.library.kind_idx % ALBUM_KINDS.len())
        .style(theme.dim)
        .highlight_style(theme.accent)
        .divider(Span::styled("·", theme.dim));
    f.render_widget(tabs, tabs_row);

    if draw_not_ready(f, body, theme, &app.library.albums, "", app.tick) {
        return;
    }
    let Some(albums) = app.library.albums.ready() else {
        return;
    };

    let rows = albums.iter().map(|a| {
        Row::new(vec![
            Cell::from(a.name.clone()),
            Cell::from(a.artist_name.clone().unwrap_or_else(|| "—".to_owned())),
            Cell::from(a.year.map_or_else(|| "—".to_owned(), |y| y.to_string())),
            Cell::from(format!("{:>2} ✦ {}", a.song_count, mmss(a.duration()))),
        ])
        .style(theme.text)
    });
    let table = Table::new(
        rows,
        [
            Constraint::Fill(3),
            Constraint::Fill(2),
            Constraint::Length(5),
            Constraint::Length(14),
        ],
    )
    .header(
        Row::new(vec!["album", "artist", "year", "tracks"]).style(theme.dim),
    )
    .row_highlight_style(theme.selected)
    .column_spacing(1);
    f.render_stateful_widget(table, body, &mut app.library.albums_table);
}

fn draw_detail(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let [header_row, body] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);

    if let Some(album) = app.library.open_album.ready() {
        let mut spans = vec![Span::styled(album.album.name.clone(), theme.accent)];
        if let Some(artist) = &album.album.artist_name {
            spans.push(Span::styled(format!(" — {artist}"), theme.text));
        }
        if let Some(year) = album.album.year {
            spans.push(Span::styled(format!("  ({year})"), theme.dim));
        }
        spans.push(Span::styled("   esc back", theme.dim));
        f.render_widget(Paragraph::new(Line::from(spans)), header_row);
    }

    if draw_not_ready(f, body, theme, &app.library.open_album, "", app.tick) {
        return;
    }
    let Some(album) = app.library.open_album.ready() else {
        return;
    };

    let current_id = app.queue.current().map(|t| t.id.clone());
    let rows = album.tracks.iter().map(|t| TrackRowData {
        id: t.id.as_str(),
        title: &t.title,
        artist: t.artist_name.as_deref(),
        album: None,
        duration: t.duration_seconds.map(u64::from).map(std::time::Duration::from_secs),
        is_current: current_id.as_deref() == Some(t.id.as_str()),
    });
    let table = track_table(rows, &app.ratings, theme, false);
    f.render_stateful_widget(table, body, &mut app.library.tracks_table);
}
