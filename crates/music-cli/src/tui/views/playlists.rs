//! Playlists section: list pane → detail pane → suggestions pane (the same
//! two-pane pattern as Library, plus a recommender "suggestions" pane). Also
//! renders the two modal overlays — the add-to-playlist picker and the
//! create/rename text prompt.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table};

use super::super::state::{App, PlaylistsPane};
use super::super::theme::Theme;
use super::{TrackRowData, draw_not_ready, track_table};

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    match app.playlists.pane {
        PlaylistsPane::List => draw_list(f, area, app, theme),
        PlaylistsPane::Detail => draw_detail(f, area, app, theme),
        PlaylistsPane::Suggestions => draw_suggestions(f, area, app, theme),
    }
}

fn draw_list(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let [hint_row, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " enter open · N new · a add-track (from a list)",
            theme.dim,
        ))),
        hint_row,
    );

    if draw_not_ready(f, body, theme, &app.playlists.list, "no playlists yet — N to create one", app.tick) {
        return;
    }
    let Some(playlists) = app.playlists.list.ready() else {
        return;
    };

    let rows = playlists.iter().map(|p| {
        let own = if p.owned { "" } else { "shared" };
        Row::new(vec![
            Cell::from(p.name.clone()),
            Cell::from(format!("{}", p.song_count)),
            Cell::from(if p.visibility.is_empty() {
                own.to_owned()
            } else {
                p.visibility.clone()
            }),
        ])
        .style(theme.text)
    });
    let table = Table::new(
        rows,
        [Constraint::Fill(3), Constraint::Length(6), Constraint::Length(8)],
    )
    .header(Row::new(vec!["playlist", "tracks", "vis"]).style(theme.dim))
    .row_highlight_style(theme.selected)
    .column_spacing(1);
    f.render_stateful_widget(table, body, &mut app.playlists.list_table);
}

fn draw_detail(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let [header_row, body] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);

    if let Some(p) = app.playlists.open.ready() {
        let mut spans = vec![Span::styled(p.summary.name.clone(), theme.accent)];
        spans.push(Span::styled(
            format!("  {} track(s)", p.summary.song_count),
            theme.dim,
        ));
        spans.push(Span::styled(
            "   enter play · s shuffle · e enqueue · a add · x remove · m suggest · R rename · X delete · esc back",
            theme.dim,
        ));
        f.render_widget(Paragraph::new(Line::from(spans)), header_row);
    }

    if draw_not_ready(f, body, theme, &app.playlists.open, "", app.tick) {
        return;
    }
    let Some(p) = app.playlists.open.ready() else {
        return;
    };
    if p.tracks.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(" (empty playlist)", theme.dim))),
            body,
        );
        return;
    }

    let current_id = app.queue.current().map(|t| t.id.clone());
    let rows = p.tracks.iter().map(|t| TrackRowData {
        id: t.id.as_str(),
        title: &t.title,
        artist: t.artist_name.as_deref(),
        album: t.album_name.as_deref(),
        duration: t.duration_seconds.map(u64::from).map(std::time::Duration::from_secs),
        is_current: current_id.as_deref() == Some(t.id.as_str()),
    });
    let table = track_table(rows, &app.ratings, theme, true);
    f.render_stateful_widget(table, body, &mut app.playlists.detail_table);
}

fn draw_suggestions(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let [header_row, body] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);

    let name = app.playlists.open.ready().map_or("playlist", |p| p.summary.name.as_str());
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("Suggestions for {name}"), theme.accent),
            Span::styled("   enter add · e enqueue · esc back", theme.dim),
        ])),
        header_row,
    );

    if draw_not_ready(f, body, theme, &app.playlists.suggestions, "", app.tick) {
        return;
    }
    let Some(tracks) = app.playlists.suggestions.ready() else {
        return;
    };
    let current_id = app.queue.current().map(|t| t.id.clone());
    let rows = tracks.iter().map(|t| TrackRowData {
        id: t.id.as_str(),
        title: &t.title,
        artist: t.artist_name.as_deref(),
        album: t.album_name.as_deref(),
        duration: t.duration_seconds.map(u64::from).map(std::time::Duration::from_secs),
        is_current: current_id.as_deref() == Some(t.id.as_str()),
    });
    let table = track_table(rows, &app.ratings, theme, true);
    f.render_stateful_widget(table, body, &mut app.playlists.suggest_table);
}

/// The add-to-playlist picker overlay (owned playlists + a "new" row).
pub(crate) fn draw_picker(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let Some(picker) = &app.picker else {
        return;
    };

    // Rows: each owned playlist, then a synthetic "new playlist…".
    let mut rows: Vec<Row> = Vec::new();
    if let Some(playlists) = app.playlists.list.ready() {
        for p in playlists.iter().filter(|p| p.owned) {
            rows.push(Row::new(vec![
                Cell::from(p.name.clone()),
                Cell::from(format!("{}", p.song_count)),
            ]));
        }
    }
    let list_loading = app.playlists.list.ready().is_none();
    rows.push(Row::new(vec![
        Cell::from(Span::styled("＋ New playlist…", theme.accent)),
        Cell::from(""),
    ]));

    let row_count = rows.len();
    let height = u16::try_from(row_count + 4).unwrap_or(u16::MAX).min(area.height);
    let width = 46.min(area.width);
    let [popup] = Layout::horizontal([Constraint::Length(width)]).flex(Flex::Center).areas(area);
    let [popup] = Layout::vertical([Constraint::Length(height)]).flex(Flex::Center).areas(popup);

    let title = format!(" add “{}” to… ", truncate(&picker.track_title, 24));
    let block = Block::new()
        .title(Span::styled(title, theme.accent))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border);

    f.render_widget(Clear, popup);
    if list_loading && row_count == 1 {
        // List still loading and only the synthetic row is present.
        let inner = block.inner(popup);
        f.render_widget(block, popup);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(" loading playlists…", theme.dim))),
            inner,
        );
        return;
    }
    let table = Table::new(rows, [Constraint::Fill(1), Constraint::Length(6)])
        .block(block)
        .row_highlight_style(theme.selected)
        .column_spacing(1);
    // Clone the selection into a local state so we don't borrow app mutably
    // and immutably at once (the rows already borrowed app.playlists.list).
    let mut ts = ratatui::widgets::TableState::default();
    ts.select(picker.table.selected());
    f.render_stateful_widget(table, popup, &mut ts);
}

/// The create/rename text-entry overlay.
pub(crate) fn draw_text_prompt(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(prompt) = &app.text_prompt else {
        return;
    };
    let width = 46.min(area.width);
    let height = 4.min(area.height);
    let [popup] = Layout::horizontal([Constraint::Length(width)]).flex(Flex::Center).areas(area);
    let [popup] = Layout::vertical([Constraint::Length(height)]).flex(Flex::Center).areas(popup);

    let block = Block::new()
        .title(Span::styled(format!(" {} ", prompt.title), theme.accent))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border);
    let inner = block.inner(popup);
    f.render_widget(Clear, popup);
    f.render_widget(block, popup);

    let [field_row, hint_row] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
    f.render_widget(
        Paragraph::new(prompt.input.line("› ", true, theme)),
        field_row,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled("enter save · esc cancel", theme.dim))),
        hint_row,
    );
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
