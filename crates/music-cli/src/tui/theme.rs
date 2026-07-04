//! Colors + glyphs. One place to restyle everything; `Theme::detect()`
//! honors `NO_COLOR` with a monochrome variant (the TUI only runs on a TTY,
//! but color-averse users still get structure from emphasis + glyphs).

use ratatui::style::{Color, Modifier, Style};

/// Shipping-crate amber.
const ACCENT: Color = Color::Rgb(230, 126, 34);

#[derive(Debug, Clone, Copy)]
pub(crate) struct Theme {
    pub accent: Style,
    pub text: Style,
    pub dim: Style,
    pub border: Style,
    pub selected: Style,
    pub playing: Style,
    pub error: Style,
    pub like: Style,
    pub dislike: Style,
}

impl Theme {
    pub(crate) fn detect() -> Self {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            Self::monochrome()
        } else {
            Self::default_colors()
        }
    }

    fn default_colors() -> Self {
        Self {
            accent: Style::new().fg(ACCENT),
            text: Style::new().fg(Color::Gray),
            dim: Style::new().fg(Color::DarkGray),
            border: Style::new().fg(Color::DarkGray),
            selected: Style::new()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
            playing: Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            error: Style::new().fg(Color::Red),
            like: Style::new().fg(Color::Red),
            dislike: Style::new().fg(Color::DarkGray),
        }
    }

    fn monochrome() -> Self {
        let plain = Style::new();
        Self {
            accent: plain.add_modifier(Modifier::BOLD),
            text: plain,
            dim: plain.add_modifier(Modifier::DIM),
            border: plain.add_modifier(Modifier::DIM),
            selected: plain.add_modifier(Modifier::REVERSED),
            playing: plain.add_modifier(Modifier::BOLD),
            error: plain.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            like: plain,
            dislike: plain.add_modifier(Modifier::DIM),
        }
    }
}

/// Glyphs, centralized so an ASCII fallback is a constant swap away.
pub(crate) mod symbols {
    pub(crate) const PLAYING: &str = "►";
    pub(crate) const PAUSED: &str = "⏸";
    pub(crate) const LIKE: &str = "♥";
    pub(crate) const DISLIKE: &str = "✖";
    pub(crate) const SECTION_MARKER: &str = "▪";
    pub(crate) const FOCUS: &str = "▸";
    pub(crate) const SPINNER: [&str; 4] = ["⣾", "⣽", "⣻", "⢿"];
}
