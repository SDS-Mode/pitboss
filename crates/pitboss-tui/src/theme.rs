//! Central palette + style helpers for pitboss-tui. Single source of
//! truth for colors — no `Color::*` literals should appear elsewhere
//! in this crate (enforced by review).

use pitboss_core::store::TaskStatus;
use ratatui::style::{Color, Modifier, Style};

use crate::state::TileStatus;

// ---------- Status palette ----------
pub const STATUS_PENDING: Color = Color::Gray;
pub const STATUS_RUNNING: Color = Color::Cyan;
pub const STATUS_SUCCESS: Color = Color::Green;
pub const STATUS_FAILED: Color = Color::Red;
pub const STATUS_TIMED_OUT: Color = Color::Yellow;
pub const STATUS_CANCELLED: Color = Color::Magenta;
pub const STATUS_SPAWN_FAILED: Color = Color::Red;
pub const STATUS_PAUSED: Color = Color::Blue;

// ---------- UI palette ----------
pub const TEXT_PRIMARY: Color = Color::White;
pub const TEXT_SECONDARY: Color = Color::Gray;
pub const TEXT_MUTED: Color = Color::DarkGray;
pub const BORDER: Color = Color::DarkGray;
pub const BORDER_FOCUSED: Color = Color::Cyan;
pub const OVERLAY_ACCENT_WARNING: Color = Color::Yellow;
pub const OVERLAY_ACCENT_INFO: Color = Color::Green;
pub const OVERLAY_ACCENT_PICKER: Color = Color::Cyan;

/// Color for a tile based on its `TileStatus` variant (matches the
/// existing `status_icon` mappings at `tui.rs:746` verbatim; the
/// spawn-failed icon shares Red with Failed by design).
pub fn tile_status_color(status: &TileStatus) -> Color {
    match status {
        TileStatus::Pending => STATUS_PENDING,
        TileStatus::Running => STATUS_RUNNING,
        TileStatus::Done(TaskStatus::Success) => STATUS_SUCCESS,
        TileStatus::Done(TaskStatus::Failed) => STATUS_FAILED,
        TileStatus::Done(TaskStatus::TimedOut) => STATUS_TIMED_OUT,
        TileStatus::Done(TaskStatus::Cancelled) => STATUS_CANCELLED,
        TileStatus::Done(TaskStatus::SpawnFailed) => STATUS_SPAWN_FAILED,
    }
}

pub fn tile_status_style(status: &TileStatus) -> Style {
    Style::default().fg(tile_status_color(status))
}

pub fn muted_style() -> Style {
    Style::default().fg(TEXT_MUTED)
}

pub fn secondary_style() -> Style {
    Style::default().fg(TEXT_SECONDARY)
}

pub fn primary_style() -> Style {
    Style::default().fg(TEXT_PRIMARY)
}

pub fn focused_border() -> Style {
    Style::default()
        .fg(BORDER_FOCUSED)
        .add_modifier(Modifier::BOLD)
}

pub fn idle_border() -> Style {
    Style::default().fg(BORDER)
}

// ---------- Model-family palette ----------
// Colors the title swatch on each tile so operators can tell at a glance
// which model family is running where. Family matched by name prefix
// (case-insensitive) — keep in sync with `model_family_color`.
pub const MODEL_OPUS: Color = Color::Magenta;
pub const MODEL_SONNET: Color = Color::Blue;
pub const MODEL_HAIKU: Color = Color::Green;
pub const MODEL_UNKNOWN: Color = Color::DarkGray;

/// Given a model string (e.g. `"claude-opus-4-7"`), return the color
/// for its family. Unknown families (nil, empty, non-Claude) fall back
/// to dark-gray. Matching is substring + case-insensitive because the
/// same family has many tags (`claude-opus-4-7`, `opus-4.7`, etc).
pub fn model_family_color(model: Option<&str>) -> Color {
    let Some(m) = model else {
        return MODEL_UNKNOWN;
    };
    let lc = m.to_ascii_lowercase();
    if lc.contains("opus") {
        MODEL_OPUS
    } else if lc.contains("sonnet") {
        MODEL_SONNET
    } else if lc.contains("haiku") {
        MODEL_HAIKU
    } else {
        MODEL_UNKNOWN
    }
}

// ---------- Log-line event palette ----------
pub const LOG_ASSISTANT_TEXT: Color = Color::White; // Primary output
pub const LOG_TOOL_USE: Color = Color::Cyan; // Model reaching out
pub const LOG_TOOL_RESULT: Color = Color::Green; // What came back
pub const LOG_SYSTEM: Color = Color::DarkGray; // Metadata (init etc.)
pub const LOG_RESULT: Color = Color::Magenta; // Terminal event
pub const LOG_RATE_LIMIT: Color = Color::Yellow; // Warning
pub const LOG_UNKNOWN: Color = Color::Gray; // Unknown shape
pub const LOG_UNPARSEABLE: Color = Color::Gray; // parse_line Err

/// Given a pre-rendered focus-log line, return a `Style` for coloring
/// it in the log pane. Matches on the display prefix written by
/// `watcher::format_event`: `"> "` assistant text, `"* "` tool use,
/// `"< "` tool result, `"v "` result, `"! "` rate limit. Anything else
/// falls back to `LOG_UNPARSEABLE` gray.
pub fn log_line_style(line: &str) -> Style {
    let color = if line.starts_with("> ") {
        LOG_ASSISTANT_TEXT
    } else if line.starts_with("* ") {
        LOG_TOOL_USE
    } else if line.starts_with("< ") {
        LOG_TOOL_RESULT
    } else if line.starts_with("v ") {
        LOG_RESULT
    } else if line.starts_with("! ") {
        LOG_RATE_LIMIT
    } else {
        LOG_UNPARSEABLE
    };
    Style::default().fg(color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn model_family_color_maps_opus_sonnet_haiku() {
        assert_eq!(model_family_color(Some("claude-opus-4-7")), MODEL_OPUS);
        assert_eq!(model_family_color(Some("claude-sonnet-4-6")), MODEL_SONNET);
        assert_eq!(model_family_color(Some("claude-haiku-4-5")), MODEL_HAIKU);
    }

    #[test]
    fn model_family_color_is_case_insensitive() {
        assert_eq!(model_family_color(Some("CLAUDE-OPUS-4-7")), MODEL_OPUS);
        assert_eq!(model_family_color(Some("Sonnet-Beta")), MODEL_SONNET);
    }

    #[test]
    fn model_family_color_unknown_falls_back() {
        assert_eq!(model_family_color(None), MODEL_UNKNOWN);
        assert_eq!(model_family_color(Some("")), MODEL_UNKNOWN);
        assert_eq!(model_family_color(Some("some-other-model")), MODEL_UNKNOWN);
    }

    #[test]
    fn log_line_style_maps_event_variants() {
        // Inputs are the display strings produced by
        // `watcher::format_event` (prefix char + content), not raw JSON.
        let cases = [
            ("> hi there", LOG_ASSISTANT_TEXT),
            ("* Read {\"file_path\":\"/tmp/x\"}", LOG_TOOL_USE),
            ("< ok", LOG_TOOL_RESULT),
            ("v result (session=abcd1234... | in=1 out=1)", LOG_RESULT),
            ("! rate-limit 429 (input) resets=?", LOG_RATE_LIMIT),
            ("something unprefixed", LOG_UNPARSEABLE),
            ("", LOG_UNPARSEABLE),
        ];
        for (line, expected) in cases {
            let style = log_line_style(line);
            assert_eq!(
                style.fg,
                Some(expected),
                "line {line:?} expected color {expected:?}, got {:?}",
                style.fg
            );
        }
    }

    #[test]
    fn status_colors_are_unique() {
        // SpawnFailed and Failed both map to Red by design (they're
        // semantically "the worker died"). Excluded from uniqueness
        // check to keep the intentional alias.
        let colors = [
            STATUS_PENDING,
            STATUS_RUNNING,
            STATUS_SUCCESS,
            STATUS_FAILED,
            STATUS_TIMED_OUT,
            STATUS_CANCELLED,
        ];
        let set: HashSet<_> = colors.iter().collect();
        assert_eq!(
            set.len(),
            colors.len(),
            "status palette has accidental color collision: {colors:?}"
        );
    }
}
