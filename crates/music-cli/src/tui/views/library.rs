//! Library: a mode switcher (`[`/`]`: albums / artists / tracks) over the
//! browse list, plus the album-detail and artist-detail panes. Both detail
//! panes use a *single* table with a flat selection so the cursor spans
//! tracks + the "you might like" footer (album) or albums + top songs
//! (artist) — see the reducer's `update::library` for the index mapping.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, Tabs};

use super::super::state::{
    ALBUM_KINDS, App, ArtistRow, LIBRARY_MODES, LibraryMode, LibraryPane, SimilarKind,
};
use super::super::theme::{Theme, symbols};
use super::super::widgets::mmss;
use super::{TrackRowData, draw_not_ready, track_table};

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    match app.library.pane {
        LibraryPane::Browse => draw_browse(f, area, app, theme),
        LibraryPane::AlbumDetail => draw_album_detail(f, area, app, theme),
        LibraryPane::ArtistDetail => draw_artist_detail(f, area, app, theme),
    }
}

fn mode_tabs(app: &App, theme: &Theme) -> Tabs<'static> {
    let selected = LIBRARY_MODES
        .iter()
        .position(|(m, _)| *m == app.library.mode)
        .unwrap_or(0);
    Tabs::new(LIBRARY_MODES.iter().map(|(_, label)| *label))
        .select(selected)
        .style(theme.dim)
        .highlight_style(theme.accent)
        .divider(Span::styled("·", theme.dim))
}

fn draw_browse(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let [mode_row, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    f.render_widget(mode_tabs(app, theme), mode_row);

    match app.library.mode {
        LibraryMode::Albums => draw_albums(f, body, app, theme),
        LibraryMode::Artists => draw_artists(f, body, app, theme),
        LibraryMode::Tracks => draw_songs(f, body, app, theme),
    }
}

fn draw_albums(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let [kind_row, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let tabs = Tabs::new(ALBUM_KINDS.iter().map(|(_, label)| *label))
        .select(app.library.kind_idx % ALBUM_KINDS.len())
        .style(theme.dim)
        .highlight_style(theme.accent)
        .divider(Span::styled("·", theme.dim));
    f.render_widget(tabs, kind_row);

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
    .header(Row::new(vec!["album", "artist", "year", "tracks"]).style(theme.dim))
    .row_highlight_style(theme.selected)
    .column_spacing(1);
    f.render_stateful_widget(table, body, &mut app.library.albums_table);
}

fn draw_artists(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if draw_not_ready(f, area, theme, &app.library.artists, "", app.tick) {
        return;
    }
    let Some(artists) = app.library.artists.ready() else {
        return;
    };
    let rows = artists.iter().map(|a| {
        Row::new(vec![
            Cell::from(a.name.clone()),
            Cell::from(
                a.album_count
                    .map_or_else(|| "—".to_owned(), |c| format!("{c} album(s)")),
            ),
        ])
        .style(theme.text)
    });
    let table = Table::new(rows, [Constraint::Fill(3), Constraint::Length(12)])
        .header(Row::new(vec!["artist", ""]).style(theme.dim))
        .row_highlight_style(theme.selected)
        .column_spacing(1);
    f.render_stateful_widget(table, area, &mut app.library.artists_table);
}

fn draw_songs(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if draw_not_ready(f, area, theme, &app.library.songs, "", app.tick) {
        return;
    }
    let Some(songs) = app.library.songs.ready() else {
        return;
    };
    let current_id = app.queue.current().map(|t| t.id.clone());
    let rows = songs.iter().map(|t| TrackRowData {
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
    f.render_stateful_widget(table, area, &mut app.library.songs_table);
}

fn draw_album_detail(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
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
        spans.push(Span::styled("   S station · esc back", theme.dim));
        f.render_widget(Paragraph::new(Line::from(spans)), header_row);
    }

    if draw_not_ready(f, body, theme, &app.library.open_album, "", app.tick) {
        return;
    }
    let Some(album) = app.library.open_album.ready() else {
        return;
    };

    // One combined table: album tracks, then the "you might like" footer,
    // so the cursor's flat index (in `tracks_table`) spans both.
    let current_id = app.queue.current().map(|t| t.id.clone());
    let mut rows: Vec<Row> = album
        .tracks
        .iter()
        .map(|t| track_row(t, current_id.as_deref(), &app.ratings, theme))
        .collect();

    if let Some(similar) = app.library.album_similar.ready() {
        for entry in similar {
            let sub = match entry.kind {
                SimilarKind::Album => entry.artist.clone().unwrap_or_else(|| "album".to_owned()),
                SimilarKind::Artist => "artist".to_owned(),
            };
            rows.push(
                Row::new(vec![
                    Cell::from(Span::styled(symbols::SIMILAR, theme.accent)),
                    Cell::from(Span::styled(entry.name.clone(), theme.accent)),
                    Cell::from(Span::styled(sub, theme.dim)),
                    Cell::from(""),
                ])
                .style(theme.text),
            );
        }
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Fill(3),
            Constraint::Fill(2),
            Constraint::Length(8),
        ],
    )
    .row_highlight_style(theme.selected)
    .column_spacing(1);
    f.render_stateful_widget(table, body, &mut app.library.tracks_table);
}

fn track_row<'a>(
    t: &'a music_core::Track,
    current_id: Option<&str>,
    ratings: &std::collections::HashMap<String, super::super::state::Rating>,
    theme: &Theme,
) -> Row<'a> {
    let is_current = current_id == Some(t.id.as_str());
    let (marker, marker_style) = if is_current {
        (symbols::PLAYING, theme.playing)
    } else {
        match ratings.get(t.id.as_str()) {
            Some(super::super::state::Rating::Like) => (symbols::LIKE, theme.like),
            Some(super::super::state::Rating::Dislike) => (symbols::DISLIKE, theme.dislike),
            None => (" ", theme.text),
        }
    };
    let style = if is_current { theme.playing } else { theme.text };
    Row::new(vec![
        Cell::from(Span::styled(marker.to_owned(), marker_style)),
        Cell::from(t.title.clone()),
        Cell::from(t.artist_name.clone().unwrap_or_else(|| "—".to_owned())),
        Cell::from(
            t.duration_seconds
                .map(u64::from)
                .map(std::time::Duration::from_secs)
                .map_or_else(|| "—".to_owned(), mmss),
        ),
    ])
    .style(style)
}

fn draw_artist_detail(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let [header_row, body] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);

    if let Some(detail) = app.library.open_artist.ready() {
        let songs = detail.rows.len() - detail.albums_len;
        let spans = vec![
            Span::styled(detail.artist.name.clone(), theme.accent),
            Span::styled(
                format!("   {} album(s) · {} top song(s)", detail.albums_len, songs),
                theme.dim,
            ),
            Span::styled("   S station · esc back", theme.dim),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)), header_row);
    }

    if draw_not_ready(f, body, theme, &app.library.open_artist, "", app.tick) {
        return;
    }
    let Some(detail) = app.library.open_artist.ready() else {
        return;
    };

    let current_id = app.queue.current().map(|t| t.id.clone());
    let rows: Vec<Row> = detail
        .rows
        .iter()
        .map(|row| match row {
            ArtistRow::Album(a) => Row::new(vec![
                Cell::from(Span::styled(symbols::ALBUM, theme.dim)),
                Cell::from(a.name.clone()),
                Cell::from(Span::styled(
                    a.year.map_or_else(|| "album".to_owned(), |y| y.to_string()),
                    theme.dim,
                )),
                Cell::from(format!("{} trk", a.song_count)),
            ])
            .style(theme.text),
            ArtistRow::Song(t) => track_row(t, current_id.as_deref(), &app.ratings, theme),
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Fill(3),
            Constraint::Fill(2),
            Constraint::Length(8),
        ],
    )
    .row_highlight_style(theme.selected)
    .column_spacing(1);
    f.render_stateful_widget(table, body, &mut app.library.artist_table);
}
