//! LRC → structured lines. Pure, dependency-free, and the only place in
//! the project that understands the format.
//!
//! Parsing server-side is the point of the whole gateway-side design: the
//! web client and the TUI both consume `[{start_ms, text}]` and neither
//! has to reimplement the quirks below.
//!
//! What "the quirks" means in practice:
//!
//! * **Repeated timestamps on one line.** `[00:12.00][01:30.00] chorus`
//!   is how a repeated chorus is written once. Each stamp becomes its own
//!   line, or the second chorus never highlights.
//! * **Two hundredths separators.** `[mm:ss.xx]` is standard; `[mm:ss:xx]`
//!   appears in older files. Both are accepted.
//! * **Two- vs three-digit fractions.** `.5` is 500 ms, `.50` is 500 ms,
//!   `.500` is 500 ms — the digit count decides the scale, so a naive
//!   "parse as centiseconds" is wrong on a third of real files.
//! * **Word-level tags.** Enhanced LRC puts `<00:12.34>` between words.
//!   We highlight per line, so they are stripped from the text.
//! * **Metadata tags.** `[ar:…]`, `[ti:…]`, `[by:…]` are not lines.
//!
//! Deliberately *kept*: timestamped lines with empty text. They mark
//! instrumental gaps, and a consumer that highlights "the last line whose
//! start is ≤ now" needs them to clear the highlight during a solo
//! instead of leaving the previous line lit for ninety seconds.

use music_recommend::LyricLine;

/// Parse an LRC document into absolute-timestamped lines, sorted.
///
/// Returns empty when the input carries no timestamps at all — i.e. it
/// was plain text wearing an LRC filename. Callers treat that as
/// "unsynced", not as a parse failure.
#[must_use]
pub fn parse_lrc(raw: &str) -> Vec<LyricLine> {
    let offset_ms = find_offset(raw);
    let mut out: Vec<LyricLine> = Vec::new();

    for source_line in raw.lines() {
        let (stamps, text) = split_line(source_line);
        if stamps.is_empty() {
            continue;
        }
        let text = strip_word_tags(&text);
        for stamp in stamps {
            // Per the LRC convention a positive `offset` shifts the words
            // *earlier*, so it is subtracted. Clamped at zero because a
            // negative start is meaningless to a media element.
            let start_ms = (stamp - offset_ms).max(0);
            out.push(LyricLine { start_ms, text: text.clone() });
        }
    }

    // Stable sort: repeated-chorus stamps were emitted out of order
    // above, but two lines sharing a timestamp keep their document order.
    out.sort_by_key(|l| l.start_ms);
    out
}

/// Timestamp-free text for a synced or unsynced document. Blank lines are
/// dropped; this is the "just show me the words" view.
#[must_use]
pub fn to_plain(raw: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for source_line in raw.lines() {
        let (stamps, text) = split_line(source_line);
        // A bracketed line with no timestamps is metadata ([ar:…]) —
        // unless it had no brackets at all, in which case `split_line`
        // hands the whole thing back as text.
        if stamps.is_empty() && text.is_empty() {
            continue;
        }
        let text = strip_word_tags(&text);
        if !text.is_empty() {
            lines.push(text);
        }
    }
    lines.join("\n")
}

/// Whether a document carries at least one usable timestamp.
#[must_use]
pub fn is_synced(raw: &str) -> bool {
    raw.lines().any(|l| !split_line(l).0.is_empty())
}

/// Split a source line into its leading timestamps (ms) and the remaining
/// text. A line whose brackets are all metadata yields no timestamps and
/// empty text, so the caller can drop it.
fn split_line(line: &str) -> (Vec<i64>, String) {
    let mut rest = line.trim_start();
    let mut stamps = Vec::new();
    let mut saw_bracket = false;

    while let Some(stripped) = rest.strip_prefix('[') {
        let Some(end) = stripped.find(']') else {
            break;
        };
        saw_bracket = true;
        let inner = &stripped[..end];
        // A metadata tag ([ar:…]) among the leading brackets is skipped
        // but does not stop the scan: `[ti:X][00:01.00] words` is legal.
        if let Some(ms) = parse_timestamp(inner) {
            stamps.push(ms);
        }
        rest = stripped[end + 1..].trim_start();
    }

    // Brackets but no timestamps ⇒ pure metadata; report it as empty so
    // `[ar:Boards of Canada]` never renders as a lyric.
    if saw_bracket && stamps.is_empty() {
        return (Vec::new(), String::new());
    }
    (stamps, rest.trim_end().to_string())
}

/// `mm:ss`, `mm:ss.xx`, `mm:ss:xx`, or `hh:mm:ss.xx` → milliseconds.
/// `None` for anything that isn't a timestamp (which is how metadata tags
/// are recognised).
fn parse_timestamp(inner: &str) -> Option<i64> {
    let inner = inner.trim();
    // A metadata tag is `key:value` with a non-numeric key; bailing on the
    // first non-digit/non-separator character rejects those cheaply.
    if !inner.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }

    // Normalize the old `mm:ss:xx` hundredths separator to a decimal point
    // so one code path handles both. Three colon-separated groups where
    // the last is at most 3 digits is the ambiguous case — `01:02:03`
    // could be h:m:s or m:s:cs. Treat it as hundredths, matching the far
    // more common LRC usage (an hour-long lyric file is not a thing).
    let (time_part, frac_part) = if let Some((t, f)) = inner.split_once('.') {
        (t, Some(f))
    } else if inner.matches(':').count() == 2 {
        let (t, f) = inner.rsplit_once(':')?;
        (t, Some(f))
    } else {
        (inner, None)
    };

    let mut total_ms: i64 = 0;
    let mut units: Vec<i64> = Vec::new();
    for part in time_part.split(':') {
        units.push(part.trim().parse::<i64>().ok()?);
    }
    match units.as_slice() {
        [m, s] => total_ms += m * 60_000 + s * 1_000,
        [h, m, s] => total_ms += h * 3_600_000 + m * 60_000 + s * 1_000,
        _ => return None,
    }

    if let Some(frac) = frac_part {
        let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return None;
        }
        let value: i64 = digits.parse().ok()?;
        // The digit count sets the scale: .5 = 500 ms, .50 = 500 ms,
        // .500 = 500 ms. Assuming centiseconds mis-times a third of files.
        total_ms += match digits.len() {
            1 => value * 100,
            2 => value * 10,
            _ => value / 10_i64.pow(u32::try_from(digits.len()).unwrap_or(3) - 3),
        };
    }
    Some(total_ms)
}

/// The document-level `[offset:±ms]` tag, or 0.
fn find_offset(raw: &str) -> i64 {
    for line in raw.lines() {
        let trimmed = line.trim();
        let Some(inner) = trimmed.strip_prefix("[offset:").and_then(|s| s.strip_suffix(']')) else {
            continue;
        };
        // `+250` needs the sign stripped; `i64::from_str` rejects a
        // leading '+' on older toolchains and tolerates it on newer, so
        // normalize rather than depend on that.
        let value = inner.trim();
        let value = value.strip_prefix('+').unwrap_or(value);
        if let Ok(ms) = value.parse::<i64>() {
            return ms;
        }
    }
    0
}

/// Remove enhanced-LRC word timings (`<00:12.34>`) from a line's text.
fn strip_word_tags(text: &str) -> String {
    if !text.contains('<') {
        return text.trim().to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut depth = 0_usize;
    for ch in text.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_stamps() {
        let lines = parse_lrc("[00:12.00]first\n[01:02.50]second\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].start_ms, 12_000);
        assert_eq!(lines[0].text, "first");
        assert_eq!(lines[1].start_ms, 62_500);
    }

    #[test]
    fn real_provider_shape_round_trips() {
        // Verbatim shape of a live LRCLIB body: `[mm:ss.xx]`, a space
        // after the bracket, repeated identical text. The space must not
        // survive into the rendered line.
        let raw = "[00:44.33] Lake\n[00:50.03] Lake\n[01:01.52] Lake\n";
        let lines = parse_lrc(raw);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].start_ms, 44_330);
        assert_eq!(lines[0].text, "Lake");
        assert_eq!(lines[2].start_ms, 61_520);
    }

    #[test]
    fn fraction_scale_follows_digit_count() {
        // The bug this guards: treating every fraction as centiseconds
        // makes `.5` mean 50 ms and `.500` mean 5 s.
        assert_eq!(parse_lrc("[00:01.5]x")[0].start_ms, 1_500);
        assert_eq!(parse_lrc("[00:01.50]x")[0].start_ms, 1_500);
        assert_eq!(parse_lrc("[00:01.500]x")[0].start_ms, 1_500);
    }

    #[test]
    fn accepts_colon_hundredths_and_no_fraction() {
        assert_eq!(parse_lrc("[00:30:25]x")[0].start_ms, 30_250);
        assert_eq!(parse_lrc("[02:07]x")[0].start_ms, 127_000);
    }

    #[test]
    fn minutes_may_exceed_sixty() {
        assert_eq!(parse_lrc("[75:00.00]x")[0].start_ms, 4_500_000);
    }

    #[test]
    fn repeated_stamps_expand_into_separate_lines() {
        // One written chorus, sung twice — both occurrences must exist or
        // the second never highlights.
        let lines = parse_lrc("[00:10.00][02:30.00]chorus\n[01:00.00]verse\n");
        assert_eq!(lines.len(), 3);
        let starts: Vec<i64> = lines.iter().map(|l| l.start_ms).collect();
        assert_eq!(starts, [10_000, 60_000, 150_000]);
        assert_eq!(lines[2].text, "chorus");
    }

    #[test]
    fn metadata_tags_are_not_lyrics() {
        let raw = "[ar:Boards of Canada]\n[ti:Roygbiv]\n[00:05.00]real line\n";
        let lines = parse_lrc(raw);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "real line");
        assert_eq!(to_plain(raw), "real line");
    }

    #[test]
    fn offset_tag_shifts_earlier_and_clamps_at_zero() {
        let lines = parse_lrc("[offset:+500]\n[00:02.00]a\n[00:00.10]b\n");
        // Positive offset ⇒ words arrive earlier ⇒ subtract.
        assert_eq!(lines[1].start_ms, 1_500);
        // …but never before the start of the track.
        assert_eq!(lines[0].start_ms, 0);
    }

    #[test]
    fn negative_offset_shifts_later() {
        let lines = parse_lrc("[offset:-250]\n[00:02.00]a\n");
        assert_eq!(lines[0].start_ms, 2_250);
    }

    #[test]
    fn word_level_tags_are_stripped() {
        let lines = parse_lrc("[00:01.00]<00:01.00>hello <00:01.50>world\n");
        assert_eq!(lines[0].text, "hello world");
    }

    #[test]
    fn empty_timed_lines_survive() {
        // An instrumental gap. Dropping it would leave the previous line
        // highlighted through the whole solo.
        let lines = parse_lrc("[00:01.00]words\n[00:30.00]\n[01:00.00]more\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].text, "");
    }

    #[test]
    fn plain_text_has_no_timestamps_or_blanks() {
        let raw = "[00:01.00]one\n[00:30.00]\n[00:31.00]two\n";
        assert_eq!(to_plain(raw), "one\ntwo");
    }

    #[test]
    fn untimed_document_is_not_synced() {
        let raw = "just some words\nand more words\n";
        assert!(!is_synced(raw));
        assert!(parse_lrc(raw).is_empty());
        assert_eq!(to_plain(raw), "just some words\nand more words");
    }

    #[test]
    fn synced_document_is_synced() {
        assert!(is_synced("[ar:X]\n[00:01.00]hi\n"));
        // Metadata alone does not count as timing.
        assert!(!is_synced("[ar:X]\n[ti:Y]\n"));
    }

    #[test]
    fn garbage_brackets_do_not_panic() {
        // Unterminated bracket, empty doc, stray separators.
        assert!(parse_lrc("[00:01.00 unterminated").is_empty());
        assert!(parse_lrc("").is_empty());
        assert!(parse_lrc("[::]x").is_empty());
        assert!(parse_lrc("[00:zz.00]x").is_empty());
    }
}
