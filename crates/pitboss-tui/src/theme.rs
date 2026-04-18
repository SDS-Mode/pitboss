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

// ---------- Log-line event palette ----------
pub const LOG_ASSISTANT_TEXT: Color = Color::White; // Primary output
pub const LOG_TOOL_USE: Color = Color::Cyan; // Model reaching out
pub const LOG_TOOL_RESULT: Color = Color::Green; // What came back
pub const LOG_SYSTEM: Color = Color::DarkGray; // Metadata (init etc.)
pub const LOG_RESULT: Color = Color::Magenta; // Terminal event
pub const LOG_RATE_LIMIT: Color = Color::Yellow; // Warning
pub const LOG_UNKNOWN: Color = Color::Gray; // Unknown shape
pub const LOG_UNPARSEABLE: Color = Color::Gray; // parse_line Err

/// Given a raw stream-json log line, return a `Style` for coloring it
/// in the log pane. Parses the line and matches on `Event` variant.
/// On parse failure, returns the `LOG_UNPARSEABLE` fallback.
pub fn log_line_style(line: &str) -> Style {
    use pitboss_core::parser::{parse_line, Event};
    let color = match parse_line(line.as_bytes()) {
        Ok(Event::AssistantText { .. }) => LOG_ASSISTANT_TEXT,
        Ok(Event::AssistantToolUse { .. }) => LOG_TOOL_USE,
        Ok(Event::ToolResult { .. }) => LOG_TOOL_RESULT,
        Ok(Event::System { .. }) => LOG_SYSTEM,
        Ok(Event::Result { .. }) => LOG_RESULT,
        Ok(Event::RateLimit { .. }) => LOG_RATE_LIMIT,
        Ok(Event::Unknown { .. }) => LOG_UNKNOWN,
        Err(_) => LOG_UNPARSEABLE,
    };
    Style::default().fg(color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn log_line_style_maps_event_variants() {
        let cases = [
            (
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#,
                LOG_ASSISTANT_TEXT,
            ),
            (
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t","name":"Read","input":{}}]}}"#,
                LOG_TOOL_USE,
            ),
            (
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":[{"type":"text","text":"ok"}]}]}}"#,
                LOG_TOOL_RESULT,
            ),
            (r#"{"type":"system","subtype":"init"}"#, LOG_SYSTEM),
            (
                r#"{"type":"result","session_id":"s","usage":{"input_tokens":1,"output_tokens":1}}"#,
                LOG_RESULT,
            ),
            (r#"{"type":"unknown","whatever":true}"#, LOG_UNKNOWN),
            ("{not valid json", LOG_UNPARSEABLE),
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
