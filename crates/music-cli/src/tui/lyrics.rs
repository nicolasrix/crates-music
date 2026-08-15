//! Pure lyric-pane arithmetic: which line is being sung, and which slice of
//! the document to draw.
//!
//! A direct port of the web client's `player/activeLine.ts`, kept pure for
//! the same reason: the interesting behaviour is all at the boundaries
//! (before the first line, exactly on a timestamp, after the last one) and
//! none of it needs a terminal or a player to assert.
//!
//! Unlike the web, this runs on the TUI's existing 250 ms tick rather than
//! an animation frame. That is coarse enough to see: a line can light up
//! nearly a quarter-second late. The lead below is what closes the gap —
//! it is not the same 150 ms the web uses, because the two are correcting
//! for different amounts of lateness.

use crate::api::LyricLine;

/// Look this far ahead of the playhead when picking the active line.
///
/// Half the tick interval: on average a tick lands mid-way through the
/// window it represents, so biasing by half of it centres the error instead
/// of always running late. Erring early is also the kinder direction — a
/// line that appears a beat early reads as anticipation, one that appears
/// late reads as broken.
pub(crate) const HIGHLIGHT_LEAD_MS: i64 = 125;

/// Index of the last line whose timestamp has been reached, or `None` while
/// the track is still ahead of the first line (an intro).
///
/// `partition_point` is exactly the binary search this needs: it returns
/// the count of leading elements satisfying the predicate, so on a sorted
/// document that count minus one *is* the active index. Requires sorted
/// input — see [`sorted_lines`].
pub(crate) fn active_line(lines: &[LyricLine], position_ms: i64) -> Option<usize> {
    let reached = lines.partition_point(|l| l.start_ms <= position_ms);
    reached.checked_sub(1)
}

/// The document's lines in timestamp order.
///
/// The gateway sorts what it parses, but a Navidrome-tagged document is
/// passed through as the file supplied it — cheap insurance for the binary
/// search's precondition. Returns a new vector rather than sorting in place.
pub(crate) fn sorted_lines(lines: &[LyricLine]) -> Vec<LyricLine> {
    let mut out = lines.to_vec();
    out.sort_by_key(|l| l.start_ms);
    out
}

/// First visible row when centring `focus` in a viewport `height` rows tall.
///
/// Clamped at both ends so the pane never scrolls past the document: the
/// first and last lines sit at the top and bottom of the pane rather than
/// being dragged to the middle against a void. (The web pads with half a
/// viewport of blank space to get centring at the edges too; a terminal
/// pane is short enough that the padding would cost more than it buys.)
pub(crate) fn window_top(focus: usize, len: usize, height: usize) -> usize {
    if len <= height || height == 0 {
        return 0;
    }
    focus.saturating_sub(height / 2).min(len - height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(stamps: &[i64]) -> Vec<LyricLine> {
        stamps
            .iter()
            .map(|&start_ms| LyricLine {
                start_ms,
                text: format!("line at {start_ms}"),
            })
            .collect()
    }

    #[test]
    fn nothing_is_active_before_the_first_line() {
        assert_eq!(active_line(&lines(&[1000, 2000]), 500), None);
    }

    #[test]
    fn a_line_activates_exactly_on_its_timestamp() {
        assert_eq!(active_line(&lines(&[1000, 2000]), 1000), Some(0));
    }

    #[test]
    fn a_line_holds_until_the_next_one_starts() {
        let l = lines(&[1000, 2000]);
        assert_eq!(active_line(&l, 1999), Some(0));
        assert_eq!(active_line(&l, 2000), Some(1));
    }

    #[test]
    fn the_last_line_holds_through_the_outro() {
        assert_eq!(active_line(&lines(&[1000, 2000]), 900_000), Some(1));
    }

    #[test]
    fn an_empty_document_has_no_active_line() {
        assert_eq!(active_line(&[], 1000), None);
    }

    #[test]
    fn duplicate_timestamps_resolve_to_the_last_of_them() {
        // Two lines sharing a stamp is legal LRC (a repeated chorus tag).
        // Either answer highlights something reasonable; what matters is
        // that the choice is deterministic rather than search-order luck.
        assert_eq!(active_line(&lines(&[0, 1000, 1000, 2000]), 1500), Some(2));
    }

    #[test]
    fn sorted_lines_orders_without_mutating_the_input() {
        let original = lines(&[2000, 0, 1000]);
        let sorted = sorted_lines(&original);
        assert_eq!(
            sorted.iter().map(|l| l.start_ms).collect::<Vec<_>>(),
            vec![0, 1000, 2000]
        );
        assert_eq!(original[0].start_ms, 2000, "input must be untouched");
    }

    #[test]
    fn a_document_shorter_than_the_pane_never_scrolls() {
        assert_eq!(window_top(4, 5, 10), 0);
    }

    #[test]
    fn the_focus_line_is_centred_in_the_middle_of_a_document() {
        assert_eq!(window_top(50, 100, 10), 45);
    }

    #[test]
    fn the_window_clamps_at_both_ends() {
        // Near the top: no negative scroll.
        assert_eq!(window_top(1, 100, 10), 0);
        // Near the bottom: the last line is the last row, not the middle.
        assert_eq!(window_top(99, 100, 10), 90);
    }

    #[test]
    fn a_zero_height_pane_does_not_divide_by_zero() {
        assert_eq!(window_top(5, 100, 0), 0);
    }
}
