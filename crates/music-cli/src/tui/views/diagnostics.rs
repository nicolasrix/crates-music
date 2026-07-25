//! The admin-only Diagnostics section (parity plan Phase 9): a sub-tab bar
//! plus one panel per gateway inspector family. The web `/diagnostics` page's
//! waterfall becomes an indented span tree here; its scatter becomes a ratatui
//! braille `Chart`. Every panel renders the four `Loadable` states via the
//! shared [`super::draw_not_ready`] helper.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Color;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Borders, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table,
};

use crate::api::{
    ClientEvent, HistogramBucket, LatentNeighbour, LatentSpace, QueueDepth, RecentlyPlayed,
    RecommenderPanels, TraceEntry,
};
use crate::tui::state::{App, DiagTab, TracingData};
use crate::tui::theme::Theme;

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let [header, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);
    draw_tabs(f, header, app, theme);
    match app.diagnostics.active_tab() {
        DiagTab::Ingest => draw_ingest(f, body, app, theme),
        DiagTab::Recommender => draw_recommender(f, body, app, theme),
        DiagTab::Listening => draw_listening(f, body, app, theme),
        DiagTab::Tracing => draw_tracing(f, body, app, theme),
        DiagTab::LatentSpace => draw_latent(f, body, app, theme),
        DiagTab::ClientEvents => draw_client_events(f, body, app, theme),
    }
}

/// Sub-tab bar + the window selector / key hints.
fn draw_tabs(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let [tabs, hint] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let active = app.diagnostics.tab % DiagTab::ALL.len();
    let mut spans = vec![Span::raw(" ")];
    for (i, t) in DiagTab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", theme.dim));
        }
        let style = if i == active { theme.accent } else { theme.dim };
        spans.push(Span::styled(t.title().to_owned(), style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), tabs);

    let win = app.diagnostics.window;
    let hint_line = Line::from(vec![
        Span::styled(" h/l tab · [ ] window ", theme.dim),
        Span::styled(format!("[{}]", win.label()), theme.accent),
    ]);
    f.render_widget(Paragraph::new(hint_line), hint);
}

// ── ingest ─────────────────────────────────────────────────────────────────

fn draw_ingest(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if super::draw_not_ready(f, area, theme, &app.diagnostics.ingest, "loading…", app.tick) {
        return;
    }
    let q: &QueueDepth = app.diagnostics.ingest.ready().unwrap();
    let total = q.not_started + q.in_progress + q.done + q.failed;
    let lines = vec![
        Line::from(Span::styled(
            format!(" embedding ingest queue · model {}", q.model_version),
            theme.dim,
        )),
        Line::raw(""),
        tile_line("done", q.done, theme.accent, theme),
        tile_line("in progress", q.in_progress, theme.text, theme),
        tile_line("not started", q.not_started, theme.text, theme),
        tile_line("failed", q.failed, theme.error, theme),
        Line::raw(""),
        tile_line("total", total, theme.dim, theme),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn tile_line<'a>(
    label: &'a str,
    value: u64,
    value_style: ratatui::style::Style,
    theme: &Theme,
) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("   {label:>12}  "), theme.dim),
        Span::styled(value.to_string(), value_style),
    ])
}

// ── recommender ──────────────────────────────────────────────────────────

#[allow(clippy::cast_precision_loss)] // bar ratios; exactness irrelevant
fn draw_recommender(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if super::draw_not_ready(
        f,
        area,
        theme,
        &app.diagnostics.recommender,
        "loading…",
        app.tick,
    ) {
        return;
    }
    let p: &RecommenderPanels = app.diagnostics.recommender.ready().unwrap();
    let [fill, sim, short, top] = Layout::vertical([
        Constraint::Length(10),
        Constraint::Length(3),
        Constraint::Length(6),
        Constraint::Min(0),
    ])
    .areas(area);

    // queue-fill histogram (autoplay refill fullness)
    let max = p.queue_fill.buckets.iter().map(|b| b.count).max().unwrap_or(0);
    let mut fill_lines = vec![Line::from(Span::styled(
        format!(" queue fill · {} refills", p.queue_fill.total),
        theme.dim,
    ))];
    for b in &p.queue_fill.buckets {
        let frac = if max == 0 { 0.0 } else { b.count as f64 / max as f64 };
        fill_lines.push(Line::from(vec![
            Span::styled(format!("  {:>8} ", b.label), theme.dim),
            Span::styled(hbar(frac, 24), theme.accent),
            Span::styled(format!(" {}", b.count), theme.text),
        ]));
    }
    f.render_widget(Paragraph::new(fill_lines), fill);

    // similarity quantiles (one line)
    let s = &p.similarity;
    let sim_line = Line::from(vec![
        Span::styled(" similarity ", theme.dim),
        Span::styled(
            format!(
                "n={} · p50 {:.3} · p90 {:.3} · p95 {:.3} · p99 {:.3} · max {:.3}",
                s.count, s.p50, s.p90, s.p95, s.p99, s.max
            ),
            theme.text,
        ),
    ]);
    f.render_widget(Paragraph::new(sim_line), sim);

    // shortfall reasons
    let mut short_lines = vec![Line::from(Span::styled(
        format!(" shortfall · {} under-delivered", p.shortfall.total),
        theme.dim,
    ))];
    if p.shortfall.counts.is_empty() {
        short_lines.push(Line::from(Span::styled("   (none)", theme.dim)));
    }
    for (reason, count) in &p.shortfall.counts {
        short_lines.push(Line::from(vec![
            Span::styled(format!("   {reason:>18}  "), theme.dim),
            Span::styled(count.to_string(), theme.text),
        ]));
    }
    f.render_widget(Paragraph::new(short_lines), short);

    // top-served leaderboard
    let rows = p.top_results.iter().map(|r| {
        Row::new(vec![
            Cell::from(r.count.to_string()),
            Cell::from(r.title.clone().unwrap_or_else(|| r.track_id.clone())),
            Cell::from(r.artist.clone().unwrap_or_else(|| "—".to_owned())),
        ])
        .style(theme.text)
    });
    let table = Table::new(
        rows,
        [Constraint::Length(6), Constraint::Fill(3), Constraint::Fill(2)],
    )
    .header(
        Row::new(vec![
            Cell::from("served"),
            Cell::from("track"),
            Cell::from("artist"),
        ])
        .style(theme.dim),
    )
    .block(Block::new().borders(Borders::TOP).border_style(theme.border));
    f.render_widget(table, top);
}

// ── listening ──────────────────────────────────────────────────────────────

fn draw_listening(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if super::draw_not_ready(
        f,
        area,
        theme,
        &app.diagnostics.listening,
        "loading…",
        app.tick,
    ) {
        return;
    }
    let events: &[RecentlyPlayed] = app.diagnostics.listening.ready().unwrap();
    // Reference clock: the newest event (list is newest-first) → relative ages.
    let now = events.iter().map(|e| e.occurred_at_ms).max().unwrap_or(0);
    // Repeat count: how many times each track appears in the window.
    let mut plays: HashMap<&str, u32> = HashMap::new();
    for e in events {
        *plays.entry(e.track_id.as_str()).or_default() += 1;
    }
    let rows = events.iter().map(|e| {
        let n = plays.get(e.track_id.as_str()).copied().unwrap_or(1);
        Row::new(vec![
            Cell::from(ago(now - e.occurred_at_ms)),
            Cell::from(e.title.clone().unwrap_or_else(|| e.track_id.clone())),
            Cell::from(e.artist.clone().unwrap_or_else(|| "—".to_owned())),
            Cell::from(if n > 1 { format!("×{n}") } else { String::new() }),
        ])
        .style(theme.text)
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Fill(3),
            Constraint::Fill(2),
            Constraint::Length(5),
        ],
    )
    .header(
        Row::new(vec![
            Cell::from("when"),
            Cell::from("track"),
            Cell::from("artist"),
            Cell::from("rpt"),
        ])
        .style(theme.dim),
    )
    .row_highlight_style(theme.selected);
    f.render_stateful_widget(table, area, &mut app.diagnostics.listening_table);
}

// ── tracing ────────────────────────────────────────────────────────────────

#[allow(clippy::cast_precision_loss)] // bar ratios; exactness irrelevant
fn draw_tracing(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if super::draw_not_ready(f, area, theme, &app.diagnostics.tracing, "loading…", app.tick) {
        return;
    }
    let [hist_area, tree_area] =
        Layout::vertical([Constraint::Length(10), Constraint::Min(0)]).areas(area);
    let data: &TracingData = app.diagnostics.tracing.ready().unwrap();

    // histogram (non-navigable, top)
    let max_p99 = data.histogram.iter().map(|b| b.p99_ms).max().unwrap_or(1).max(1);
    let hist_rows = data.histogram.iter().map(|b: &HistogramBucket| {
        let frac = b.p99_ms as f64 / max_p99 as f64;
        Row::new(vec![
            Cell::from(b.name.clone()),
            Cell::from(b.count.to_string()),
            Cell::from(format!("{}/{}/{}", b.p50_ms, b.p95_ms, b.p99_ms)),
            Cell::from(hbar(frac, 16)),
        ])
        .style(theme.text)
    });
    let hist = Table::new(
        hist_rows,
        [
            Constraint::Fill(2),
            Constraint::Length(6),
            Constraint::Length(14),
            Constraint::Length(18),
        ],
    )
    .header(
        Row::new(vec![
            Cell::from("span"),
            Cell::from("count"),
            Cell::from("p50/95/99"),
            Cell::from("p99"),
        ])
        .style(theme.dim),
    );
    f.render_widget(hist, hist_area);

    // span tree (navigable, bottom) — traces are pre-sorted (trace_id,start_ms).
    let max_dur = data.traces.iter().map(|t| t.duration_ms).max().unwrap_or(1).max(1);
    let rows = data.traces.iter().map(|t: &TraceEntry| {
        let indent = if t.parent_span_id.is_some() { "  └ " } else { "" };
        let frac = t.duration_ms as f64 / max_dur as f64;
        Row::new(vec![
            Cell::from(format!("{indent}{}", t.name)),
            Cell::from(hbar(frac, 14)),
            Cell::from(format!("{} ms", t.duration_ms)),
        ])
        .style(theme.text)
    });
    let tree = Table::new(
        rows,
        [Constraint::Fill(3), Constraint::Length(16), Constraint::Length(10)],
    )
    .header(
        Row::new(vec![Cell::from("span"), Cell::from("duration"), Cell::from("")])
            .style(theme.dim),
    )
    .block(Block::new().borders(Borders::TOP).border_style(theme.border))
    .row_highlight_style(theme.selected);
    f.render_stateful_widget(tree, tree_area, &mut app.diagnostics.tracing_table);
}

// ── latent space ─────────────────────────────────────────────────────────

/// Distinct colours for the genre buckets (points with no genre fall to dim).
const GENRE_COLORS: [Color; 8] = [
    Color::Cyan,
    Color::Green,
    Color::Yellow,
    Color::Magenta,
    Color::Blue,
    Color::LightRed,
    Color::LightGreen,
    Color::LightMagenta,
];

fn draw_latent(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let d = &app.diagnostics;
    let [chart_area, side] =
        Layout::horizontal([Constraint::Min(24), Constraint::Length(34)]).areas(area);
    if super::draw_not_ready(f, chart_area, theme, &d.latent, "loading…", app.tick) {
        return;
    }
    let space: &LatentSpace = d.latent.ready().unwrap();
    if space.points.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " no 2-D projection yet — run the latent-space projection job on the gateway",
                theme.dim,
            )))
            .wrap(ratatui::widgets::Wrap { trim: true }),
            chart_area,
        );
        return;
    }

    // Axis bounds from the data, padded a touch so edge points aren't clipped.
    let (mut xmin, mut xmax, mut ymin, mut ymax) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for p in &space.points {
        xmin = xmin.min(p.x);
        xmax = xmax.max(p.x);
        ymin = ymin.min(p.y);
        ymax = ymax.max(p.y);
    }
    let xpad = ((xmax - xmin) * 0.05).max(0.01);
    let ypad = ((ymax - ymin) * 0.05).max(0.01);
    let (xmin, xmax, ymin, ymax) = (xmin - xpad, xmax + xpad, ymin - ypad, ymax + ypad);

    // Bucket points by genre (stable colour per distinct genre).
    let mut genre_order: Vec<&str> = Vec::new();
    let mut buckets: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut untyped: Vec<(f64, f64)> = Vec::new();
    for p in &space.points {
        match p.genre.as_deref().filter(|g| !g.is_empty()) {
            Some(g) => {
                let idx = genre_order.iter().position(|x| *x == g).unwrap_or_else(|| {
                    genre_order.push(g);
                    buckets.push(Vec::new());
                    genre_order.len() - 1
                });
                buckets[idx].push((p.x, p.y));
            }
            None => untyped.push((p.x, p.y)),
        }
    }
    // The selected point as its own single-item highlight dataset.
    let selected: Vec<(f64, f64)> = d
        .latent_table
        .selected()
        .and_then(|s| space.points.get(s))
        .map(|p| vec![(p.x, p.y)])
        .unwrap_or_default();

    let mut datasets: Vec<Dataset> = Vec::new();
    if !untyped.is_empty() {
        datasets.push(scatter(&untyped, Color::DarkGray));
    }
    for (i, data) in buckets.iter().enumerate() {
        datasets.push(scatter(data, GENRE_COLORS[i % GENRE_COLORS.len()]));
    }
    if !selected.is_empty() {
        datasets.push(scatter(&selected, Color::White));
    }

    let chart = Chart::new(datasets)
        .x_axis(Axis::default().bounds([xmin, xmax]).style(theme.dim))
        .y_axis(Axis::default().bounds([ymin, ymax]).style(theme.dim))
        .block(
            Block::new()
                .borders(Borders::RIGHT)
                .border_style(theme.border),
        );
    f.render_widget(chart, chart_area);

    draw_latent_side(f, side, app, theme);
}

fn scatter(data: &[(f64, f64)], color: Color) -> Dataset<'_> {
    Dataset::default()
        .marker(ratatui::symbols::Marker::Braille)
        .graph_type(GraphType::Scatter)
        .style(ratatui::style::Style::default().fg(color))
        .data(data)
}

/// Selected-point info + its nearest-neighbour side list.
fn draw_latent_side(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let d = &app.diagnostics;
    let [info, list] =
        Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).areas(area);

    let sel = d.latent_table.selected().and_then(|s| {
        d.latent.ready().and_then(|sp| sp.points.get(s))
    });
    let info_lines = match sel {
        Some(p) => vec![
            Line::from(Span::styled(" selected", theme.dim)),
            Line::from(Span::styled(
                format!("  {}", p.title.clone().unwrap_or_else(|| p.track_id.clone())),
                theme.text,
            )),
            Line::from(Span::styled(
                format!("  {}", p.artist.clone().unwrap_or_else(|| "—".to_owned())),
                theme.dim,
            )),
            Line::from(Span::styled(
                format!("  genre: {}", p.genre.clone().unwrap_or_else(|| "—".to_owned())),
                theme.dim,
            )),
        ],
        None => vec![Line::from(Span::styled(
            " j/k select a point · enter plays",
            theme.dim,
        ))],
    };
    f.render_widget(Paragraph::new(info_lines), info);

    if super::draw_not_ready(f, list, theme, &d.neighbours, "neighbours…", app.tick) {
        return;
    }
    let neigh: &[LatentNeighbour] = d.neighbours.ready().unwrap();
    let rows = neigh.iter().map(|n| {
        Row::new(vec![
            Cell::from(format!("{:.3}", n.cosine_distance)),
            Cell::from(n.title.clone().unwrap_or_else(|| n.track_id.clone())),
        ])
        .style(theme.text)
    });
    let table = Table::new(rows, [Constraint::Length(6), Constraint::Fill(1)]).header(
        Row::new(vec![Cell::from("dist"), Cell::from("neighbour")]).style(theme.dim),
    );
    f.render_widget(table, list);
}

// ── client events (RUM) ────────────────────────────────────────────────────

fn draw_client_events(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if super::draw_not_ready(
        f,
        area,
        theme,
        &app.diagnostics.client_events,
        "loading…",
        app.tick,
    ) {
        return;
    }
    let events: &[ClientEvent] = app.diagnostics.client_events.ready().unwrap();
    let now = events.iter().map(|e| e.received_ms).max().unwrap_or(0);
    let rows = events.iter().map(|e| {
        let val = e
            .value_ms
            .map(|v| format!("{v:.0} ms"))
            .or_else(|| e.rating.clone())
            .unwrap_or_default();
        Row::new(vec![
            Cell::from(ago(now - e.received_ms)),
            Cell::from(e.name.clone()),
            Cell::from(val),
            Cell::from(e.page_path.clone()),
        ])
        .style(theme.text)
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Fill(2),
            Constraint::Length(10),
            Constraint::Fill(2),
        ],
    )
    .header(
        Row::new(vec![
            Cell::from("when"),
            Cell::from("event"),
            Cell::from("value"),
            Cell::from("page"),
        ])
        .style(theme.dim),
    )
    .row_highlight_style(theme.selected);
    f.render_stateful_widget(table, area, &mut app.diagnostics.client_events_table);
}

// ── shared helpers ─────────────────────────────────────────────────────────

/// A unicode fill bar of `width` cells at `frac` (0..=1) full.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)] // display bar; exactness irrelevant
fn hbar(frac: f64, width: usize) -> String {
    let filled = (frac.clamp(0.0, 1.0) * width as f64).round() as usize;
    let mut s = "█".repeat(filled);
    s.push_str(&"░".repeat(width.saturating_sub(filled)));
    s
}

/// Compact "time ago" from a positive millisecond delta.
fn ago(delta_ms: i64) -> String {
    let secs = (delta_ms.max(0)) / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{LatentPoint, LatentSpace};
    use crate::tui::state::{Loadable, Section};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn draw_active(app: &mut App) {
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        let theme = Theme::detect();
        term.draw(|f| {
            let area = f.area();
            draw(f, area, app, &theme);
        })
        .unwrap();
    }

    #[test]
    fn every_tab_renders_without_panic_when_idle() {
        let mut app = App::new(None, false, true);
        app.section = Section::Diagnostics;
        for i in 0..DiagTab::ALL.len() {
            app.diagnostics.tab = i;
            draw_active(&mut app);
        }
        // And on a tiny terminal (layout math must not underflow).
        let backend = TestBackend::new(18, 5);
        let mut tiny = Terminal::new(backend).unwrap();
        let theme = Theme::detect();
        tiny.draw(|f| {
            let area = f.area();
            draw(f, area, &mut app, &theme);
        })
        .unwrap();
    }

    #[test]
    fn latent_scatter_renders_with_points() {
        let mut app = App::new(None, false, true);
        app.section = Section::Diagnostics;
        app.diagnostics.tab = DiagTab::ALL
            .iter()
            .position(|t| *t == DiagTab::LatentSpace)
            .unwrap();
        app.diagnostics.latent = Loadable::Ready(LatentSpace {
            model_version: "m".to_owned(),
            proj_version: Some("m-2d".to_owned()),
            points: vec![
                LatentPoint {
                    track_id: "t1".to_owned(),
                    x: -1.0,
                    y: 0.5,
                    title: Some("A".to_owned()),
                    artist: Some("X".to_owned()),
                    album: None,
                    genre: Some("Jazz".to_owned()),
                },
                LatentPoint {
                    track_id: "t2".to_owned(),
                    x: 2.0,
                    y: -1.5,
                    title: Some("B".to_owned()),
                    artist: None,
                    album: None,
                    genre: None,
                },
            ],
        });
        app.diagnostics.latent_table.select(Some(0));
        draw_active(&mut app);
    }
}
