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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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
