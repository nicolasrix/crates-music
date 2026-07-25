//! Hand-rolled single-line text input. The cursor indexes *extended grapheme
//! clusters* (via `unicode-segmentation`), so combining marks (`e` + U+0301)
//! and emoji ZWJ/flag sequences move, delete, and render as one unit — one
//! keypress, one user-perceived character.

use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use super::super::msg::InputMsg;
use super::super::theme::Theme;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct InputField {
    value: String,
    /// Cursor position in *grapheme clusters* (0 ..= cluster count).
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
        self.cursor = self.grapheme_count();
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
                // The inserted scalar may merge leftward into the previous
                // cluster (a combining mark), so the cursor can't just += 1 —
                // recompute it as the cluster count up to the end of the
                // inserted bytes.
                let end = at + c.len_utf8();
                self.cursor = self.value[..end].graphemes(true).count();
            }
            InputMsg::Backspace => {
                if self.cursor > 0 {
                    let start = self.byte_index(self.cursor - 1);
                    let end = self.byte_index(self.cursor);
                    self.value.replace_range(start..end, "");
                    self.cursor -= 1;
                }
            }
            InputMsg::Delete => {
                if self.cursor < self.grapheme_count() {
                    let start = self.byte_index(self.cursor);
                    let end = self.byte_index(self.cursor + 1);
                    self.value.replace_range(start..end, "");
                }
            }
            InputMsg::Left => self.cursor = self.cursor.saturating_sub(1),
            InputMsg::Right => self.cursor = (self.cursor + 1).min(self.grapheme_count()),
            InputMsg::Home => self.cursor = 0,
            InputMsg::End => self.cursor = self.grapheme_count(),
        }
    }

    fn grapheme_count(&self) -> usize {
        self.value.graphemes(true).count()
    }

    /// Render as a one-line prompt. When focused, the cluster under the cursor
    /// is reverse-video (a poor man's caret — the real terminal cursor is
    /// hidden by ratatui).
    pub(crate) fn line<'a>(&self, label: &'a str, focused: bool, theme: &Theme) -> Line<'a> {
        let label_style = if focused { theme.accent } else { theme.dim };
        let mut spans = vec![Span::styled(label, label_style)];
        if focused {
            let before: String = self.value.graphemes(true).take(self.cursor).collect();
            let at: String = self.value.graphemes(true).skip(self.cursor).take(1).collect();
            let after: String = self.value.graphemes(true).skip(self.cursor + 1).collect();
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

    /// Byte offset of the `n`th grapheme boundary (or end of string).
    fn byte_index(&self, n: usize) -> usize {
        self.value
            .grapheme_indices(true)
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
    fn multibyte_scalars_edit_correctly() {
        // Precomposed scalars: each é / 日 is a single codepoint *and* cluster.
        let mut f = typed("aé日");
        assert_eq!(f.cursor(), 3);
        f.apply(&InputMsg::Backspace);
        assert_eq!(f.value(), "aé");
        f.apply(&InputMsg::Home);
        f.apply(&InputMsg::Delete);
        assert_eq!(f.value(), "é");
    }

    #[test]
    fn combining_mark_is_one_cluster() {
        // "e" + combining acute (U+0301) → one user-perceived character.
        let mut f = InputField::default();
        f.apply(&InputMsg::Char('e'));
        f.apply(&InputMsg::Char('\u{0301}'));
        assert_eq!(f.value(), "e\u{0301}");
        assert_eq!(f.cursor(), 1, "combining mark merges into one cluster");
        // Backspace deletes the whole cluster, not just the accent.
        f.apply(&InputMsg::Backspace);
        assert_eq!(f.value(), "");
        assert_eq!(f.cursor(), 0);
    }

    #[test]
    fn emoji_zwj_sequence_is_one_cluster() {
        // 👩‍👧 (woman + ZWJ + girl) is a single grapheme cluster.
        let family = "👩\u{200d}👧";
        let mut f = InputField::default();
        f.set_value(family);
        assert_eq!(f.cursor(), 1, "ZWJ sequence counts as one cluster");
        // One backspace removes the entire sequence.
        f.apply(&InputMsg::Backspace);
        assert_eq!(f.value(), "");
    }

    #[test]
    fn cursor_navigates_clusters_not_scalars() {
        let mut f = InputField::default();
        f.set_value("a👩\u{200d}👧b"); // a | family | b  → 3 clusters
        assert_eq!(f.cursor(), 3);
        f.apply(&InputMsg::Home);
        f.apply(&InputMsg::Right); // now between 'a' and the family
        f.apply(&InputMsg::Delete); // deletes the whole family cluster
        assert_eq!(f.value(), "ab");
    }
}
