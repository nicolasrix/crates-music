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

/// Human-readable byte size (binary units, matching the config's GiB-based
/// budget defaults). One decimal past KiB so the download gauges read
/// naturally: `0 B`, `812 KiB`, `1.3 GiB`.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    #[allow(clippy::cast_precision_loss)] // display only; exactness irrelevant
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
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

    #[test]
    fn human_bytes_formats() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(812), "812 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(10 * 1024 * 1024 * 1024), "10.0 GiB");
    }
}
