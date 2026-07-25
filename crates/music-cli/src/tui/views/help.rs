//! `?` overlay — renders the keymap's own [`KEY_HELP`] table so the help
//! can never drift from the actual bindings.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use super::super::keymap::KEY_HELP;
use super::super::theme::Theme;

pub(crate) fn draw(f: &mut Frame, area: Rect, theme: &Theme) {
    let width = 52.min(area.width);
    let height = u16::try_from(KEY_HELP.len() + 4).unwrap_or(u16::MAX).min(area.height);
    let [popup] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let [popup] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(popup);

    let key_width = KEY_HELP
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = KEY_HELP
        .iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(format!(" {key:key_width$}  "), theme.accent),
                Span::styled(*what, theme.text),
            ])
        })
        .collect();

    let block = Block::new()
        .title(Span::styled(" keys ", theme.accent))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border);
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(lines).block(block), popup);
}
