//! Key → semantic-`Msg` translation. Pure over `(App, KeyEvent)` so the
//! whole dispatch table is unit-testable; the help overlay renders
//! [`KEY_HELP`] so the docs can never drift from the bindings.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::msg::{InputMsg, Msg};
use super::state::{App, FeedbackVote, LibraryPane, Overlay, Rating, Section};

/// Rendered by the help overlay — keep in sync with `action_for` (it *is*
/// the documentation of that function).
pub(crate) const KEY_HELP: &[(&str, &str)] = &[
    ("q / ctrl-c", "quit"),
    ("?", "help"),
    ("1..9 / tab", "switch section"),
    ("j k / ↓ ↑", "move cursor"),
    ("g / G", "top / bottom"),
    ("ctrl-d / ctrl-u", "half-page down / up"),
    ("[ / ]", "library mode: albums / artists / tracks"),
    ("h / l", "album kind (albums mode) · result bucket (search)"),
    ("S", "station from album / artist (in detail pane)"),
    ("enter", "open · play from here · jump"),
    ("e", "enqueue track / album"),
    ("a", "add track to a playlist"),
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
    ("A", "toggle autoplay (keep the queue topped up)"),
    ("f / F", "autoplay pick: thumbs up / down"),
    ("d", "save track offline (toggle pin)"),
    ("W", "download album / playlist · warm from liked (downloads)"),
    ("E", "downloads: evict regular cache to budget"),
    ("N", "playlists: new playlist"),
    ("s / m", "playlist: shuffle-play / suggest more"),
    ("R / X", "playlist: rename / delete"),
    ("x", "playlist: remove selected track"),
    ("enter / h l", "settings: change · adjust value"),
];

// Overlay guards + a flat key table; the length is the keymap's, not the
// logic's — each arm is a one-liner.
#[allow(clippy::too_many_lines)]
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

    // Playlist picker overlay: navigate + activate a row, or dismiss.
    if app.overlay == Overlay::PlaylistPicker {
        return match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Some(Msg::PickerClose),
            KeyCode::Enter => Some(Msg::PickerActivate),
            KeyCode::Char('j') | KeyCode::Down => Some(Msg::PickerMove(1)),
            KeyCode::Char('k') | KeyCode::Up => Some(Msg::PickerMove(-1)),
            _ => None,
        };
    }

    // Text-prompt overlay: keys type into the field (like a focused input).
    if app.overlay == Overlay::TextPrompt {
        return match key.code {
            KeyCode::Esc => Some(Msg::PromptClose),
            KeyCode::Enter => Some(Msg::PromptSubmit),
            KeyCode::Backspace => Some(Msg::PromptInput(InputMsg::Backspace)),
            KeyCode::Delete => Some(Msg::PromptInput(InputMsg::Delete)),
            KeyCode::Left => Some(Msg::PromptInput(InputMsg::Left)),
            KeyCode::Right => Some(Msg::PromptInput(InputMsg::Right)),
            KeyCode::Home => Some(Msg::PromptInput(InputMsg::Home)),
            KeyCode::End => Some(Msg::PromptInput(InputMsg::End)),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Msg::PromptInput(InputMsg::Char(c)))
            }
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
        KeyCode::Char(c @ '1'..='9') => {
            let idx = (c as usize) - ('1' as usize);
            // Gate the digit through visibility so a non-admin's "9"
            // (Diagnostics) is inert rather than jumping to a hidden section.
            Section::ALL
                .get(idx)
                .copied()
                .filter(|s| app.is_section_visible(*s))
                .map(Msg::GoSection)
        }
        KeyCode::Char('/') => Some(Msg::FocusSearch),
        KeyCode::Char('i') => Some(Msg::FocusInput),
        KeyCode::Char('j') | KeyCode::Down => Some(Msg::NavDown),
        KeyCode::Char('k') | KeyCode::Up => Some(Msg::NavUp),
        KeyCode::Char('g') => Some(Msg::NavTop),
        KeyCode::Char('G') => Some(Msg::NavBottom),
        KeyCode::Char('h') | KeyCode::Left => Some(Msg::CycleKindPrev),
        KeyCode::Char('l') | KeyCode::Right => Some(Msg::CycleKindNext),
        // [ / ] cycle the library browse mode (albums / artists / tracks).
        KeyCode::Char('[') => Some(Msg::CycleModePrev),
        KeyCode::Char(']') => Some(Msg::CycleModeNext),
        // S starts a station, but only inside an album/artist detail pane
        // (in the browse list there's no seed set — it would just dead-end).
        KeyCode::Char('S')
            if app.section == Section::Library && app.library.pane != LibraryPane::Browse =>
        {
            Some(Msg::AlbumStation)
        }
        KeyCode::Enter => Some(Msg::Activate),
        KeyCode::Char('e') => Some(Msg::Enqueue),
        // Add-to-playlist works on any track row (queue, lists, detail).
        KeyCode::Char('a') => Some(Msg::AddToPlaylist),
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
        // Autoplay toggle + recommendation feedback are global (the web keeps
        // them in the player bar). `f`/`F` act on the now-playing track and
        // no-op with a status line unless it was an autoplay pick.
        KeyCode::Char('A') => Some(Msg::ToggleAutoplay),
        KeyCode::Char('f') => Some(Msg::Feedback(FeedbackVote::Up)),
        KeyCode::Char('F') => Some(Msg::Feedback(FeedbackVote::Down)),
        // Save-offline is global (any track row); bulk-download acts on the
        // open album/playlist or the Downloads page; evict is Downloads-only.
        KeyCode::Char('d') => Some(Msg::SaveOffline),
        KeyCode::Char('W') => Some(Msg::BulkDownload),
        KeyCode::Char('E') if app.section == Section::Downloads => Some(Msg::EvictCache),
        // Queue edits only bind inside the queue view — 'x'/'c' are too
        // destructive to be global, and J/K/T would shadow navigation.
        KeyCode::Char('x') if app.section == Section::Queue => Some(Msg::QueueRemoveSelected),
        KeyCode::Char('c') if app.section == Section::Queue => Some(Msg::QueueClear),
        KeyCode::Char('J') if app.section == Section::Queue => Some(Msg::QueueMoveDown),
        KeyCode::Char('K') if app.section == Section::Queue => Some(Msg::QueueMoveUp),
        KeyCode::Char('T') if app.section == Section::Queue => Some(Msg::QueueMoveTop),
        // Playlist edits bind only inside the Playlists section (N/s/R/m are
        // ordinary letters we don't want stealing globally; 'x' removes a
        // playlist track here rather than a queue row).
        KeyCode::Char('N') if app.section == Section::Playlists => Some(Msg::NewPlaylist),
        KeyCode::Char('s') if app.section == Section::Playlists => Some(Msg::PlaylistShufflePlay),
        KeyCode::Char('R') if app.section == Section::Playlists => Some(Msg::PlaylistRenamePrompt),
        KeyCode::Char('X') if app.section == Section::Playlists => Some(Msg::PlaylistDelete),
        KeyCode::Char('m') if app.section == Section::Playlists => Some(Msg::PlaylistSuggest),
        KeyCode::Char('x') if app.section == Section::Playlists => Some(Msg::PlaylistRemoveTrack),
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
            action_for(&a, key(KeyCode::Char('4'))),
            Some(Msg::GoSection(Section::Playlists))
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('6'))),
            Some(Msg::GoSection(Section::Liked))
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('7'))),
            Some(Msg::GoSection(Section::Downloads))
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('8'))),
            Some(Msg::GoSection(Section::Settings))
        ));
        // No 9th section yet — the number is inert, not a panic.
        assert!(action_for(&a, key(KeyCode::Char('9'))).is_none());
    }

    #[test]
    fn section_nine_is_admin_only() {
        let mut a = app();
        // Non-admin (whoami unset): "9" is inert — Diagnostics is hidden.
        assert!(action_for(&a, key(KeyCode::Char('9'))).is_none());
        // Admin: "9" jumps to Diagnostics.
        a.whoami = Some(crate::api::WhoamiInfo {
            user_id: 1,
            role: "admin".to_owned(),
            username: None,
            display_name: None,
        });
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('9'))),
            Some(Msg::GoSection(Section::Diagnostics))
        ));
    }

    #[test]
    fn playlist_edit_keys_only_bind_in_playlists_section() {
        let mut a = app();
        assert!(action_for(&a, key(KeyCode::Char('N'))).is_none());
        assert!(action_for(&a, key(KeyCode::Char('R'))).is_none());
        a.section = Section::Playlists;
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('N'))),
            Some(Msg::NewPlaylist)
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('X'))),
            Some(Msg::PlaylistDelete)
        ));
        // 'x' removes a playlist track here, a queue row in the queue.
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('x'))),
            Some(Msg::PlaylistRemoveTrack)
        ));
    }

    #[test]
    fn library_mode_and_station_bindings() {
        let a = app(); // starts in Library
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('['))),
            Some(Msg::CycleModePrev)
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char(']'))),
            Some(Msg::CycleModeNext)
        ));
        // S only starts a station inside a Library *detail* pane — inert on
        // the browse list and in other sections.
        assert!(action_for(&a, key(KeyCode::Char('S'))).is_none());
        let mut det = app();
        det.library.pane = LibraryPane::AlbumDetail;
        assert!(matches!(
            action_for(&det, key(KeyCode::Char('S'))),
            Some(Msg::AlbumStation)
        ));
        let mut b = app();
        b.section = Section::Queue;
        assert!(action_for(&b, key(KeyCode::Char('S'))).is_none());
    }

    #[test]
    fn autoplay_and_feedback_bindings_are_global() {
        let a = app();
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('A'))),
            Some(Msg::ToggleAutoplay)
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('f'))),
            Some(Msg::Feedback(FeedbackVote::Up))
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('F'))),
            Some(Msg::Feedback(FeedbackVote::Down))
        ));
    }

    #[test]
    fn download_bindings() {
        let a = app(); // Library
        // Save-offline + bulk-download are global; evict is Downloads-only.
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('d'))),
            Some(Msg::SaveOffline)
        ));
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('W'))),
            Some(Msg::BulkDownload)
        ));
        assert!(action_for(&a, key(KeyCode::Char('E'))).is_none());
        // ctrl-d still pages (not save-offline).
        assert!(matches!(action_for(&a, ctrl('d')), Some(Msg::NavHalfPageDown)));
        let mut d = app();
        d.section = Section::Downloads;
        assert!(matches!(
            action_for(&d, key(KeyCode::Char('E'))),
            Some(Msg::EvictCache)
        ));
    }

    #[test]
    fn add_to_playlist_is_global() {
        let a = app();
        assert!(matches!(
            action_for(&a, key(KeyCode::Char('a'))),
            Some(Msg::AddToPlaylist)
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
