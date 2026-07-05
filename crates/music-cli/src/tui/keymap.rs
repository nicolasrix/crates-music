//! Key → semantic-`Msg` translation. Pure over `(App, KeyEvent)` so the
//! whole dispatch table is unit-testable; the help overlay renders
//! [`KEY_HELP`] so the docs can never drift from the bindings.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::msg::{InputMsg, Msg};
use super::state::{App, Overlay, Rating, Section};

/// Rendered by the help overlay — keep in sync with `action_for` (it *is*
/// the documentation of that function).
pub(crate) const KEY_HELP: &[(&str, &str)] = &[
    ("q / ctrl-c", "quit"),
    ("?", "help"),
    ("1..5 / tab", "switch section"),
    ("j k / ↓ ↑", "move cursor"),
    ("g / G", "top / bottom"),
    ("ctrl-d / ctrl-u", "half-page down / up"),
    ("h / l", "album list kind (library)"),
    ("enter", "open album · play from here · jump"),
    ("e", "enqueue track / album"),
    ("/", "search"),
    ("i", "edit station prompt"),
    ("esc", "back / unfocus"),
    ("space", "play / pause"),
    ("n / p", "next / previous track"),
    (", / .", "seek -10s / +10s"),
    ("- / =", "volume down / up"),
    ("L / D / u", "like / dislike / unrate"),
    ("r", "recommend from now playing → queue"),
    ("P", "play next (queue: move after current)"),
    ("x / c", "queue: remove / clear upcoming"),
    ("J / K", "queue: move row down / up"),
    ("T", "queue: move row to top"),
    ("o", "toggle audio output on this device (sync)"),
];

pub(crate) fn action_for(app: &App, key: KeyEvent) -> Option<Msg> {
    // Ctrl-C always quits, even mid-typing.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Msg::Quit);
    }

    // Help overlay swallows everything; any close-ish key dismisses it.
    if app.overlay == Overlay::Help {
        return match key.code {
            KeyCode::Esc | KeyCode::Char('q' | '?') | KeyCode::Enter => Some(Msg::Back),
            _ => None,
        };
    }

    // Focused text input: keys type instead of acting.
    if app.input_focused() {
        return match key.code {
            KeyCode::Esc => Some(Msg::Back),
            KeyCode::Enter => Some(Msg::SubmitInput),
            KeyCode::Backspace => Some(Msg::Input(InputMsg::Backspace)),
            KeyCode::Delete => Some(Msg::Input(InputMsg::Delete)),
            KeyCode::Left => Some(Msg::Input(InputMsg::Left)),
            KeyCode::Right => Some(Msg::Input(InputMsg::Right)),
            KeyCode::Home => Some(Msg::Input(InputMsg::Home)),
            KeyCode::End => Some(Msg::Input(InputMsg::End)),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Msg::Input(InputMsg::Char(c)))
            }
            _ => None,
        };
    }

    // List navigation with Ctrl held (half-paging) — checked before the
    // plain-char table so 'd'/'u' with Ctrl don't hit other bindings.
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('d') => Some(Msg::NavHalfPageDown),
            KeyCode::Char('u') => Some(Msg::NavHalfPageUp),
            _ => None,
        };
    }

    match key.code {
        KeyCode::Char('q') => Some(Msg::Quit),
        KeyCode::Char('?') => Some(Msg::ToggleHelp),
        KeyCode::Esc => Some(Msg::Back),
        KeyCode::Tab => Some(Msg::NextSection),
        KeyCode::BackTab => Some(Msg::PrevSection),
        KeyCode::Char(c @ '1'..='5') => {
            let idx = (c as usize) - ('1' as usize);
            Section::ALL.get(idx).copied().map(Msg::GoSection)
        }
        KeyCode::Char('/') => Some(Msg::FocusSearch),
        KeyCode::Char('i') => Some(Msg::FocusInput),
        KeyCode::Char('j') | KeyCode::Down => Some(Msg::NavDown),
        KeyCode::Char('k') | KeyCode::Up => Some(Msg::NavUp),
        KeyCode::Char('g') => Some(Msg::NavTop),
        KeyCode::Char('G') => Some(Msg::NavBottom),
        KeyCode::Char('h') | KeyCode::Left => Some(Msg::CycleKindPrev),
        KeyCode::Char('l') | KeyCode::Right => Some(Msg::CycleKindNext),
        KeyCode::Enter => Some(Msg::Activate),
        KeyCode::Char('e') => Some(Msg::Enqueue),
        KeyCode::Char(' ') => Some(Msg::TransportToggle),
        KeyCode::Char('n') => Some(Msg::TransportNext),
        KeyCode::Char('p') => Some(Msg::TransportPrev),
        KeyCode::Char(',') => Some(Msg::SeekBy(-10)),
        KeyCode::Char('.') => Some(Msg::SeekBy(10)),
        KeyCode::Char('-') => Some(Msg::VolumeBy(-0.05)),
        KeyCode::Char('=' | '+') => Some(Msg::VolumeBy(0.05)),
        KeyCode::Char('L') => Some(Msg::Rate(Some(Rating::Like))),
        KeyCode::Char('D') => Some(Msg::Rate(Some(Rating::Dislike))),
        KeyCode::Char('u') => Some(Msg::Rate(None)),
        KeyCode::Char('r') => Some(Msg::RecommendFromNowPlaying),
        // Play-next works on any track row (moves within the queue view).
        KeyCode::Char('P') => Some(Msg::PlayNext),
        // Output toggle only matters with a live sync room, but it's
        // harmless (and self-explaining) elsewhere.
        KeyCode::Char('o') => Some(Msg::ToggleOutput),
        // Queue edits only bind inside the queue view — 'x'/'c' are too
        // destructive to be global, and J/K/T would shadow navigation.
        KeyCode::Char('x') if app.section == Section::Queue => Some(Msg::QueueRemoveSelected),
        KeyCode::Char('c') if app.section == Section::Queue => Some(Msg::QueueClear),
        KeyCode::Char('J') if app.section == Section::Queue => Some(Msg::QueueMoveDown),
        KeyCode::Char('K') if app.section == Section::Queue => Some(Msg::QueueMoveUp),
        KeyCode::Char('T') if app.section == Section::Queue => Some(Msg::QueueMoveTop),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn app() -> App {
        App::new(None, false, true)
    }

    #[test]
    fn space_toggles_transport_when_no_input_focused() {
        let a = app();
        assert!(matches!(
            action_for(&a, key(KeyCode::Char(' '))),
            Some(Msg::TransportToggle)
        ));
    }

    #[test]
    fn space_types_into_focused_search_input() {
        let mut a = app();
        a.section = Section::Search;
        a.search.focused = true;
        assert!(matches!(
            action_for(&a, key(KeyCode::Char(' '))),
            Some(Msg::Input(InputMsg::Char(' ')))
        ));
        // and 'q' types too, instead of quitting
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('q'))),
            Some(Msg::Input(InputMsg::Char('q')))
        ));
    }

    #[test]
    fn ctrl_c_quits_even_while_typing() {
        let mut a = app();
        a.section = Section::Search;
        a.search.focused = true;
        assert!(matches!(action_for(&a, ctrl('c')), Some(Msg::Quit)));
    }

    #[test]
    fn number_keys_jump_sections() {
        let a = app();
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('3'))),
            Some(Msg::GoSection(Section::Queue))
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('5'))),
            Some(Msg::GoSection(Section::Liked))
        ));
    }

    #[test]
    fn queue_edit_keys_only_bind_in_queue_section() {
        let mut a = app();
        assert!(action_for(&a, key(KeyCode::Char('x'))).is_none());
        assert!(action_for(&a, key(KeyCode::Char('c'))).is_none());
        a.section = Section::Queue;
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('x'))),
            Some(Msg::QueueRemoveSelected)
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('c'))),
            Some(Msg::QueueClear)
        ));
    }

    #[test]
    fn help_overlay_swallows_and_dismisses() {
        let mut a = app();
        a.overlay = Overlay::Help;
        assert!(matches!(action_for(&a, key(KeyCode::Esc)), Some(Msg::Back)));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('?'))),
            Some(Msg::Back)
        ));
        // arbitrary other keys do nothing while help is up
        assert!(action_for(&a, key(KeyCode::Char('n'))).is_none());
    }

    #[test]
    fn half_page_uses_ctrl() {
        let a = app();
        assert!(matches!(action_for(&a, ctrl('d')), Some(Msg::NavHalfPageDown)));
        assert!(matches!(action_for(&a, ctrl('u')), Some(Msg::NavHalfPageUp)));
        // plain 'u' is unrate, not paging
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('u'))),
            Some(Msg::Rate(None))
        ));
    }

    #[test]
    fn ratings_and_transport_bindings() {
        let a = app();
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('L'))),
            Some(Msg::Rate(Some(Rating::Like)))
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('D'))),
            Some(Msg::Rate(Some(Rating::Dislike)))
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char(','))),
            Some(Msg::SeekBy(-10))
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('='))),
            Some(Msg::VolumeBy(_))
        ));
    }
}
