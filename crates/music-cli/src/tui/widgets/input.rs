//! Hand-rolled single-line text input. Char-indexed cursor (not grapheme
//! clusters — fine for search/prompt fields in v1), no dependency.

use ratatui::text::{Line, Span};

use super::super::msg::InputMsg;
use super::super::theme::Theme;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct InputField {
    value: String,
    /// Cursor position in *chars* (0 ..= char count).
    cursor: usize,
}

impl InputField {
    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    /// Replace the contents and park the cursor at the end — used to
    /// pre-fill a prompt (e.g. the current name when renaming).
    pub(crate) fn set_value(&mut self, value: &str) {
        value.clone_into(&mut self.value);
        self.cursor = self.char_count();
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn apply(&mut self, msg: &InputMsg) {
        match msg {
            InputMsg::Char(c) => {
                let at = self.byte_index(self.cursor);
                self.value.insert(at, *c);
                self.cursor += 1;
            }
            InputMsg::Backspace => {
                if self.cursor > 0 {
                    let at = self.byte_index(self.cursor - 1);
                    self.value.remove(at);
                    self.cursor -= 1;
                }
            }
            InputMsg::Delete => {
                if self.cursor < self.char_count() {
                    let at = self.byte_index(self.cursor);
                    self.value.remove(at);
                }
            }
            InputMsg::Left => self.cursor = self.cursor.saturating_sub(1),
            InputMsg::Right => self.cursor = (self.cursor + 1).min(self.char_count()),
            InputMsg::Home => self.cursor = 0,
            InputMsg::End => self.cursor = self.char_count(),
        }
    }

    fn char_count(&self) -> usize {
        self.value.chars().count()
    }

    /// Render as a one-line prompt. When focused, the char under the cursor
    /// is reverse-video (a poor man's caret — the real terminal cursor is
    /// hidden by ratatui).
    pub(crate) fn line<'a>(&self, label: &'a str, focused: bool, theme: &Theme) -> Line<'a> {
        let label_style = if focused { theme.accent } else { theme.dim };
        let mut spans = vec![Span::styled(label, label_style)];
        if focused {
            let before: String = self.value.chars().take(self.cursor).collect();
            let at: String = self.value.chars().skip(self.cursor).take(1).collect();
            let after: String = self.value.chars().skip(self.cursor + 1).collect();
            spans.push(Span::styled(before, theme.text));
            spans.push(Span::styled(
                if at.is_empty() { " ".to_owned() } else { at },
                theme.selected,
            ));
            spans.push(Span::styled(after, theme.text));
        } else if self.value.is_empty() {
            spans.push(Span::styled("(press i to type)", theme.dim));
        } else {
            spans.push(Span::styled(self.value.clone(), theme.text));
        }
        Line::from(spans)
    }

    /// Byte offset of the `n`th char (or end of string).
    fn byte_index(&self, n: usize) -> usize {
        self.value
            .char_indices()
            .nth(n)
            .map_or(self.value.len(), |(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> InputField {
        let mut f = InputField::default();
        for c in s.chars() {
            f.apply(&InputMsg::Char(c));
        }
        f
    }

    #[test]
    fn typing_appends_and_moves_cursor() {
        let f = typed("abc");
        assert_eq!(f.value(), "abc");
        assert_eq!(f.cursor(), 3);
    }

    #[test]
    fn insert_mid_string() {
        let mut f = typed("ac");
        f.apply(&InputMsg::Left);
        f.apply(&InputMsg::Char('b'));
        assert_eq!(f.value(), "abc");
        assert_eq!(f.cursor(), 2);
    }

    #[test]
    fn backspace_removes_before_cursor() {
        let mut f = typed("abc");
        f.apply(&InputMsg::Backspace);
        assert_eq!(f.value(), "ab");
        // at start: no-op
        f.apply(&InputMsg::Home);
        f.apply(&InputMsg::Backspace);
        assert_eq!(f.value(), "ab");
    }

    #[test]
    fn delete_removes_at_cursor() {
        let mut f = typed("abc");
        f.apply(&InputMsg::Home);
        f.apply(&InputMsg::Delete);
        assert_eq!(f.value(), "bc");
        assert_eq!(f.cursor(), 0);
        // at end: no-op
        f.apply(&InputMsg::End);
        f.apply(&InputMsg::Delete);
        assert_eq!(f.value(), "bc");
    }

    #[test]
    fn motion_clamps_at_both_ends() {
        let mut f = typed("ab");
        f.apply(&InputMsg::Left);
        f.apply(&InputMsg::Left);
        f.apply(&InputMsg::Left);
        assert_eq!(f.cursor(), 0);
        f.apply(&InputMsg::Right);
        f.apply(&InputMsg::Right);
        f.apply(&InputMsg::Right);
        assert_eq!(f.cursor(), 2);
    }

    #[test]
    fn multibyte_chars_edit_correctly() {
        let mut f = typed("aé日");
        assert_eq!(f.cursor(), 3);
        f.apply(&InputMsg::Backspace);
        assert_eq!(f.value(), "aé");
        f.apply(&InputMsg::Home);
        f.apply(&InputMsg::Delete);
        assert_eq!(f.value(), "é");
    }
}
