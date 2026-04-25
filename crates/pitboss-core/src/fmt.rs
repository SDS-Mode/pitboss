//! Small display-format helpers shared across pitboss binaries.
//!
//! Centralized so we have one canonical answer to "how does pitboss render
//! a duration?" — four duplicated copies across `status.rs`, `tui_table.rs`,
//! `diff.rs`, and `tui.rs` drifted apart over the v0.7/v0.8 cycle (#97).

/// Format a millisecond duration as a human-readable string.
///
/// - `ms <= 0` renders as `"—"` (em-dash) to mean "unknown / not started".
/// - Sub-second durations render as `"Nms"` (e.g. `"42ms"`) (#108).
/// - Sub-minute durations render as `"Ns"` (e.g. `"42s"`).
/// - Sub-hour durations render as `"NmSSs"` (e.g. `"12m03s"`).
/// - Hour+ durations render as `"HhMMmSSs"` (e.g. `"2h07m03s"`) — without
///   this rollover the display kept ballooning minutes past 60 (#97).
#[must_use]
pub fn format_duration_ms(ms: i64) -> String {
    if ms <= 0 {
        return "—".to_string();
    }
    #[allow(clippy::cast_sign_loss)]
    let secs = (ms / 1000) as u64;
    if secs == 0 {
        return format!("{ms}ms");
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h{m:02}m{s:02}s")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Truncate `s` to at most `max` display columns, appending `…` (single char)
/// when truncation occurs. Widths are measured in `char`s, which is correct
/// for the ASCII task-IDs and sublead arrows pitboss emits; a full grapheme-
/// width implementation would pull in unicode-width for no current benefit.
///
/// Used by `pitboss status` and the TUI table header to keep long sub-lead
/// IDs from pushing subsequent columns out of alignment (#96).
#[must_use]
pub fn truncate_ellipsis(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_nonpositive_renders_as_dash() {
        assert_eq!(format_duration_ms(0), "—");
        assert_eq!(format_duration_ms(-5), "—");
    }

    #[test]
    fn duration_sub_second_shows_milliseconds() {
        assert_eq!(format_duration_ms(1), "1ms");
        assert_eq!(format_duration_ms(500), "500ms");
        assert_eq!(format_duration_ms(999), "999ms");
    }

    #[test]
    fn duration_sub_minute_drops_minute_component() {
        assert_eq!(format_duration_ms(1_000), "1s");
        assert_eq!(format_duration_ms(42_000), "42s");
        assert_eq!(format_duration_ms(59_999), "59s");
    }

    #[test]
    fn duration_sub_hour_shows_minutes_and_zero_padded_seconds() {
        assert_eq!(format_duration_ms(60_000), "1m00s");
        assert_eq!(format_duration_ms(180_000), "3m00s");
        // Just under the 1h rollover boundary.
        assert_eq!(format_duration_ms(3_599_000), "59m59s");
    }

    #[test]
    fn duration_rolls_over_at_one_hour() {
        assert_eq!(format_duration_ms(3_600_000), "1h00m00s");
        assert_eq!(format_duration_ms(7_623_000), "2h07m03s");
        // 100-hour run formats without overflowing the h-slot.
        assert_eq!(format_duration_ms(360_000_000), "100h00m00s");
    }

    #[test]
    fn truncate_shorter_than_max_returns_unchanged() {
        assert_eq!(truncate_ellipsis("abc", 5), "abc");
        assert_eq!(truncate_ellipsis("abcde", 5), "abcde");
    }

    #[test]
    fn truncate_longer_than_max_appends_ellipsis() {
        assert_eq!(truncate_ellipsis("abcdef", 5), "abcd…");
        assert_eq!(truncate_ellipsis("root→S1→w3", 6), "root→…");
    }

    #[test]
    fn truncate_max_zero_returns_empty() {
        assert_eq!(truncate_ellipsis("abc", 0), "");
    }

    #[test]
    fn truncate_max_one_is_just_ellipsis() {
        assert_eq!(truncate_ellipsis("abc", 1), "…");
    }
}
