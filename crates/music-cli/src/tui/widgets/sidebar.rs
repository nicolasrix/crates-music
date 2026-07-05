//! Section list on the left: `1 Library`, `2 Search`, … with a focus marker
//! on the active one.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::super::state::{App, Section, SyncPhase};
use super::super::theme::{Theme, symbols};

pub(crate) fn draw(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let lines: Vec<Line> = Section::ALL
        .iter()
        .map(|s| {
            let active = *s == app.section;
            let marker = if active { symbols::FOCUS } else { " " };
            let style = if active { theme.accent } else { theme.dim };
            Line::from(vec![
                Span::styled(format!(" {marker} "), style),
                Span::styled(format!("{} ", s.index() + 1), theme.dim),
                Span::styled(s.title(), style),
            ])
        })
        .collect();

    let block = Block::new()
        .borders(Borders::RIGHT)
        .border_style(theme.border);
    f.render_widget(Paragraph::new(lines).block(block), area);

    // Offline badge pinned to the bottom interior row (gateway configured but
    // the sync WS isn't delivering) — a reminder that pinned tracks still
    // play. Direct/online modes show nothing here.
    if app.sync.phase == SyncPhase::Offline && area.height > 0 {
        let badge = Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width.saturating_sub(1),
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {} offline", symbols::OFFLINE),
                theme.error,
            ))),
            badge,
        );
    }
}
