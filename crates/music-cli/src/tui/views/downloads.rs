//! Downloads (section 7): the two-budget cache gauges + storage totals over a
//! table of pinned tracks. `d` toggles a track's pin from anywhere, `W`
//! bulk-downloads an album/playlist or warms from liked, `E` evicts to budget,
//! `enter` plays a pinned track (offline-capable — the row keys on its id).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Gauge, Paragraph, Row, Table};

use super::super::state::{App, PinnedRow};
use super::super::theme::{Theme, symbols};
use super::super::widgets::human_bytes;
use super::draw_not_ready;

/// Shown both as the empty-`Ready` hint and (harmlessly) as the never-reached
/// `Idle` hint — a single copy of the "how to pin" prompt.
const EMPTY_HINT: &str = "no pinned tracks — press d on a track to save it offline";

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let [top, table_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);
    draw_gauges(f, top, app, theme);
    draw_pinned(f, table_area, app, theme);
}

/// The two budget gauges (regular + pinned) and a totals line.
fn draw_gauges(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if draw_not_ready(
        f,
        area,
        theme,
        &app.downloads.stats,
        "cache totals load when you open Downloads",
        app.tick,
    ) {
        return;
    }
    let Some(s) = app.downloads.stats.ready() else {
        return;
    };
    let [reg, pin, totals] = Layout::vertical([Constraint::Length(1); 3]).areas(area);
    budget_gauge(
        f,
        reg,
        theme,
        "regular",
        s.regular_bytes,
        s.regular_budget_bytes,
        s.regular_count,
    );
    budget_gauge(
        f,
        pin,
        theme,
        "pinned ",
        s.pinned_bytes,
        s.pinned_budget_bytes,
        s.pinned_count,
    );
    let line = format!(
        " {} pinned · {} auto-cached · {} on disk",
        s.pinned_count,
        s.regular_count,
        human_bytes(s.regular_bytes.saturating_add(s.pinned_bytes)),
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(line, theme.dim))),
        totals,
    );
}

fn budget_gauge(
    f: &mut Frame,
    area: Rect,
    theme: &Theme,
    label: &str,
    used: u64,
    budget: u64,
    count: u64,
) {
    #[allow(clippy::cast_precision_loss)] // display ratio; exactness irrelevant
    let ratio = if budget == 0 {
        0.0
    } else {
        (used as f64 / budget as f64).clamp(0.0, 1.0)
    };
    let text = format!(
        "{label}  {} / {} · {count} file(s)",
        human_bytes(used),
        human_bytes(budget),
    );
    let gauge = Gauge::default()
        .ratio(ratio)
        .label(Span::styled(text, theme.text))
        .gauge_style(theme.accent)
        .use_unicode(true);
    f.render_widget(gauge, area);
}

/// The pinned-track table (hydrated title/artist when online, id + size when
/// not).
fn draw_pinned(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if draw_not_ready(f, area, theme, &app.downloads.pinned, EMPTY_HINT, app.tick) {
        return;
    }
    let Some(rows) = app.downloads.pinned.ready() else {
        return;
    };
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {EMPTY_HINT}"), theme.dim))),
            area,
        );
        return;
    }
    let table = pinned_table(rows, theme);
    f.render_stateful_widget(table, area, &mut app.downloads.table);
}

/// Build the pinned-track table: download glyph · title/id · artist · size.
fn pinned_table<'a>(rows: &'a [PinnedRow], theme: &Theme) -> Table<'a> {
    let body = rows.iter().map(|r| {
        let artist = r
            .track
            .as_ref()
            .and_then(|t| t.artist_name.clone())
            .unwrap_or_else(|| "—".to_owned());
        Row::new(vec![
            Cell::from(Span::styled(symbols::DOWNLOAD.to_owned(), theme.accent)),
            Cell::from(r.title_or_id()),
            Cell::from(artist),
            Cell::from(human_bytes(r.bytes)),
        ])
        .style(theme.text)
    });
    Table::new(
        body,
        [
            Constraint::Length(2),
            Constraint::Fill(3),
            Constraint::Fill(2),
            Constraint::Length(10),
        ],
    )
    .header(Row::new(vec!["", "title / id", "artist", "size"]).style(theme.dim))
    .row_highlight_style(theme.selected)
    .column_spacing(1)
}
