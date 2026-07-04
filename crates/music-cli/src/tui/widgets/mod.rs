pub(crate) mod input;
pub(crate) mod now_playing;
pub(crate) mod sidebar;

use std::time::Duration;

/// `mm:ss` (or `h:mm:ss` past the hour) for progress and track columns.
pub(crate) fn mmss(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mmss_formats() {
        assert_eq!(mmss(Duration::from_secs(0)), "0:00");
        assert_eq!(mmss(Duration::from_secs(65)), "1:05");
        assert_eq!(mmss(Duration::from_secs(3671)), "1:01:11");
    }
}
