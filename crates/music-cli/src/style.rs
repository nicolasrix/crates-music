//! TTY-only styling for classic (non-TUI) command output.
//!
//! Contract: **piped output is byte-identical to the unstyled string.**
//! `format.rs` stays plain and fully testable; this layer wraps whole
//! already-formatted lines at print sites only. owo-colors'
//! `if_supports_color` checks NO_COLOR / FORCE_COLOR / TTY-ness per call,
//! so scripts and agents reading a pipe see exactly the old bytes.

use owo_colors::{OwoColorize, Stream, Style};

fn accent() -> Style {
    Style::new().yellow().bold()
}

/// Section headings like `ARTISTS`, `LIKED`.
pub fn heading(s: &str) -> String {
    s.if_supports_color(Stream::Stdout, |t| t.style(accent()))
        .to_string()
}

/// Success words like the `ok` from `ping`.
pub fn ok(s: &str) -> String {
    s.if_supports_color(Stream::Stdout, |t| t.style(Style::new().green().bold()))
        .to_string()
}

/// Style a `format.rs` table: bold the first (header) line, leave every
/// data row verbatim. ANSI codes wrap around the padded header — no width
/// math changes.
pub fn table(table: &str) -> String {
    match table.split_once('\n') {
        Some((header, rest)) => format!(
            "{}\n{rest}",
            header.if_supports_color(Stream::Stdout, |t| t.style(Style::new().bold()))
        ),
        None => table.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The ambient env can force color even on a pipe (FORCE_COLOR — CI
    // and this harness set it), so pin the owo-colors override to make
    // the test hermetic: with color off, styling must be a byte-for-byte
    // passthrough. That is exactly what a plain pipe / NO_COLOR gets.
    #[test]
    fn output_is_passthrough_when_color_is_off() {
        owo_colors::set_override(false);
        assert_eq!(heading("ARTISTS"), "ARTISTS");
        assert_eq!(ok("ok"), "ok");
        let t = "ID    NAME\n1     x\n";
        assert_eq!(table(t), t);
        owo_colors::unset_override();
    }
}
