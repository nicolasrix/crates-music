//! Top-level frame layout: header / (sidebar + view) / now-playing bar,
//! plus overlays.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::state::{App, LibraryPane, Overlay, Section};
use super::theme::{Theme, symbols};
use super::{views, widgets};

pub(crate) fn draw(f: &mut Frame, app: &mut App, theme: &Theme) {
    let [header, body, bar] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .areas(f.area());

    draw_header(f, header, app, theme);

    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(14), Constraint::Min(0)]).areas(body);
    widgets::sidebar::draw(f, sidebar, app, theme);

    match app.section {
        Section::Library => views::library::draw(f, main, app, theme),
        Section::Search => views::search::draw(f, main, app, theme),
        Section::Queue => views::queue::draw(f, main, app, theme),
        Section::Stations => views::stations::draw(f, main, app, theme),
        Section::Liked => views::liked::draw(f, main, app, theme),
    }

    widgets::now_playing::draw(f, bar, app, theme);

    if app.overlay == Overlay::Help {
        views::help::draw(f, f.area(), theme);
    }
}

fn draw_header(f: &mut Frame, area: ratatui::layout::Rect, app: &App, theme: &Theme) {
    let crumb = breadcrumb(app);
    let left = format!(" {} crates", symbols::SECTION_MARKER);
    let pad = usize::from(area.width)
        .saturating_sub(left.chars().count() + crumb.chars().count() + 1);
    let line = Line::from(vec![
        Span::styled(left, theme.accent),
        Span::raw(" ".repeat(pad)),
        Span::styled(crumb, theme.dim),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn breadcrumb(app: &App) -> String {
    match app.section {
        Section::Library => match app.library.pane {
            LibraryPane::Albums => format!(
                "library ▸ {}",
                super::state::ALBUM_KINDS[app.library.kind_idx
                    % super::state::ALBUM_KINDS.len()]
                .1
            ),
            LibraryPane::AlbumDetail => {
                let name = app
                    .library
                    .open_album
                    .ready()
                    .map_or("…", |a| a.album.name.as_str());
                format!("library ▸ {name}")
            }
        },
        Section::Search => "search".to_owned(),
        Section::Queue => format!("queue ▸ {} track(s)", app.queue.len()),
        Section::Stations => "stations".to_owned(),
        Section::Liked => "liked".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Smoke test: an empty App renders without panicking at a normal size.
    #[test]
    fn draw_empty_app_does_not_panic() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(None, false);
        let theme = Theme::detect();
        terminal.draw(|f| draw(f, &mut app, &theme)).unwrap();
        // And with the help overlay + a tiny terminal.
        app.overlay = Overlay::Help;
        terminal.draw(|f| draw(f, &mut app, &theme)).unwrap();
        let backend = TestBackend::new(20, 6);
        let mut tiny = Terminal::new(backend).unwrap();
        tiny.draw(|f| draw(f, &mut app, &theme)).unwrap();
    }
}
