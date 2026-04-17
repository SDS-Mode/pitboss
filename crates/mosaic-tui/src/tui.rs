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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::state::{AppState, Mode, TileStatus};
use mosaic_core::store::TaskStatus;

// ---------------------------------------------------------------------------
// Stats helpers
// ---------------------------------------------------------------------------

/// Aggregate token, duration, and cost stats across all tiles.
///
/// * `total_in` / `total_out` — summed from all tiles that have token data
///   (non-zero input means the record came from summary.jsonl).
/// * `total_ms` — sum of `duration_ms` for every Done tile (parallel tasks
///   run concurrently so this is total CPU-time, not wall-clock).
/// * `in_progress` — true if any tile is still Pending or Running.
/// * `total_cost` — sum of estimated USD cost for tiles with known models
///   (tiles with unknown models contribute 0 but don't suppress the total).
fn run_stats(state: &AppState) -> (u64, u64, i64, bool, f64) {
    let mut total_in = 0u64;
    let mut total_out = 0u64;
    let mut total_ms = 0i64;
    let mut in_progress = false;
    let mut total_cost = 0.0f64;
    for tile in &state.tasks {
        total_in += tile.token_usage_input;
        total_out += tile.token_usage_output;
        if let Some(ms) = tile.duration_ms {
            total_ms += ms;
        }
        if matches!(tile.status, TileStatus::Pending | TileStatus::Running) {
            in_progress = true;
        }
        if let Some(model) = tile.model.as_deref() {
            let usage = mosaic_core::parser::TokenUsage {
                input: tile.token_usage_input,
                output: tile.token_usage_output,
                cache_read: tile.cache_read,
                cache_creation: tile.cache_creation,
            };
            if let Some(c) = mosaic_core::prices::cost_usd(model, &usage) {
                total_cost += c;
            }
        }
    }
    (total_in, total_out, total_ms, in_progress, total_cost)
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

/// Format the title string for the tile at `idx` in `state.tasks`.
///
/// Returns `[LEAD] {id}` if the tile is the lead (no `parent_task_id` AND at
/// least one other tile lists it as parent), otherwise just the id.
pub fn format_tile_title(state: &crate::state::AppState, idx: usize) -> String {
    let Some(tile) = state.tasks.get(idx) else {
        return String::new();
    };
    let id = &tile.id;
    let is_lead = tile.parent_task_id.is_none()
        && state
            .tasks
            .iter()
            .any(|t| t.parent_task_id.as_deref() == Some(id.as_str()));
    if is_lead {
        format!("[LEAD] {id}")
    } else {
        id.clone()
    }
}

/// Format the subtitle string for the tile at `idx` in `state.tasks`.
///
/// Returns `← {parent-id}` if the tile has a parent (i.e. it's a worker
/// spawned by a lead), otherwise an empty string.
pub fn format_tile_subtitle(state: &crate::state::AppState, idx: usize) -> String {
    let Some(tile) = state.tasks.get(idx) else {
        return String::new();
    };
    if let Some(parent) = &tile.parent_task_id {
        format!("\u{2190} {parent}")
    } else {
        String::new()
    }
}

/// Count tiles that were spawned as workers (i.e. have a `parent_task_id`).
pub fn workers_spawned(state: &crate::state::AppState) -> usize {
    state
        .tasks
        .iter()
        .filter(|t| t.parent_task_id.is_some())
        .count()
}

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

    // SnapIn is a full-screen replacement — skip the normal grid entirely.
    if let Mode::SnapIn {
        ref task_id,
        scroll,
        ..
    } = state.mode
    {
        render_snap_in(frame, area, state, task_id, scroll);
        return;
    }

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
    render_statusbar(frame, chunks[2], state);

    // Overlays (drawn last so they appear on top).
    match state.mode {
        Mode::ViewingLog => render_log_overlay(frame, area, state),
        Mode::Help => render_help_overlay(frame, area),
        Mode::PickingRun { selected } => render_run_picker_overlay(frame, area, state, selected),
        Mode::Normal | Mode::SnapIn { .. } => {}
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

    let (total_in, total_out, total_ms, in_progress, total_cost) = run_stats(state);

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

    // Cost part: show when any tile has tokens (cost may be 0 if all models unknown).
    let cost_part = if total_in > 0 || total_out > 0 {
        format!(" — ${total_cost:.2} total")
    } else {
        String::new()
    };

    let duration_part = if in_progress {
        // Show wall-clock elapsed time if we know when the run started.
        if let Some(started) = state.run_started_at {
            let elapsed = chrono::Utc::now() - started;
            #[allow(clippy::cast_sign_loss)]
            let secs = elapsed.num_seconds().max(0) as u64;
            let m = secs / 60;
            let s = secs % 60;
            format!(" — live {m}m{s:02}s")
        } else {
            " — in progress".to_string()
        }
    } else if total_ms > 0 {
        format!(" — {}", fmt_ms(total_ms))
    } else {
        String::new()
    };

    let workers = workers_spawned(state);
    let workers_part = if workers > 0 {
        format!(" — {workers} workers spawned")
    } else {
        String::new()
    };

    let title_text = format!(
        " Mosaic — run {short_id}… — {done}/{total} done, {failed} failed{token_part}{cost_part}{duration_part}{workers_part} "
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
            let focused = tile_idx == state.focus;
            render_tile(frame, cols_layout[col], state, tile_idx, focused);
        }
    }
}

fn render_tile(frame: &mut Frame, area: Rect, state: &AppState, tile_idx: usize, focused: bool) {
    let tile = &state.tasks[tile_idx];
    let (icon, icon_color) = status_icon(&tile.status);

    let tile_title = format_tile_title(state, tile_idx);
    let is_lead = tile_title.starts_with("[LEAD]");

    let border_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else if is_lead {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {tile_title} "))
        .border_style(border_style);

    // For worker tiles, append a dim parent annotation on the bottom border
    // so the hierarchy is visible without consuming content rows.
    let subtitle = format_tile_subtitle(state, tile_idx);
    if !subtitle.is_empty() {
        block = block.title_bottom(Span::styled(
            format!(" {subtitle} "),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Inner content: icon+status line, duration, tokens, cost
    let status_label = status_label(&tile.status);
    let duration_str = tile.duration_ms.map_or_else(
        || "\u{2014}".to_string(),
        |ms| format!("{:02}m{:02}s", ms / 60_000, (ms % 60_000) / 1000),
    );

    let cost_str = tile.model.as_deref().map_or_else(
        || "\u{2014}".to_string(),
        |model| {
            let usage = mosaic_core::parser::TokenUsage {
                input: tile.token_usage_input,
                output: tile.token_usage_output,
                cache_read: tile.cache_read,
                cache_creation: tile.cache_creation,
            };
            mosaic_core::prices::fmt_cost(mosaic_core::prices::cost_usd(model, &usage))
        },
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
        Line::from(Span::styled(cost_str, Style::default().fg(Color::DarkGray))),
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

fn render_statusbar(frame: &mut Frame, area: Rect, state: &AppState) {
    let keys = if matches!(state.mode, Mode::PickingRun { .. }) {
        " [j/k] navigate  [Enter] open  [Esc] cancel"
    } else {
        " [h/j/k/l] nav  [L] log  [o] open run  [?] help  [q] quit"
    };
    let para = Paragraph::new(keys).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Overlays
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Snap-in full-screen view
// ---------------------------------------------------------------------------

fn render_snap_in(frame: &mut Frame, area: Rect, state: &AppState, task_id: &str, scroll: usize) {
    // Layout: title_bar (1) | log_body (fill) | status_bar (1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    let total_lines = state.focus_log.len();
    let visible_rows = chunks[1].height as usize;

    // N = last visible line index (1-based, capped at total).
    let n = (scroll + visible_rows).min(total_lines);

    // Status of the snapped tile (if it still exists).
    let status_str = state
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .map_or("?", |t| status_label(&t.status));

    // --- Title bar ---
    let title_text = format!(" Snap-in: {task_id} ({status_str}) — line {n}/{total_lines} ");
    let title_para = Paragraph::new(title_text).style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(title_para, chunks[0]);

    // --- Log body ---
    let log_slice = if state.focus_log.is_empty() {
        &[][..]
    } else {
        let start = scroll.min(state.focus_log.len());
        let end = (scroll + visible_rows).min(state.focus_log.len());
        &state.focus_log[start..end]
    };

    let lines: Vec<Line> = log_slice
        .iter()
        .map(|l| Line::from(Span::styled(l.as_str(), Style::default().fg(Color::White))))
        .collect();

    let log_para = Paragraph::new(lines);
    frame.render_widget(log_para, chunks[1]);

    // --- Status bar ---
    let hint = " [Esc] back  [j/k] scroll  [Ctrl-D/U] page  [G] bottom  [g] top  [q] quit";
    let status_para = Paragraph::new(hint).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(status_para, chunks[2]);
}

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

fn render_run_picker_overlay(frame: &mut Frame, area: Rect, state: &AppState, selected: usize) {
    let overlay_area = centered_rect(80, 75, area);
    frame.render_widget(Clear, overlay_area);

    // Split overlay: list (fill) | help hint (1 line inside the border)
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Open Run  [j/k] navigate  [Enter] open  [Esc] cancel ")
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(overlay_area);
    frame.render_widget(block, overlay_area);

    if state.run_list.is_empty() {
        let msg = Paragraph::new(" No runs found.").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(msg, inner);
        return;
    }

    // Build list items.
    let items: Vec<ListItem> = state
        .run_list
        .iter()
        .map(|e| {
            let started = crate::runs::format_mtime(e.mtime);
            let status = if e.is_complete { "complete" } else { "running" };
            // Format: "run-id  started  N tasks  N failed  status"
            let short_id = if e.run_id.len() > 38 {
                &e.run_id[..38]
            } else {
                &e.run_id
            };
            let text = format!(
                "{:<38}  {:<22}  {:>5} tasks  {:>4} failed  {}",
                short_id, started, e.tasks_total, e.tasks_failed, status
            );
            ListItem::new(text)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    let mut list_state = ListState::default();
    list_state.select(Some(selected));

    frame.render_stateful_widget(list, inner, &mut list_state);
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
            cache_read: 0,
            cache_creation: 0,
            exit_code: None,
            log_path: PathBuf::from("/dev/null"),
            model: None,
            parent_task_id: None,
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
            run_list: Vec::new(),
            run_started_at: None,
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
        let (ti, to, ms, in_progress, _cost) = run_stats(&s);
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
        let (ti, to, ms, in_progress, _cost) = run_stats(&s);
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
        let (_, _, _, in_progress, _) = run_stats(&s);
        assert!(in_progress);
    }

    #[test]
    fn run_stats_pending_counts_as_in_progress() {
        let s = state(vec![tile("a", TileStatus::Pending, None, 0, 0)]);
        let (_, _, _, in_progress, _) = run_stats(&s);
        assert!(in_progress);
    }

    // -----------------------------------------------------------------------
    // SnapIn rendering test
    // -----------------------------------------------------------------------

    #[test]
    fn snap_in_render_contains_task_id_in_title() {
        use crate::state::Mode;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let task_id = "snap-task-42";

        let mut s = state(vec![tile(task_id, TileStatus::Running, None, 0, 0)]);
        s.focus_log = vec![
            "line one".to_string(),
            "line two".to_string(),
            "line three".to_string(),
        ];
        s.mode = Mode::SnapIn {
            task_id: task_id.to_string(),
            scroll: 0,
            at_bottom: true,
        };

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &s)).unwrap();

        // Collect the first rendered row (the title bar).
        let buf = terminal.backend().buffer();
        let first_row: String = (0..80)
            .map(|x| buf.cell((x, 0)).unwrap().symbol().to_string())
            .collect();

        assert!(
            first_row.contains(task_id),
            "title bar should contain task id; got: {first_row:?}"
        );
        assert!(
            first_row.contains("Run"),
            "title bar should contain status; got: {first_row:?}"
        );
    }

    #[test]
    fn snap_in_render_shows_log_lines() {
        use crate::state::Mode;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let task_id = "my-task";

        let mut s = state(vec![tile(task_id, TileStatus::Running, None, 0, 0)]);
        s.focus_log = vec![
            "> first log line".to_string(),
            "> second log line".to_string(),
        ];
        s.mode = Mode::SnapIn {
            task_id: task_id.to_string(),
            scroll: 0,
            at_bottom: true,
        };

        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &s)).unwrap();

        // Collect all rows as one string for searching.
        let buf = terminal.backend().buffer();
        let rendered: String = (0..10)
            .flat_map(|y| (0..80u16).map(move |x| (x, y)))
            .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
            .collect();

        assert!(
            rendered.contains("first log line"),
            "rendered output should contain log content; got snippet: {:?}",
            &rendered[..rendered.len().min(200)]
        );
    }

    // -----------------------------------------------------------------------
    // prices module — fmt_cost formatting
    // -----------------------------------------------------------------------

    #[test]
    fn fmt_cost_handles_unknown() {
        assert_eq!(mosaic_core::prices::fmt_cost(None), "\u{2014}");
    }

    #[test]
    fn fmt_cost_two_decimal_places() {
        assert_eq!(mosaic_core::prices::fmt_cost(Some(0.867)), "$0.87");
        assert_eq!(mosaic_core::prices::fmt_cost(Some(1.00)), "$1.00");
        assert_eq!(mosaic_core::prices::fmt_cost(Some(0.00)), "$0.00");
    }

    // -----------------------------------------------------------------------
    // run_stats includes cost
    // -----------------------------------------------------------------------

    #[test]
    fn run_stats_includes_cost_for_known_model() {
        let mut t = tile(
            "a",
            TileStatus::Done(mosaic_core::store::TaskStatus::Success),
            Some(1000),
            1_000_000,
            1_000_000,
        );
        t.model = Some("claude-haiku-4-5".to_string());
        let s = state(vec![t]);
        let (_, _, _, _, cost) = run_stats(&s);
        // 1M input + 1M output on haiku = $4.80
        assert!((cost - 4.80).abs() < 1e-4, "expected ~$4.80 got ${cost:.6}");
    }

    #[test]
    fn run_stats_cost_zero_for_unknown_model() {
        let mut t = tile(
            "a",
            TileStatus::Done(mosaic_core::store::TaskStatus::Success),
            Some(1000),
            500_000,
            500_000,
        );
        t.model = Some("claude-unknown-x-y".to_string());
        let s = state(vec![t]);
        let (_, _, _, _, cost) = run_stats(&s);
        assert!(
            cost.abs() < 1e-10,
            "unknown model should contribute 0 to total"
        );
    }

    // Helper variant — creates a tile with a specified parent_task_id.
    fn tile_with_parent(id: &str, status: TileStatus, parent: Option<String>) -> TileState {
        let mut t = tile(id, status, None, 0, 0);
        t.parent_task_id = parent;
        t
    }

    #[test]
    fn render_tile_title_for_lead() {
        // The lead is the tile whose parent_task_id is None and whose id
        // appears as a parent of at least one other tile. Mosaic renders
        // its title with [LEAD] prefix.
        let tiles = vec![
            tile("triage-lead", TileStatus::Running, None, 0, 0),
            tile_with_parent("worker-1", TileStatus::Running, Some("triage-lead".into())),
        ];
        let s = state(tiles);
        let title = crate::tui::format_tile_title(&s, 0);
        assert!(title.contains("[LEAD]"));
        assert!(title.contains("triage-lead"));

        let worker_title = crate::tui::format_tile_title(&s, 1);
        assert!(!worker_title.contains("[LEAD]"));
        assert!(worker_title.contains("worker-1"));
    }

    #[test]
    fn worker_tile_shows_parent_annotation() {
        let tiles = vec![
            tile("triage", TileStatus::Running, None, 0, 0),
            tile_with_parent("w-1", TileStatus::Running, Some("triage".into())),
        ];
        let s = state(tiles);
        let sub = crate::tui::format_tile_subtitle(&s, 1);
        assert!(sub.contains("← triage"));
    }

    #[test]
    fn lead_tile_has_no_parent_annotation() {
        let tiles = vec![tile("triage", TileStatus::Running, None, 0, 0)];
        let s = state(tiles);
        let sub = crate::tui::format_tile_subtitle(&s, 0);
        assert!(!sub.contains("←"));
    }

    #[test]
    fn workers_spawned_counts_tiles_with_parent() {
        let tiles = vec![
            tile("lead", TileStatus::Running, None, 0, 0),
            tile_with_parent("w-1", TileStatus::Running, Some("lead".into())),
            tile_with_parent("w-2", TileStatus::Running, Some("lead".into())),
        ];
        let s = state(tiles);
        assert_eq!(crate::tui::workers_spawned(&s), 2);
    }
}
