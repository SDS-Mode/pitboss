//! Terminal initialisation, teardown, and rendering.

use std::io::Stdout;

use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::state::{AppState, Mode, TileStatus};
use mosaic_core::store::TaskStatus;

// ---------------------------------------------------------------------------
// Stats helpers
// ---------------------------------------------------------------------------

/// Aggregate token and duration stats across all tiles.
///
/// * `total_in` / `total_out` — summed from all tiles that have token data
///   (non-zero input means the record came from summary.jsonl).
/// * `total_ms` — sum of `duration_ms` for every Done tile (parallel tasks
///   run concurrently so this is total CPU-time, not wall-clock).
/// * `in_progress` — true if any tile is still Pending or Running.
fn run_stats(state: &AppState) -> (u64, u64, i64, bool) {
    let mut total_in = 0u64;
    let mut total_out = 0u64;
    let mut total_ms = 0i64;
    let mut in_progress = false;
    for tile in &state.tasks {
        total_in += tile.token_usage_input;
        total_out += tile.token_usage_output;
        if let Some(ms) = tile.duration_ms {
            total_ms += ms;
        }
        if matches!(tile.status, TileStatus::Pending | TileStatus::Running) {
            in_progress = true;
        }
    }
    (total_in, total_out, total_ms, in_progress)
}

/// Format a token count as e.g. `"12.3k"` or `"456"`.
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000 {
        #[allow(clippy::cast_precision_loss)]
        let k = n as f64 / 1_000.0;
        format!("{k:.1}k")
    } else {
        n.to_string()
    }
}

/// Format milliseconds as `"8m24s"` (or `"0s"` for zero).
fn fmt_ms(ms: i64) -> String {
    #[allow(clippy::cast_sign_loss)]
    let secs = (ms.max(0) / 1_000) as u64;
    let m = secs / 60;
    let s = secs % 60;
    if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

// Number of tile columns in the grid.
const TILE_COLS: usize = 4;
// Percentage of mid-area height for the focus log pane (roughly 40%).
const LOG_PANE_PCT: u16 = 40;

// ---------------------------------------------------------------------------
// Init / teardown
// ---------------------------------------------------------------------------

pub fn init() -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub fn teardown(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Main render entry point
// ---------------------------------------------------------------------------

pub fn render(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    // Outer layout: title (1) | body (fill) | statusbar (1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    render_title(frame, chunks[0], state);
    render_body(frame, chunks[1], state);
    render_statusbar(frame, chunks[2]);

    // Overlays (drawn last so they appear on top).
    match state.mode {
        Mode::ViewingLog => render_log_overlay(frame, area, state),
        Mode::Help => render_help_overlay(frame, area),
        Mode::Normal => {}
    }
}

// ---------------------------------------------------------------------------
// Title bar
// ---------------------------------------------------------------------------

fn render_title(frame: &mut Frame, area: Rect, state: &AppState) {
    let short_id = if state.run_id.len() > 8 {
        &state.run_id[..8]
    } else {
        &state.run_id
    };

    let total = state.tasks.len();
    let done = state
        .tasks
        .iter()
        .filter(|t| matches!(t.status, TileStatus::Done(_)))
        .count();
    let failed = state.failed_count;

    let (total_in, total_out, total_ms, in_progress) = run_stats(state);

    // Build the stats suffix only when there is something meaningful to show.
    let token_part = if total_in > 0 || total_out > 0 {
        format!(
            " — {} in / {} out",
            fmt_tokens(total_in),
            fmt_tokens(total_out)
        )
    } else {
        String::new()
    };

    let duration_part = if in_progress {
        " — in progress".to_string()
    } else if total_ms > 0 {
        format!(" — {}", fmt_ms(total_ms))
    } else {
        String::new()
    };

    let title_text = format!(
        " Mosaic — run {short_id}… — {done}/{total} done, {failed} failed{token_part}{duration_part} "
    );

    let para = Paragraph::new(title_text)
        .style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Left);
    frame.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Body: tile grid + focus log pane
// ---------------------------------------------------------------------------

fn render_body(frame: &mut Frame, area: Rect, state: &AppState) {
    if state.tasks.is_empty() {
        let msg =
            Paragraph::new(" No tasks found in this run.").style(Style::default().fg(Color::Gray));
        frame.render_widget(msg, area);
        return;
    }

    // Split body vertically: grid | log pane
    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(100 - LOG_PANE_PCT),
            Constraint::Percentage(LOG_PANE_PCT),
        ])
        .split(area);

    render_tile_grid(frame, body_chunks[0], state);
    render_focus_log(frame, body_chunks[1], state);
}

// ---------------------------------------------------------------------------
// Tile grid
// ---------------------------------------------------------------------------

fn render_tile_grid(frame: &mut Frame, area: Rect, state: &AppState) {
    let n = state.tasks.len();
    let cols = TILE_COLS.min(n);
    let rows = n.div_ceil(cols);

    // Build column constraints (equal width).
    let col_constraints: Vec<Constraint> = (0..cols)
        .map(|_| Constraint::Ratio(1, u32::try_from(cols).unwrap_or(1)))
        .collect();

    // Build row constraints (equal height).
    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Ratio(1, u32::try_from(rows).unwrap_or(1)))
        .collect();

    let rows_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    for row in 0..rows {
        let cols_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints.clone())
            .split(rows_layout[row]);

        for col in 0..cols {
            let tile_idx = row * cols + col;
            if tile_idx >= n {
                break;
            }
            let tile = &state.tasks[tile_idx];
            let focused = tile_idx == state.focus;
            render_tile(frame, cols_layout[col], tile, focused);
        }
    }
}

fn render_tile(frame: &mut Frame, area: Rect, tile: &crate::state::TileState, focused: bool) {
    let (icon, icon_color) = status_icon(&tile.status);

    let border_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", tile.id))
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Inner content: icon+status line, duration, tokens
    let status_label = status_label(&tile.status);
    let duration_str = tile.duration_ms.map_or_else(
        || "—".to_string(),
        |ms| format!("{:02}m{:02}s", ms / 60_000, (ms % 60_000) / 1000),
    );

    let lines = vec![
        Line::from(vec![
            Span::styled(icon, Style::default().fg(icon_color)),
            Span::raw(" "),
            Span::styled(status_label, Style::default().fg(icon_color)),
        ]),
        Line::from(Span::styled(duration_str, Style::default().fg(Color::Gray))),
        Line::from(Span::styled(
            format!(
                "in:{} out:{}",
                tile.token_usage_input, tile.token_usage_output
            ),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

// ---------------------------------------------------------------------------
// Focus log pane (inline, bottom of body)
// ---------------------------------------------------------------------------

fn render_focus_log(frame: &mut Frame, area: Rect, state: &AppState) {
    let focused_id = state.focused_tile().map_or("—", |t| t.id.as_str());

    let status_str = state
        .focused_tile()
        .map(|t| status_label(&t.status))
        .unwrap_or_default();

    let block = Block::default()
        .borders(Borders::TOP)
        .title(format!(" Focus: {focused_id} ({status_str}) "))
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Render last N lines of log that fit in the area height.
    let height = inner.height as usize;
    let log_slice = if state.focus_log.len() > height {
        &state.focus_log[state.focus_log.len() - height..]
    } else {
        &state.focus_log
    };

    let lines: Vec<Line> = log_slice
        .iter()
        .map(|l| Line::from(Span::styled(l.as_str(), Style::default().fg(Color::Gray))))
        .collect();

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn render_statusbar(frame: &mut Frame, area: Rect) {
    let keys = " [h/j/k/l] nav  [L] log  [r] refresh  [?] help  [q] quit";
    let para = Paragraph::new(keys).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Overlays
// ---------------------------------------------------------------------------

fn render_log_overlay(frame: &mut Frame, area: Rect, state: &AppState) {
    let overlay_area = centered_rect(90, 85, area);

    // Clear background
    frame.render_widget(Clear, overlay_area);

    let focused_id = state.focused_tile().map_or("—", |t| t.id.as_str());

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Log: {focused_id}  [L/Esc] close "))
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(overlay_area);
    frame.render_widget(block, overlay_area);

    let lines: Vec<Line> = state
        .focus_log
        .iter()
        .map(|l| Line::from(Span::raw(l.as_str())))
        .collect();

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

fn render_help_overlay(frame: &mut Frame, area: Rect) {
    let overlay_area = centered_rect(60, 60, area);
    frame.render_widget(Clear, overlay_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help — Mosaic TUI v0.2-alpha ")
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(overlay_area);
    frame.render_widget(block, overlay_area);

    let help_text = vec![
        Line::from(""),
        Line::from("  Keybindings"),
        Line::from("  ──────────────────────────────"),
        Line::from("  h / ← : focus left"),
        Line::from("  l / → : focus right"),
        Line::from("  k / ↑ : focus up (4 cols)"),
        Line::from("  j / ↓ : focus down (4 cols)"),
        Line::from("  L     : view full log overlay"),
        Line::from("  r     : force refresh"),
        Line::from("  ?     : toggle this help"),
        Line::from("  q     : quit"),
        Line::from("  Esc   : close overlay"),
        Line::from(""),
        Line::from("  OBSERVE mode — no task spawning in v0.2-alpha."),
        Line::from(""),
        Line::from(Span::styled(
            "  Press Esc or ? to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let para = Paragraph::new(help_text);
    frame.render_widget(para, inner);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn status_icon(status: &TileStatus) -> (&'static str, Color) {
    match status {
        TileStatus::Pending => ("…", Color::Gray),
        TileStatus::Running => ("●", Color::Cyan),
        TileStatus::Done(TaskStatus::Success) => ("✓", Color::Green),
        TileStatus::Done(TaskStatus::Failed) => ("✗", Color::Red),
        TileStatus::Done(TaskStatus::TimedOut) => ("⏱", Color::Yellow),
        TileStatus::Done(TaskStatus::Cancelled) => ("⊘", Color::Magenta),
        TileStatus::Done(TaskStatus::SpawnFailed) => ("!", Color::Red),
    }
}

fn status_label(status: &TileStatus) -> &'static str {
    match status {
        TileStatus::Pending => "Pend",
        TileStatus::Running => "Run",
        TileStatus::Done(TaskStatus::Success) => "Done",
        TileStatus::Done(TaskStatus::Failed) => "Fail",
        TileStatus::Done(TaskStatus::TimedOut) => "Time",
        TileStatus::Done(TaskStatus::Cancelled) => "Canc",
        TileStatus::Done(TaskStatus::SpawnFailed) => "SpwF",
    }
}

/// Returns a `Rect` centered in `r` with the given width/height as percentages.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, TileState};
    use mosaic_core::store::TaskStatus;
    use std::path::PathBuf;

    fn tile(
        id: &str,
        status: TileStatus,
        duration_ms: Option<i64>,
        tin: u64,
        tout: u64,
    ) -> TileState {
        TileState {
            id: id.to_string(),
            status,
            duration_ms,
            token_usage_input: tin,
            token_usage_output: tout,
            exit_code: None,
            log_path: PathBuf::from("/dev/null"),
        }
    }

    fn state(tiles: Vec<TileState>) -> AppState {
        AppState {
            run_dir: PathBuf::from("/tmp/x"),
            run_id: "test".to_string(),
            tasks: tiles,
            focus: 0,
            mode: crate::state::Mode::Normal,
            focus_log: Vec::new(),
            failed_count: 0,
        }
    }

    #[test]
    fn fmt_tokens_small() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(1), "1");
        assert_eq!(fmt_tokens(999), "999");
    }

    #[test]
    fn fmt_tokens_large() {
        assert_eq!(fmt_tokens(1_000), "1.0k");
        assert_eq!(fmt_tokens(12_345), "12.3k");
    }

    #[test]
    fn fmt_ms_seconds_only() {
        assert_eq!(fmt_ms(0), "0s");
        assert_eq!(fmt_ms(59_000), "59s");
    }

    #[test]
    fn fmt_ms_minutes_and_seconds() {
        assert_eq!(fmt_ms(60_000), "1m00s");
        assert_eq!(fmt_ms(504_000), "8m24s");
    }

    #[test]
    fn fmt_ms_negative_treated_as_zero() {
        assert_eq!(fmt_ms(-1_000), "0s");
    }

    #[test]
    fn run_stats_empty() {
        let s = state(vec![]);
        let (ti, to, ms, in_progress) = run_stats(&s);
        assert_eq!((ti, to, ms), (0, 0, 0));
        assert!(!in_progress);
    }

    #[test]
    fn run_stats_sums_done_tiles() {
        let s = state(vec![
            tile(
                "a",
                TileStatus::Done(TaskStatus::Success),
                Some(1000),
                10,
                20,
            ),
            tile(
                "b",
                TileStatus::Done(TaskStatus::Failed),
                Some(2000),
                30,
                40,
            ),
        ]);
        let (ti, to, ms, in_progress) = run_stats(&s);
        assert_eq!((ti, to, ms), (40, 60, 3000));
        assert!(!in_progress);
    }

    #[test]
    fn run_stats_flags_in_progress() {
        let s = state(vec![
            tile(
                "a",
                TileStatus::Done(TaskStatus::Success),
                Some(1000),
                10,
                20,
            ),
            tile("b", TileStatus::Running, None, 0, 0),
        ]);
        let (_, _, _, in_progress) = run_stats(&s);
        assert!(in_progress);
    }

    #[test]
    fn run_stats_pending_counts_as_in_progress() {
        let s = state(vec![tile("a", TileStatus::Pending, None, 0, 0)]);
        let (_, _, _, in_progress) = run_stats(&s);
        assert!(in_progress);
    }
}
