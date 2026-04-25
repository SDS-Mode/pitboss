//! Terminal initialisation, teardown, and rendering.

use std::io::Stdout;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
        Tabs, Wrap,
    },
    Frame, Terminal,
};

use crate::grouped_grid::SubtreeContainer;
use crate::state::{AppState, Mode, TileStatus};
use crate::theme;
use pitboss_core::store::TaskStatus;

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
            let usage = pitboss_core::parser::TokenUsage {
                input: tile.token_usage_input,
                output: tile.token_usage_output,
                cache_read: tile.cache_read,
                cache_creation: tile.cache_creation,
            };
            if let Some(c) = pitboss_core::prices::cost_usd(model, &usage) {
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

/// Format milliseconds as `"8m24s"` / `"2h07m03s"` (or `"0s"` for zero).
/// Thin wrapper around `pitboss_core::fmt::format_duration_ms` that swaps the
/// "not started" em-dash for `"0s"` — the TUI title bar and tile detail pane
/// want a concrete zero, not an unknown-marker.
fn fmt_ms(ms: i64) -> String {
    if ms <= 0 {
        return "0s".to_string();
    }
    pitboss_core::fmt::format_duration_ms(ms)
}

// Number of tile columns in the grid.
const TILE_COLS: usize = 4;
// Percentage of mid-area height for the focus log pane (roughly 40%).
const LOG_PANE_PCT: u16 = 40;

/// Single-char role glyph for the tile title. Lead = `★`, worker = `▸`.
/// Same lead-detection rule as `format_tile_title` (no `parent_task_id`
/// AND at least one other tile claims this id as parent). Exposed for
/// tests.
pub fn tile_role_glyph(state: &crate::state::AppState, idx: usize) -> &'static str {
    let Some(tile) = state.tasks.get(idx) else {
        return "";
    };
    let id = &tile.id;
    let is_lead = tile.parent_task_id.is_none()
        && state
            .tasks
            .iter()
            .any(|t| t.parent_task_id.as_deref() == Some(id.as_str()));
    if is_lead {
        "\u{2605}" // ★
    } else {
        "\u{25B8}" // ▸
    }
}

/// Format the title string for the tile at `idx` in `state.tasks`.
/// Just the id — the role glyph + model color swatch are rendered as
/// separate styled spans in `render_tile`.
pub fn format_tile_title(state: &crate::state::AppState, idx: usize) -> String {
    state
        .tasks
        .get(idx)
        .map(|t| t.id.clone())
        .unwrap_or_default()
}

/// Whether the tile at `idx` represents the run's lead. Used by the
/// render layer for border-styling and by tests.
pub fn tile_is_lead(state: &crate::state::AppState, idx: usize) -> bool {
    let Some(tile) = state.tasks.get(idx) else {
        return false;
    };
    let id = &tile.id;
    tile.parent_task_id.is_none()
        && state
            .tasks
            .iter()
            .any(|t| t.parent_task_id.as_deref() == Some(id.as_str()))
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
    // EnableMouseCapture lets us receive MouseEvent::ScrollUp/ScrollDown
    // for wheel scrolling in the detail view. Costs: the alt-screen
    // terminal no longer receives native selection/copy via click-drag
    // (users can typically hold Shift to bypass mouse capture in most
    // terminal emulators).
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub fn teardown(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Main render entry point
// ---------------------------------------------------------------------------

pub fn render(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    // Wipe the full frame before any widget draws. Ratatui's Block/Paragraph
    // only set STYLE for cells they "cover" — they don't clear character
    // content for cells that no text lands in. On terminal resize or when
    // a tile's inner content is shorter than the inner height, stale chars
    // from the prior frame persist (visible leakage — fix for #129).
    frame.render_widget(Clear, area);

    // Detail is a full-screen replacement — skip the normal grid entirely.
    if let Mode::Detail {
        ref task_id,
        scroll,
        ..
    } = state.mode
    {
        render_detail_view(frame, area, state, task_id, scroll);
        return;
    }

    // Completed page is also full-screen.
    if matches!(state.mode, Mode::Completed { .. }) {
        render_completed_page(frame, area, state);
        return;
    }

    // Show tab bar (title + tab line) when completed tiles exist.
    let completed_count = state.completed_tile_indices().len();
    let title_lines: u16 = if completed_count > 0 { 2 } else { 1 };

    // Outer layout: title (1 or 2) | body (fill) | statusbar (1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(title_lines),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    if title_lines == 2 {
        let title_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(chunks[0]);
        render_title(frame, title_chunks[0], state);
        render_tab_bar(frame, title_chunks[1], state, completed_count, false);
    } else {
        render_title(frame, chunks[0], state);
    }
    render_body(frame, chunks[1], state);
    render_statusbar(frame, chunks[2], state);

    // Overlays (drawn last so they appear on top).
    match &state.mode {
        Mode::Help => render_help_overlay(frame, area),
        Mode::PickingRun { selected } => render_run_picker_overlay(frame, area, state, *selected),
        Mode::ConfirmKill { target } => render_confirm_kill(frame, area, target),
        Mode::PromptReprompt { task_id, draft } => {
            render_prompt_reprompt(frame, area, task_id, draft);
        }
        Mode::ApprovalModal {
            request_id,
            task_id,
            summary,
            plan,
            kind,
            sub_mode,
        } => render_approval_modal(
            frame,
            area,
            request_id,
            task_id,
            summary,
            plan.as_ref(),
            *kind,
            sub_mode,
        ),
        Mode::PolicyEditor { rules, selected } => {
            render_policy_editor(frame, area, rules, *selected);
        }
        Mode::Normal | Mode::Detail { .. } | Mode::Completed { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Tab bar — shown between title and body when completed tiles exist.
// ---------------------------------------------------------------------------

fn render_tab_bar(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    completed_count: usize,
    on_completed_page: bool,
) {
    let active = state.active_tile_indices();
    let running = active
        .iter()
        .filter(|&&i| matches!(state.tasks[i].status, TileStatus::Running))
        .count();
    let pending = active
        .iter()
        .filter(|&&i| matches!(state.tasks[i].status, TileStatus::Pending))
        .count();

    let active_label = format!("Active ({running} running · {pending} pending)");
    let completed_label = format!("Completed ({completed_count})");

    let selected_idx = usize::from(on_completed_page);
    let tabs = Tabs::new(vec![active_label, completed_label])
        .select(selected_idx)
        .style(theme::secondary_style())
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .divider("│");
    frame.render_widget(tabs, area);
}

// ---------------------------------------------------------------------------
// Completed page — scrollable table of promoted tiles.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn render_completed_page(frame: &mut Frame, area: Rect, state: &AppState) {
    let Mode::Completed {
        ref selected_task_id,
        ref scroll_offset,
        ref sort_key,
        ref filter_status,
    } = state.mode
    else {
        return;
    };

    // Layout: title (1) | tab bar (1) | table (fill) | footer (1) | statusbar (1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    // Title
    let title_para = Paragraph::new(" Pitboss TUI — Completed Workers ")
        .style(theme::primary_style().add_modifier(Modifier::BOLD));
    frame.render_widget(title_para, chunks[0]);

    // Tab bar (on completed page = index 1 selected)
    let completed_count = state.completed_tile_indices().len();
    render_tab_bar(frame, chunks[1], state, completed_count, true);

    // Build sorted + filtered completed indices.
    let mut idxs = state.completed_tile_indices();
    // Apply sort_key.
    match sort_key {
        crate::state::SortKey::EndedAtDesc => {
            // already sorted by completed_tile_indices (desc ended_at)
        }
        crate::state::SortKey::DurationDesc => {
            idxs.sort_by(|&a, &b| {
                let da = state.tasks[a].duration_ms.unwrap_or(0);
                let db = state.tasks[b].duration_ms.unwrap_or(0);
                db.cmp(&da)
            });
        }
        crate::state::SortKey::StatusAsc => {
            idxs.sort_by_key(|&i| format!("{:?}", state.tasks[i].status));
        }
    }
    // Apply filter.
    if let Some(filter) = filter_status {
        idxs.retain(|&i| matches!(&state.tasks[i].status, TileStatus::Done(s) if s == filter));
    }

    let table_area = chunks[2];

    if idxs.is_empty() {
        let placeholder =
            Paragraph::new("\n  No completed workers yet.").style(theme::secondary_style());
        frame.render_widget(placeholder, table_area);
    } else {
        // Resolve selected row position.
        let sel_pos = idxs
            .iter()
            .position(|&i| state.tasks[i].id == *selected_task_id)
            .unwrap_or(0);

        // Column widths: TASK_ID(28) STATUS(14) TIME(10) TOKENS(18) EXIT(4) ENDED(17)
        let widths = [
            Constraint::Length(28),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(18),
            Constraint::Length(4),
            Constraint::Length(17),
        ];

        let header = Row::new(vec![
            Cell::from("TASK ID")
                .style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
            Cell::from("STATUS")
                .style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
            Cell::from("TIME")
                .style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
            Cell::from("TOKENS (in/out)")
                .style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
            Cell::from("EXIT")
                .style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
            Cell::from("ENDED")
                .style(Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        ])
        .height(1);

        // Build rows and hit rects simultaneously.
        let mut hit_rects: Vec<(usize, Rect)> = Vec::with_capacity(idxs.len());
        let rows: Vec<Row> = idxs
            .iter()
            .enumerate()
            .map(|(row_pos, &task_idx)| {
                let tile = &state.tasks[task_idx];
                let task_id = pitboss_core::fmt::truncate_ellipsis(&tile.id, 27);

                let (status_str, status_color) = match &tile.status {
                    TileStatus::Pending => ("… Pending", Color::Gray),
                    TileStatus::Running => ("● Running", Color::Green),
                    TileStatus::Done(s) => match s {
                        pitboss_core::store::TaskStatus::Success => ("✓ Success", Color::Green),
                        pitboss_core::store::TaskStatus::Failed => ("✗ Failed", Color::Red),
                        pitboss_core::store::TaskStatus::TimedOut => ("⏱ TimedOut", Color::Yellow),
                        pitboss_core::store::TaskStatus::Cancelled => ("⊘ Cancelled", Color::Gray),
                        pitboss_core::store::TaskStatus::SpawnFailed => ("! SpawnFail", Color::Red),
                        pitboss_core::store::TaskStatus::ApprovalRejected => {
                            ("⊘ ApprovalRej", Color::Yellow)
                        }
                        pitboss_core::store::TaskStatus::ApprovalTimedOut => {
                            ("⏱ ApprovalTO", Color::Yellow)
                        }
                    },
                };

                let time_str = tile
                    .duration_ms
                    .map_or_else(|| "—".to_string(), pitboss_core::fmt::format_duration_ms);

                let tokens_str = if tile.token_usage_input == 0 && tile.token_usage_output == 0 {
                    "—".to_string()
                } else {
                    format!(
                        "{} / {}",
                        fmt_tokens(tile.token_usage_input),
                        fmt_tokens(tile.token_usage_output)
                    )
                };

                let exit_str = tile
                    .exit_code
                    .map_or_else(|| "—".to_string(), |c| c.to_string());

                let ended_str = tile.completed_at.map_or_else(
                    || "—".to_string(),
                    |t| t.format("%m-%d %H:%M:%S").to_string(),
                );

                // Row y-position for hit rect: table_area.y + header(1) + row_pos,
                // adjusted for scroll. We store it and populate hit_rects below.
                let row_y = table_area
                    .y
                    .saturating_add(1) // header row
                    .saturating_add(
                        u16::try_from(row_pos.saturating_sub(*scroll_offset)).unwrap_or(u16::MAX),
                    );
                if row_pos >= *scroll_offset
                    && row_y < table_area.y.saturating_add(table_area.height)
                {
                    hit_rects.push((
                        task_idx,
                        Rect::new(table_area.x, row_y, table_area.width, 1),
                    ));
                }

                let style = if row_pos == sel_pos {
                    Style::default().bg(Color::DarkGray).fg(Color::White)
                } else {
                    Style::default().fg(status_color)
                };

                Row::new(vec![
                    Cell::from(task_id).style(style),
                    Cell::from(status_str).style(style.fg(status_color)),
                    Cell::from(time_str).style(style),
                    Cell::from(tokens_str).style(style),
                    Cell::from(exit_str).style(style),
                    Cell::from(ended_str).style(style),
                ])
                .height(1)
            })
            .collect();

        // Update hit rects.
        if let Ok(mut cache) = state.completed_hit_rects.lock() {
            *cache = hit_rects;
        }

        let mut table_state = TableState::default()
            .with_selected(Some(sel_pos))
            .with_offset(*scroll_offset);

        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::default())
            .row_highlight_style(Style::default().bg(Color::DarkGray));

        frame.render_stateful_widget(table, table_area, &mut table_state);
    }

    // Aggregate footer.
    let completed_all = state.completed_tile_indices();
    let total_cost: f64 = completed_all
        .iter()
        .filter_map(|&i| {
            let tile = &state.tasks[i];
            tile.model.as_deref().and_then(|m| {
                let usage = pitboss_core::parser::TokenUsage {
                    input: tile.token_usage_input,
                    output: tile.token_usage_output,
                    cache_read: tile.cache_read,
                    cache_creation: tile.cache_creation,
                };
                pitboss_core::prices::cost_usd(m, &usage)
            })
        })
        .sum();
    let total_in: u64 = completed_all
        .iter()
        .map(|&i| state.tasks[i].token_usage_input)
        .sum();
    let total_out: u64 = completed_all
        .iter()
        .map(|&i| state.tasks[i].token_usage_output)
        .sum();
    let failed_c = completed_all
        .iter()
        .filter(|&&i| {
            !matches!(
                state.tasks[i].status,
                TileStatus::Done(pitboss_core::store::TaskStatus::Success)
            )
        })
        .count();
    let footer_text = format!(
        " {}/{} completed — {} failed — {} in / {} out — ${:.2} total  [j/k] nav  [Enter] detail  [s] sort  [A/Esc] active view",
        completed_all.len(), state.tasks.len(), failed_c,
        fmt_tokens(total_in), fmt_tokens(total_out), total_cost
    );
    let footer = Paragraph::new(footer_text).style(theme::secondary_style());
    frame.render_widget(footer, chunks[3]);

    // Status bar hint.
    let hint = Paragraph::new(" [j/k] navigate  [Enter] detail  [s] cycle sort  [g/G] top/bottom  [A/Esc] back to Active ")
        .style(theme::secondary_style());
    frame.render_widget(hint, chunks[4]);
}

// ---------------------------------------------------------------------------
// Title bar
// ---------------------------------------------------------------------------

/// Return the most-distinguishing short form of a UUID-shaped run id.
/// For `UUIDv7` (our format), the last hyphen-delimited segment is the
/// random tail — sibling runs from the same minute share a common
/// time-prefix, so showing `…146e21f77dd8` is much more useful than
/// the lead 8 chars. Falls back to the full string when no hyphen
/// is present (custom run ids, tests).
fn short_run_id(run_id: &str) -> &str {
    run_id.rsplit_once('-').map_or(run_id, |(_, tail)| tail)
}

fn render_title(frame: &mut Frame, area: Rect, state: &AppState) {
    // Display the LAST segment of the UUID (random tail) rather than the
    // leading 8 chars. UUIDv7 time-prefixes are similar across runs created
    // close together; the tail is the actually-discriminating part, so
    // "…146e21f77dd8" tells you which run you're looking at where
    // "019da1b8…" looks the same as every sibling run from the same minute.
    let short_id = short_run_id(&state.run_id);

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
            format!(" — live {}", fmt_ms(elapsed.num_milliseconds()))
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
        " Pitboss TUI — run …{short_id} — {done}/{total} done, {failed} failed{token_part}{cost_part}{duration_part}{workers_part} "
    );

    let para = Paragraph::new(title_text)
        .style(theme::primary_style().add_modifier(Modifier::BOLD))
        .alignment(Alignment::Left);
    frame.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Body: tile grid + focus log pane
// ---------------------------------------------------------------------------

fn render_body(frame: &mut Frame, area: Rect, state: &AppState) {
    if state.tasks.is_empty() {
        let msg = Paragraph::new(" No tasks found in this run.").style(theme::secondary_style());
        frame.render_widget(msg, area);
        return;
    }

    // Split body horizontally: grid (70%) | approval pane (30%)
    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    let grid_area = h_chunks[0];
    let approval_area = h_chunks[1];

    // Split grid area vertically: tile grid | log pane
    let body_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(100 - LOG_PANE_PCT),
            Constraint::Percentage(LOG_PANE_PCT),
        ])
        .split(grid_area);

    render_tile_grid(frame, body_chunks[0], state);
    render_focus_log(frame, body_chunks[1], state);
    render_approval_list_pane(frame, approval_area, state);
}

// ---------------------------------------------------------------------------
// Tile grid
// ---------------------------------------------------------------------------

fn render_tile_grid(frame: &mut Frame, area: Rect, state: &AppState) {
    // Wipe any prior-frame content in the grid area before drawing tiles.
    frame.render_widget(Clear, area);

    // If there are no sub-trees this is a depth-1 run — use the original
    // flat grid layout so v0.5 behavior is preserved exactly.
    if state.subtrees.is_empty() {
        render_flat_tile_grid(frame, area, state);
        return;
    }

    // Depth-2: grouped layout.
    // Top section: root-layer tiles (those with no parent_task_id that are
    // not sub-leads themselves). Then one container per sub-tree.
    render_grouped_tile_grid(frame, area, state);
}

/// Original flat grid used for depth-1 runs (no sub-trees). Only renders
/// active (non-promoted) tiles; promoted tiles are on the Completed page.
fn render_flat_tile_grid(frame: &mut Frame, area: Rect, state: &AppState) {
    let active = state.active_tile_indices();
    let n = active.len();
    if n == 0 {
        let msg = Paragraph::new("\n  All workers have moved to the Completed page (C).")
            .style(theme::secondary_style());
        frame.render_widget(msg, area);
        if let Ok(mut cache) = state.tile_hit_rects.lock() {
            cache.clear();
        }
        return;
    }
    let cols = TILE_COLS.min(n);
    let rows = n.div_ceil(cols);

    let col_constraints: Vec<Constraint> = (0..cols)
        .map(|_| Constraint::Ratio(1, u32::try_from(cols).unwrap_or(1)))
        .collect();

    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Ratio(1, u32::try_from(rows).unwrap_or(1)))
        .collect();

    let rows_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    let mut hit_rects: Vec<(usize, Rect)> = Vec::with_capacity(n);

    for row in 0..rows {
        let cols_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints.clone())
            .split(rows_layout[row]);

        for col in 0..cols {
            let pos = row * cols + col;
            if pos >= n {
                break;
            }
            let task_idx = active[pos];
            let focused = task_idx == state.focus;
            let tile_rect = cols_layout[col];
            if state.compact_tiles {
                render_tile_compact(frame, tile_rect, state, task_idx, focused);
            } else {
                render_tile(frame, tile_rect, state, task_idx, focused);
            }
            // Store original tasks[] index (not pos within active) so mouse
            // hit-testing resolves to the correct element in state.tasks.
            hit_rects.push((task_idx, tile_rect));
        }
    }

    if let Ok(mut cache) = state.tile_hit_rects.lock() {
        *cache = hit_rects;
    }
}

/// Grouped tile grid for depth-2 runs:
/// ```text
/// ┌── Root layer ─────────────────────────────────────┐
/// │  [root-worker-1]  [root-worker-2]                 │
/// └───────────────────────────────────────────────────┘
/// ┌─ S1 (▼) $2.30/$5 | 3 workers | ⚠ 1 approval ─────┐
/// │  [W1.1]  [W1.2]  [W1.3]                           │
/// └───────────────────────────────────────────────────┘
/// ┌─ S2 (▶) $0.80/$5 | 2 workers [collapsed]  ───────┐
/// └───────────────────────────────────────────────────┘
/// ```
fn render_grouped_tile_grid(frame: &mut Frame, area: Rect, state: &AppState) {
    let sublead_ids = state.sorted_sublead_ids();

    // Identify root-layer tiles: those not belonging to any sub-tree.
    let subtree_worker_ids: std::collections::HashSet<&str> = state
        .subtrees
        .values()
        .flat_map(|v| v.workers.keys().map(String::as_str))
        .collect();
    // Sub-lead tiles themselves (the lead tile for each sub-tree) are also
    // in state.tasks — include them in the root row since they're the entry
    // point visible at the root level.
    let root_tiles: Vec<usize> = state
        .tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| !subtree_worker_ids.contains(t.id.as_str()))
        .map(|(i, _)| i)
        .collect();

    // Build vertical constraints: 1 row per section.
    // Root section height: tile rows (min 1 if root_tiles is non-empty).
    let root_height = if root_tiles.is_empty() {
        1u16
    } else {
        #[allow(clippy::cast_possible_truncation)]
        let root_tile_rows = (root_tiles.len().div_ceil(4).max(1) as u16).max(1);
        // Add 2 for the border block (top + bottom).
        root_tile_rows + 2
    };

    let mut constraints: Vec<Constraint> = vec![Constraint::Length(root_height)];
    let containers: Vec<SubtreeContainer> = sublead_ids
        .iter()
        .map(|id| {
            let view = &state.subtrees[id];
            let expanded = state.expanded.get(id).copied().unwrap_or(true);
            SubtreeContainer {
                sublead_id: id.as_str(),
                view,
                expanded,
            }
        })
        .collect();

    for c in &containers {
        // +2 for border chrome (top + bottom lines of the Block).
        let h = c.current_height() + 2;
        constraints.push(Constraint::Length(h));
    }
    // Fill remainder so the layout doesn't leave junk below.
    constraints.push(Constraint::Min(0));

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let hit_rects: Vec<(usize, Rect)> = Vec::with_capacity(state.tasks.len());
    let root_section_focused = state.focused_subtree_idx == 0;

    // --- Root section ---
    render_root_section(frame, sections[0], state, &root_tiles, root_section_focused);

    // --- Sub-tree containers ---
    for (i, container) in containers.iter().enumerate() {
        let section_rect = sections[i + 1];
        let header_focused = state.focused_subtree_idx == i + 1;
        render_subtree_container(frame, section_rect, state, container, header_focused);
    }

    if let Ok(mut cache) = state.tile_hit_rects.lock() {
        *cache = hit_rects;
    }
}

/// Render the root-layer tile section (tasks not owned by any sub-tree).
fn render_root_section(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    root_tile_indices: &[usize],
    focused: bool,
) {
    let border_style = if focused {
        theme::focused_border()
    } else {
        theme::idle_border()
    };
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .title(" Root layer ")
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if root_tile_indices.is_empty() {
        return;
    }
    let n = root_tile_indices.len();
    let cols = TILE_COLS.min(n);
    let rows = n.div_ceil(cols);
    let col_constraints: Vec<Constraint> = (0..cols)
        .map(|_| Constraint::Ratio(1, u32::try_from(cols).unwrap_or(1)))
        .collect();
    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Ratio(1, u32::try_from(rows).unwrap_or(1)))
        .collect();
    let rows_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(inner);

    for row in 0..rows {
        let cols_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints.clone())
            .split(rows_layout[row]);
        for col in 0..cols {
            let local = row * cols + col;
            if local >= n {
                break;
            }
            let tile_idx = root_tile_indices[local];
            let tile_focused = tile_idx == state.focus;
            render_tile(frame, cols_layout[col], state, tile_idx, tile_focused);
        }
    }
}

/// Render a single sub-tree container (header + optional inner tile grid).
fn render_subtree_container(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    container: &SubtreeContainer<'_>,
    header_focused: bool,
) {
    let border_style = if header_focused {
        theme::focused_border()
    } else {
        theme::idle_border()
    };
    let header_text = container.header_text();
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .title(header_text)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if !container.expanded || container.view.workers.is_empty() {
        return;
    }

    // Render worker tiles inside the inner area.
    let worker_ids: Vec<&str> = {
        let mut ids: Vec<&str> = container.view.workers.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    };
    let n = worker_ids.len();
    let cols = TILE_COLS.min(n);
    let rows = n.div_ceil(cols);
    let col_constraints: Vec<Constraint> = (0..cols)
        .map(|_| Constraint::Ratio(1, u32::try_from(cols).unwrap_or(1)))
        .collect();
    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Ratio(1, u32::try_from(rows).unwrap_or(1)))
        .collect();
    let rows_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(inner);

    for row in 0..rows {
        let cols_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints.clone())
            .split(rows_layout[row]);
        for col in 0..cols {
            let local = row * cols + col;
            if local >= n {
                break;
            }
            let worker_id = worker_ids[local];
            if let Some(tile_state) = container.view.workers.get(worker_id) {
                // Find the index in state.tasks for this worker (for focus highlighting).
                let tile_idx = state.tasks.iter().position(|t| t.id == tile_state.id);
                let tile_focused = tile_idx.is_some_and(|i| i == state.focus);
                // Render using the tile_state from the subtree view.
                render_subtree_worker_tile(
                    frame,
                    cols_layout[col],
                    state,
                    tile_state,
                    tile_focused,
                );
            }
        }
    }
}

/// Render a worker tile that lives inside a sub-tree container.
/// Uses the `TileState` from the `SubtreeView` rather than from `state.tasks`.
fn render_subtree_worker_tile(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    tile: &crate::state::TileState,
    focused: bool,
) {
    let (icon, icon_color) = status_icon(&tile.status);
    let swatch_color = theme::model_family_color(tile.model.as_deref());
    let border_style = if focused {
        theme::focused_border()
    } else {
        theme::idle_border()
    };
    let title_spans = vec![
        ratatui::text::Span::raw(" "),
        ratatui::text::Span::styled(
            "\u{258E}",
            ratatui::style::Style::default().fg(swatch_color),
        ),
        ratatui::text::Span::raw(" "),
        ratatui::text::Span::styled(
            "\u{25B8}",
            ratatui::style::Style::default().fg(theme::TEXT_SECONDARY),
        ),
        ratatui::text::Span::raw(" "),
        ratatui::text::Span::raw(tile.id.clone()),
        ratatui::text::Span::raw(" "),
    ];
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .title(ratatui::text::Line::from(title_spans))
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let status_label = status_label(&tile.status);
    let duration_str = tile.duration_ms.map_or_else(
        || "\u{2014}".to_string(),
        |ms| format!("{:02}m{:02}s", ms / 60_000, (ms % 60_000) / 1000),
    );
    let cost_str = tile.model.as_deref().map_or_else(
        || "\u{2014}".to_string(),
        |model| {
            let usage = pitboss_core::parser::TokenUsage {
                input: tile.token_usage_input,
                output: tile.token_usage_output,
                cache_read: tile.cache_read,
                cache_creation: tile.cache_creation,
            };
            pitboss_core::prices::fmt_cost(pitboss_core::prices::cost_usd(model, &usage))
        },
    );
    let activity_line = state
        .store_activity
        .get(&tile.id)
        .filter(|c| c.kv_ops > 0 || c.lease_ops > 0)
        .map(|c| {
            ratatui::text::Line::from(ratatui::text::Span::styled(
                format!("kv:{} lease:{}", c.kv_ops, c.lease_ops),
                theme::muted_style(),
            ))
        });
    let mut lines = vec![
        ratatui::text::Line::from(vec![
            ratatui::text::Span::styled(icon, ratatui::style::Style::default().fg(icon_color)),
            ratatui::text::Span::raw(" "),
            ratatui::text::Span::styled(
                status_label,
                ratatui::style::Style::default().fg(icon_color),
            ),
        ]),
        ratatui::text::Line::from(ratatui::text::Span::styled(
            duration_str,
            theme::secondary_style(),
        )),
        ratatui::text::Line::from(ratatui::text::Span::styled(
            format!(
                "in:{} out:{}",
                tile.token_usage_input, tile.token_usage_output
            ),
            theme::muted_style(),
        )),
        ratatui::text::Line::from(ratatui::text::Span::styled(cost_str, theme::muted_style())),
    ];
    if let Some(line) = activity_line {
        lines.push(line);
    }
    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

fn render_tile(frame: &mut Frame, area: Rect, state: &AppState, tile_idx: usize, focused: bool) {
    let tile = &state.tasks[tile_idx];
    let (icon, icon_color) = status_icon(&tile.status);

    let tile_title = format_tile_title(state, tile_idx);
    let is_lead = tile_is_lead(state, tile_idx);
    let role_glyph = tile_role_glyph(state, tile_idx);

    // Spec §8: focused and lead tiles both render with a distinct cyan + bold
    // border. The focused branch stays first for semantic clarity even though
    // it produces an identical style to the unfocused-lead branch today.
    let border_style = if focused || is_lead {
        theme::focused_border()
    } else {
        theme::idle_border()
    };

    // Title: `▎ ★ {id}` — color swatch (model family) + role glyph + id.
    // The swatch is a left-half-block char (`▎`) painted in the model's
    // family color so tiles sharing a model are glanceable as a group.
    // The role glyph replaces the old `[LEAD]` prefix (2 chars vs 7, and
    // visually distinct without reading text).
    let swatch_color = theme::model_family_color(tile.model.as_deref());
    let title_spans = vec![
        Span::raw(" "),
        Span::styled("\u{258E}", Style::default().fg(swatch_color)),
        Span::raw(" "),
        Span::styled(
            role_glyph,
            Style::default().fg(if is_lead {
                theme::BORDER_FOCUSED
            } else {
                theme::TEXT_SECONDARY
            }),
        ),
        Span::raw(" "),
        Span::raw(tile_title.clone()),
        Span::raw(" "),
    ];

    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title_spans))
        .border_style(border_style);

    // For worker tiles, append a dim parent annotation on the bottom border
    // so the hierarchy is visible without consuming content rows.
    let subtitle = format_tile_subtitle(state, tile_idx);
    if !subtitle.is_empty() {
        block = block.title_bottom(Span::styled(format!(" {subtitle} "), theme::muted_style()));
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
            let usage = pitboss_core::parser::TokenUsage {
                input: tile.token_usage_input,
                output: tile.token_usage_output,
                cache_read: tile.cache_read,
                cache_creation: tile.cache_creation,
            };
            pitboss_core::prices::fmt_cost(pitboss_core::prices::cost_usd(model, &usage))
        },
    );

    // Shared-store activity line: `kv:N lease:M`. Skip when both are 0
    // (almost all tiles during the first second + the lead in flat-mode
    // runs), so quiet tiles don't waste a row on a useless "kv:0 lease:0".
    let activity_line = state
        .store_activity
        .get(&tile.id)
        .filter(|c| c.kv_ops > 0 || c.lease_ops > 0)
        .map(|c| {
            Line::from(Span::styled(
                format!("kv:{} lease:{}", c.kv_ops, c.lease_ops),
                theme::muted_style(),
            ))
        });

    let mut lines = vec![
        Line::from(vec![
            Span::styled(icon, Style::default().fg(icon_color)),
            Span::raw(" "),
            Span::styled(status_label, Style::default().fg(icon_color)),
        ]),
        Line::from(Span::styled(duration_str, theme::secondary_style())),
        Line::from(Span::styled(
            format!(
                "in:{} out:{}",
                tile.token_usage_input, tile.token_usage_output
            ),
            theme::muted_style(),
        )),
        Line::from(Span::styled(cost_str, theme::muted_style())),
    ];
    if let Some(line) = activity_line {
        lines.push(line);
    }

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

/// Compact 2-line tile for the Active grid (`v` toggle). Shows only the
/// status line and token summary — much denser than the 5-line default,
/// useful when monitoring many workers simultaneously.
fn render_tile_compact(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    tile_idx: usize,
    focused: bool,
) {
    let tile = &state.tasks[tile_idx];
    let (icon, icon_color) = status_icon(&tile.status);
    let status_label = status_label(&tile.status);
    let tile_title = pitboss_core::fmt::truncate_ellipsis(&format_tile_title(state, tile_idx), 20);
    let is_lead = tile_is_lead(state, tile_idx);
    let role_glyph = tile_role_glyph(state, tile_idx);

    let border_style = if focused || is_lead {
        theme::focused_border()
    } else {
        theme::idle_border()
    };
    let swatch_color = theme::model_family_color(tile.model.as_deref());
    let title_spans = vec![
        Span::raw(" "),
        Span::styled("\u{258E}", Style::default().fg(swatch_color)),
        Span::raw(" "),
        Span::styled(role_glyph, Style::default().fg(theme::TEXT_SECONDARY)),
        Span::raw(" "),
        Span::raw(tile_title),
        Span::raw(" "),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title_spans))
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Two content lines: status + tokens.
    let tokens = format!(
        "{}↑ {}↓",
        fmt_tokens(tile.token_usage_input),
        fmt_tokens(tile.token_usage_output)
    );
    let lines = vec![
        Line::from(vec![
            Span::styled(icon, Style::default().fg(icon_color)),
            Span::raw(" "),
            Span::styled(status_label, Style::default().fg(icon_color)),
        ]),
        Line::from(Span::styled(tokens, theme::muted_style())),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
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

    // Full-frame block so wrapped content can't bleed outside the pane's
    // rect when the terminal is tight or long log lines wrap unexpectedly.
    // Prior `Borders::TOP` had no side/bottom to clip against.
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Focus: {focused_id} ({status_str}) "))
        .border_style(theme::idle_border());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Tail behavior: source-line slicing alone is incorrect when `wrap` is
    // enabled — wrapped lines expand beyond `inner.height` and Paragraph
    // shows the *head* of the slice. Cap source lines to a generous
    // multiple of the visible height, then ask Paragraph for the
    // width-aware wrapped line count and scroll to the bottom.
    let source_cap = (inner.height as usize).saturating_mul(4).max(32);
    let start = state.focus_log.len().saturating_sub(source_cap);
    let log_slice = &state.focus_log[start..];

    let lines: Vec<Line> = log_slice
        .iter()
        .map(|l| Line::from(Span::styled(l.as_str(), crate::theme::log_line_style(l))))
        .collect();

    // Estimate wrapped row count per source line so we can scroll to the
    // bottom. `Paragraph::line_count` is gated behind an unstable ratatui
    // feature; a div-by-width approximation matches its behavior closely
    // enough for bottom-anchor scroll. Uses char count (not grapheme
    // width) — accurate for the ASCII + stream-json content pitboss tails.
    let width = (inner.width as usize).max(1);
    let total_rows: usize = lines
        .iter()
        .map(|l| {
            let w = l.width();
            if w == 0 {
                1
            } else {
                w.div_ceil(width)
            }
        })
        .sum();
    let scroll =
        u16::try_from(total_rows.saturating_sub(inner.height as usize)).unwrap_or(u16::MAX);
    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(para, inner);
}

// ---------------------------------------------------------------------------
// Approval list pane (right-rail, 30% width)
// ---------------------------------------------------------------------------

fn render_approval_list_pane(frame: &mut Frame, area: Rect, state: &AppState) {
    use crate::state::PaneFocus;

    let focused = state.pane_focus == PaneFocus::ApprovalList;
    let border_style = if focused {
        theme::focused_border()
    } else {
        theme::idle_border()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Pending Approvals [a] ")
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.approval_list.items.is_empty() {
        let msg = Paragraph::new("No pending approvals")
            .style(theme::secondary_style())
            .alignment(Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    }

    let items: Vec<ListItem> = state
        .approval_list
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let line = state.approval_list.line_for(item);
            let style = if i == state.approval_list.selected_idx && focused {
                Style::default()
                    .fg(theme::OVERLAY_ACCENT_INFO)
                    .add_modifier(Modifier::BOLD)
            } else if i == state.approval_list.selected_idx {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                theme::primary_style()
            };
            ListItem::new(line).style(style)
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.approval_list.selected_idx));

    let list = List::new(items);
    frame.render_stateful_widget(list, inner, &mut list_state);
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn render_statusbar(frame: &mut Frame, area: Rect, state: &AppState) {
    let keys = if matches!(state.mode, Mode::PickingRun { .. }) {
        " [j/k] navigate  [Enter] open  [Esc] cancel"
    } else {
        " [hjkl] nav  [Enter] snap  [L] log  [x/X] kill wrk/run  [p/c] pause/cont  [r] reprompt  [P] policy  [o] open  [?] help  [q] quit"
    };
    let para = Paragraph::new(keys).style(theme::muted_style());
    frame.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Overlays
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Snap-in full-screen view
// ---------------------------------------------------------------------------

fn render_detail_view(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    task_id: &str,
    scroll: usize,
) {
    // Outer: title (1) | body (fill) | status (1)
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    // Body: metadata pane (left, fixed ~40 cols) | log pane (right, fills)
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(40), Constraint::Min(0)])
        .split(outer[1]);

    let tile = state.tasks.iter().find(|t| t.id == task_id);

    // --- Title bar ---
    let status_str = tile.map_or("?", |t| status_label(&t.status));
    // Both totals use visual rows (post-wrap) published by the render pass.
    // Falls back to focus_log.len() only before the first render pass when
    // detail_log_total_rows is still 0 — ensures the hint shows something
    // meaningful on the very first frame.
    let total_rows = state
        .detail_log_total_rows
        .load(std::sync::atomic::Ordering::Relaxed);
    let total_rows = if total_rows == 0 {
        state.focus_log.len()
    } else {
        total_rows
    };
    let viewport = state
        .detail_log_viewport
        .load(std::sync::atomic::Ordering::Relaxed)
        .max(1);
    let max_scroll = total_rows.saturating_sub(viewport);
    let display_scroll = scroll.min(max_scroll);
    let first_visible = display_scroll + 1;
    let last_visible = (display_scroll + viewport).min(total_rows);
    let scroll_hint = if max_scroll == 0 {
        format!(" (log {total_rows} rows; fits in view)")
    } else {
        format!(" (log rows {first_visible}-{last_visible} of {total_rows})")
    };
    let title_text = format!(" Detail: {task_id} ({status_str}){scroll_hint} ");
    // Intentional: inverted text on highlight bar — selection highlight pairing.
    let title_para = Paragraph::new(title_text).style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(title_para, outer[0]);

    // --- Metadata pane (left) ---
    render_detail_metadata(frame, body[0], state, task_id, tile);

    // --- Log pane (right) ---
    render_detail_log(frame, body[1], state, scroll);

    // --- Status bar ---
    let hint = " [jk 1 / JK 5 / Ctrl-D/U 10 / gG] scroll log  [Esc] back  [q] quit";
    let status_para = Paragraph::new(hint).style(theme::muted_style());
    frame.render_widget(status_para, outer[2]);
}

/// Left-pane metadata for the detail view. Pulls from `TileState`, scans
/// `focus_log` for live activity counters, and reads cached git diff.
#[allow(clippy::too_many_lines)]
fn render_detail_metadata(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    task_id: &str,
    tile: Option<&crate::state::TileState>,
) {
    // Mirror the log-pane Clear so leftover cells from the neighbour can't
    // bleed into this pane when scroll changes the log's paint extent.
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Metadata ")
        .border_style(theme::idle_border());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::with_capacity(32);

    let Some(tile) = tile else {
        lines.push(Line::from(Span::styled(
            "task not found in state",
            theme::muted_style(),
        )));
        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(para, inner);
        return;
    };

    // --- Identity ---
    lines.push(Line::from(Span::styled(
        "IDENTITY",
        theme::secondary_style().add_modifier(Modifier::BOLD),
    )));
    let role = if tile.parent_task_id.is_none() {
        "lead"
    } else {
        "worker"
    };
    lines.push(kv_line("role", role));
    if let Some(parent) = tile.parent_task_id.as_deref() {
        lines.push(kv_line("parent", &short_id(parent)));
    }
    let model_display = tile.model.as_deref().unwrap_or("—");
    let model_initial = model_initial(tile.model.as_deref());
    lines.push(kv_line(
        "model",
        &format!("{model_initial}  {model_display}"),
    ));
    lines.push(Line::from(""));

    // --- Lifecycle ---
    lines.push(Line::from(Span::styled(
        "LIFECYCLE",
        theme::secondary_style().add_modifier(Modifier::BOLD),
    )));
    let (icon, icon_color) = status_icon(&tile.status);
    let status_label_str = status_label(&tile.status);
    lines.push(Line::from(vec![
        Span::styled("  status  ", theme::muted_style()),
        Span::styled(icon, Style::default().fg(icon_color)),
        Span::raw(" "),
        Span::styled(status_label_str, Style::default().fg(icon_color)),
    ]));
    if let Some(exit) = tile.exit_code {
        lines.push(kv_line("exit", &exit.to_string()));
    }
    if let Some(ms) = tile.duration_ms {
        lines.push(kv_line("duration", &fmt_ms(ms)));
    } else {
        lines.push(kv_line("duration", "—"));
    }
    lines.push(Line::from(""));

    // --- Economics ---
    lines.push(Line::from(Span::styled(
        "TOKENS",
        theme::secondary_style().add_modifier(Modifier::BOLD),
    )));
    lines.push(kv_line("input", &fmt_tokens(tile.token_usage_input)));
    lines.push(kv_line("output", &fmt_tokens(tile.token_usage_output)));
    lines.push(kv_line("cache_r", &fmt_tokens(tile.cache_read)));
    lines.push(kv_line("cache_c", &fmt_tokens(tile.cache_creation)));
    let cache_hit_pct = {
        let total = tile.token_usage_input + tile.cache_read + tile.cache_creation;
        if total == 0 {
            0u64
        } else {
            // All operands non-negative so cast_sign_loss is a false positive;
            // precision loss at these magnitudes is negligible for a % display.
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_precision_loss,
                clippy::cast_sign_loss
            )]
            let pct = (tile.cache_read as f64 / total as f64 * 100.0) as u64;
            pct
        }
    };
    lines.push(kv_line("cache %", &format!("{cache_hit_pct}")));

    if let Some(model) = tile.model.as_deref() {
        let usage = pitboss_core::parser::TokenUsage {
            input: tile.token_usage_input,
            output: tile.token_usage_output,
            cache_read: tile.cache_read,
            cache_creation: tile.cache_creation,
        };
        let cost = pitboss_core::prices::cost_usd(model, &usage);
        let cost_str = pitboss_core::prices::fmt_cost(cost);
        lines.push(kv_line("cost", &cost_str));
    }
    lines.push(Line::from(""));

    // --- Activity (scanned from focus_log) ---
    lines.push(Line::from(Span::styled(
        "ACTIVITY (tail)",
        theme::secondary_style().add_modifier(Modifier::BOLD),
    )));
    let metrics = scan_focus_log(&state.focus_log);
    lines.push(kv_line("tool calls", &metrics.tool_use.to_string()));
    lines.push(kv_line("results", &metrics.tool_result.to_string()));
    lines.push(kv_line("text msgs", &metrics.assistant_text.to_string()));
    if metrics.rate_limit > 0 {
        lines.push(kv_line("rate lim", &metrics.rate_limit.to_string()));
    }
    if !metrics.top_tools.is_empty() {
        lines.push(Line::from(Span::styled(
            "  top tools:",
            theme::muted_style(),
        )));
        for (name, count) in &metrics.top_tools {
            // Strip the mcp__pitboss__ prefix for brevity.
            let short_name = name.strip_prefix("mcp__pitboss__").unwrap_or(name);
            lines.push(Line::from(Span::styled(
                format!("    {count}× {short_name}"),
                theme::muted_style(),
            )));
        }
    }
    lines.push(Line::from(""));

    // --- Git diff (cached on detail entry) ---
    lines.push(Line::from(Span::styled(
        "GIT DIFF",
        theme::secondary_style().add_modifier(Modifier::BOLD),
    )));
    if let Some(diff) = state.cached_git_diff.get(task_id) {
        lines.push(kv_line("files", &diff.files_changed.to_string()));
        lines.push(kv_line("+lines", &diff.insertions.to_string()));
        lines.push(kv_line("-lines", &diff.deletions.to_string()));
    } else {
        lines.push(Line::from(Span::styled(
            "  (unavailable — worktree path not tracked)",
            theme::muted_style(),
        )));
    }

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

/// Right-pane scrollable log. Scroll unit is VISUAL ROWS post-wrap, not
/// log-line indices — so long wrapped lines don't eat scroll budget and
/// the "jump to bottom" position actually shows the end of the log
/// regardless of how much content wraps.
///
/// We paint via `Buffer::set_stringn` directly rather than wrapping through
/// `Paragraph`. Ratatui 0.29's Paragraph render loop has no `x < area.width`
/// guard, so any over-measurement by its wrapping/truncating composers
/// (triggered by certain graphemes or style modifiers in our log) paints
/// past the pane's right edge and bleeds into the neighbouring metadata
/// pane. `set_stringn` takes an explicit `max_width` and clips to both that
/// and buffer bounds, so there's nowhere for overflow to land.
fn render_detail_log(frame: &mut Frame, area: Rect, state: &AppState, scroll: usize) {
    // Clear + block first. Clear blanks every cell in the pane so that any
    // row we don't paint below (short log, or viewport > content) shows as
    // empty rather than showing stale content from a previous frame.
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Log ")
        .border_style(theme::idle_border());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible_rows = inner.height as usize;
    let pane_width = inner.width as usize;
    if pane_width == 0 || visible_rows == 0 {
        return;
    }

    // Pre-wrap each log line ourselves into chunks of at most `pane_width`
    // chars. Each chunk becomes a single painted row. We track the row's
    // style alongside so set_stringn can color it consistently.
    let mut wrapped: Vec<(String, Style)> =
        Vec::with_capacity(state.focus_log.len().saturating_mul(2));
    for raw in &state.focus_log {
        let style = crate::theme::log_line_style(raw);
        if raw.is_empty() {
            wrapped.push((String::new(), style));
            continue;
        }
        let chars: Vec<char> = raw.chars().collect();
        for chunk in chars.chunks(pane_width) {
            wrapped.push((chunk.iter().collect(), style));
        }
    }
    let total_visual_rows = wrapped.len();

    // Publish viewport + total so scroll handlers compute a correct cap.
    state
        .detail_log_viewport
        .store(visible_rows, std::sync::atomic::Ordering::Relaxed);
    state
        .detail_log_total_rows
        .store(total_visual_rows, std::sync::atomic::Ordering::Relaxed);

    let max_scroll = total_visual_rows.saturating_sub(visible_rows);
    let clamped_scroll = scroll.min(max_scroll);
    let end = (clamped_scroll + visible_rows).min(total_visual_rows);

    let buf = frame.buffer_mut();
    let left = inner.left();
    let top = inner.top();
    for (row_idx, (line, style)) in wrapped[clamped_scroll..end].iter().enumerate() {
        // row_idx < visible_rows ≤ inner.height, which is already a u16.
        let y = top + u16::try_from(row_idx).unwrap_or(u16::MAX);
        // `set_stringn` returns the (x, y) of the first cell after the
        // painted string — we ignore it; clipping is handled internally.
        // The `max_width` is pane_width so the string is truncated at the
        // right edge regardless of what's in `line`.
        buf.set_stringn(left, y, line, pane_width, *style);
    }
}

/// Simple `  key  value` two-column line for the metadata pane.
fn kv_line(key: &str, value: &str) -> Line<'static> {
    // 12-char left-padded label so "tool calls" (exactly 10 chars) still
    // has a visible gap before the value. Indent is 2 spaces. Total
    // prefix = 14 chars before the value.
    Line::from(vec![
        Span::styled(format!("  {key:<12}"), theme::muted_style()),
        Span::styled(value.to_string(), theme::primary_style()),
    ])
}

/// Collapse very long ids (UUID-format) into a shorter form for display.
fn short_id(id: &str) -> String {
    if id.len() > 20 {
        format!("{}…{}", &id[..8], &id[id.len() - 4..])
    } else {
        id.to_string()
    }
}

/// Single-character model family initial: H/S/O for Haiku/Sonnet/Opus,
/// `?` otherwise.
fn model_initial(model: Option<&str>) -> &'static str {
    match model.unwrap_or("") {
        m if m.contains("haiku") => "H",
        m if m.contains("sonnet") => "S",
        m if m.contains("opus") => "O",
        _ => "?",
    }
}

/// Counters extracted from the focus-log tail.
#[derive(Debug, Default)]
struct FocusLogMetrics {
    assistant_text: usize,
    tool_use: usize,
    tool_result: usize,
    rate_limit: usize,
    /// Top 3 tool names by frequency (descending).
    top_tools: Vec<(String, usize)>,
}

/// Scan the tailed focus log (already formatted with prefix chars by
/// `watcher::format_event`) and count events by type, plus tally tool
/// names for the "top tools" breakdown.
fn scan_focus_log(focus_log: &[String]) -> FocusLogMetrics {
    use std::collections::HashMap;
    let mut m = FocusLogMetrics::default();
    let mut tool_counts: HashMap<String, usize> = HashMap::new();

    for line in focus_log {
        if let Some(rest) = line.strip_prefix("* ") {
            m.tool_use += 1;
            // Format: "* <tool_name> <args_summary>"
            if let Some(space) = rest.find(' ') {
                let name = &rest[..space];
                *tool_counts.entry(name.to_string()).or_insert(0) += 1;
            } else {
                *tool_counts.entry(rest.to_string()).or_insert(0) += 1;
            }
        } else if line.starts_with("< ") {
            m.tool_result += 1;
        } else if line.starts_with("> ") {
            m.assistant_text += 1;
        } else if line.starts_with("! ") {
            m.rate_limit += 1;
        }
    }

    // Sort by count descending, then by name ascending for a stable order —
    // HashMap iteration is unstable, so without the secondary key entries
    // with equal counts flicker between frames.
    let mut sorted: Vec<(String, usize)> = tool_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sorted.truncate(3);
    m.top_tools = sorted;
    m
}

fn render_help_overlay(frame: &mut Frame, area: Rect) {
    let overlay_area = centered_rect(70, 80, area);
    frame.render_widget(Clear, overlay_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help — Pitboss TUI ")
        .border_style(Style::default().fg(theme::OVERLAY_ACCENT_INFO));

    let inner = block.inner(overlay_area);
    frame.render_widget(block, overlay_area);

    let help_text = vec![
        Line::from(""),
        Line::from("  Keybindings"),
        Line::from("  ──────────────────────────────"),
        Line::from("  Navigation"),
        Line::from("    h / ←  : focus left"),
        Line::from("    l / →  : focus right"),
        Line::from("    k / ↑  : focus up (4 cols)"),
        Line::from("    j / ↓  : focus down (4 cols)"),
        Line::from(""),
        Line::from("  Views"),
        Line::from("    Enter  : snap-in to focused tile (full-screen log)"),
        Line::from("    L      : view full log overlay"),
        Line::from("    o      : open run picker"),
        Line::from(""),
        Line::from("  Control (v0.4)"),
        Line::from("    x      : cancel focused worker (confirm)"),
        Line::from("    X      : cancel entire run (confirm)"),
        Line::from("    p      : pause focused worker"),
        Line::from("    c      : continue focused worker"),
        Line::from("    r      : reprompt focused worker"),
        Line::from(""),
        Line::from("  System"),
        Line::from("    ?      : toggle this help"),
        Line::from("    q      : quit"),
        Line::from("    Esc    : close overlay / modal"),
        Line::from(""),
        Line::from(Span::styled(
            "  Press Esc or ? to close",
            theme::muted_style(),
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
        .border_style(Style::default().fg(theme::OVERLAY_ACCENT_PICKER));

    let inner = block.inner(overlay_area);
    frame.render_widget(block, overlay_area);

    if state.run_list.is_empty() {
        let msg = Paragraph::new(" No runs found.").style(theme::muted_style());
        frame.render_widget(msg, inner);
        return;
    }

    // Build list items.
    let items: Vec<ListItem> = state
        .run_list
        .iter()
        .map(|e| {
            let started = crate::runs::format_mtime(e.mtime);
            let status = e.status.label();
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

    // Intentional: inverted selection highlight pairing, not a palette color.
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

    // Populate the picker hit-cache so mouse clicks resolve to row
    // indices. Rows are laid out top-to-bottom starting at `inner.y`;
    // we clip to the inner height so clicks below the last run land
    // on no row (returns None from `picker_row_at`).
    if let Ok(mut cache) = state.picker_hit_rects.lock() {
        cache.clear();
        let max_rows = inner.height as usize;
        for (idx, _) in state.run_list.iter().enumerate().take(max_rows) {
            let y = inner.y + u16::try_from(idx).unwrap_or(u16::MAX);
            cache.push((idx, ratatui::layout::Rect::new(inner.x, y, inner.width, 1)));
        }
    }
}

fn render_confirm_kill(frame: &mut Frame, area: Rect, target: &crate::state::KillTarget) {
    let msg = match target {
        crate::state::KillTarget::Worker(id) => format!(" Cancel worker `{id}`? [y/N] "),
        crate::state::KillTarget::Run => {
            " Cancel the ENTIRE RUN? All workers terminate. [y/N] ".into()
        }
    };
    let msg_w = u16::try_from(msg.len()).unwrap_or(u16::MAX);
    let modal_w = msg_w.saturating_add(4).min(area.width);
    let modal_h = 3u16;
    let x = area.x + area.width.saturating_sub(modal_w) / 2;
    let y = area.y + area.height.saturating_sub(modal_h) / 2;
    let modal = Rect::new(x, y, modal_w, modal_h);
    frame.render_widget(Clear, modal);
    let block = Block::default().borders(Borders::ALL).title(" Confirm ");
    let para = Paragraph::new(msg)
        .block(block)
        .alignment(Alignment::Center)
        .style(Style::default().fg(theme::OVERLAY_ACCENT_WARNING));
    frame.render_widget(para, modal);
}

// Forward decls filled in by Task 26 / Task 32.
fn render_prompt_reprompt(frame: &mut Frame, area: Rect, task_id: &str, draft: &str) {
    let modal_w = (area.width * 3 / 4).min(80);
    let modal_h = (area.height / 2).clamp(8, 20);
    let x = area.x + area.width.saturating_sub(modal_w) / 2;
    let y = area.y + area.height.saturating_sub(modal_h) / 2;
    let modal = Rect::new(x, y, modal_w, modal_h);
    frame.render_widget(Clear, modal);
    let block = Block::default().borders(Borders::ALL).title(format!(
        " Reprompt `{task_id}` — F2 / Alt+Enter / Ctrl+Enter to send, Esc cancel "
    ));
    let para = Paragraph::new(draft)
        .block(block)
        .wrap(Wrap { trim: false })
        .style(theme::primary_style());
    frame.render_widget(para, modal);
}

#[allow(clippy::too_many_arguments)]
fn render_approval_modal(
    frame: &mut Frame,
    area: Rect,
    _request_id: &str,
    task_id: &str,
    summary: &str,
    plan: Option<&pitboss_cli::control::protocol::ApprovalPlanWire>,
    kind: pitboss_cli::control::protocol::ApprovalKind,
    sub_mode: &crate::state::ApprovalSubMode,
) {
    use crate::state::ApprovalSubMode;
    let modal_w = (area.width * 3 / 4).min(90);
    let modal_h = (area.height * 2 / 3).clamp(10, 24);
    let x = area.x + area.width.saturating_sub(modal_w) / 2;
    let y = area.y + area.height.saturating_sub(modal_h) / 2;
    let modal = Rect::new(x, y, modal_w, modal_h);
    frame.render_widget(Clear, modal);
    // Badge distinguishes pre-flight plan approvals from in-flight action
    // approvals. Operators reading the modal need to know whether
    // approving unblocks the *run* (Plan) or a *single action* (Action).
    let badge = match kind {
        pitboss_cli::control::protocol::ApprovalKind::Plan => "PRE-FLIGHT PLAN",
        pitboss_cli::control::protocol::ApprovalKind::Action => "IN-FLIGHT ACTION",
    };
    match sub_mode {
        ApprovalSubMode::Overview => {
            // When a typed plan is present, render structured fields as
            // a multi-section view. Plain summary otherwise.
            // Esc is "dismiss" (request stays pending, retrievable from
            // the approval list via `a`), NOT "cancel/reject". The old
            // label misled operators into thinking Esc aborted the
            // request; they'd then wonder why the run stayed blocked.
            let title = format!(
                " [{badge}] Approval from `{task_id}` — y=approve  n=reject  e=edit  Esc=dismiss (stays pending, press `a` to re-open) "
            );
            let block = Block::default().borders(Borders::ALL).title(title);
            let lines = plan.map_or_else(
                || {
                    vec![Line::from(Span::styled(
                        summary.to_string(),
                        theme::primary_style(),
                    ))]
                },
                |p| build_approval_plan_lines(summary, p),
            );
            let para = Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(para, modal);
        }
        ApprovalSubMode::Editing { draft } => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Edit summary — F2 / Alt+Enter / Ctrl+Enter to submit  Esc cancel ");
            let para = Paragraph::new(draft.clone())
                .block(block)
                .wrap(Wrap { trim: false })
                .style(theme::primary_style());
            frame.render_widget(para, modal);
        }
        ApprovalSubMode::Rejecting { draft } => {
            let block = Block::default().borders(Borders::ALL).title(
                " Rejection reason (optional) — F2 / Alt+Enter / Ctrl+Enter to send  Esc cancel ",
            );
            let para = Paragraph::new(draft.clone())
                .block(block)
                .wrap(Wrap { trim: false })
                .style(theme::primary_style());
            frame.render_widget(para, modal);
        }
    }
}

/// Render a typed `ApprovalPlan` as labeled sections. Section headers are
/// bold secondary; body rows are primary text; risks are highlighted in
/// the warning color so reviewers see them before approving.
fn build_approval_plan_lines(
    summary: &str,
    plan: &pitboss_cli::control::protocol::ApprovalPlanWire,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        summary.to_string(),
        theme::primary_style().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    if let Some(rationale) = &plan.rationale {
        lines.push(Line::from(Span::styled(
            "RATIONALE",
            theme::secondary_style().add_modifier(Modifier::BOLD),
        )));
        for r in rationale.lines() {
            lines.push(Line::from(Span::styled(
                format!("  {r}"),
                theme::primary_style(),
            )));
        }
        lines.push(Line::from(""));
    }
    if !plan.resources.is_empty() {
        lines.push(Line::from(Span::styled(
            "RESOURCES",
            theme::secondary_style().add_modifier(Modifier::BOLD),
        )));
        for r in &plan.resources {
            lines.push(Line::from(Span::styled(
                format!("  • {r}"),
                theme::primary_style(),
            )));
        }
        lines.push(Line::from(""));
    }
    if !plan.risks.is_empty() {
        lines.push(Line::from(Span::styled(
            "RISKS",
            Style::default()
                .fg(theme::OVERLAY_ACCENT_WARNING)
                .add_modifier(Modifier::BOLD),
        )));
        for r in &plan.risks {
            lines.push(Line::from(Span::styled(
                format!("  ! {r}"),
                Style::default().fg(theme::OVERLAY_ACCENT_WARNING),
            )));
        }
        lines.push(Line::from(""));
    }
    if let Some(rollback) = &plan.rollback {
        lines.push(Line::from(Span::styled(
            "ROLLBACK",
            theme::secondary_style().add_modifier(Modifier::BOLD),
        )));
        for r in rollback.lines() {
            lines.push(Line::from(Span::styled(
                format!("  {r}"),
                theme::primary_style(),
            )));
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn status_icon(status: &TileStatus) -> (&'static str, Color) {
    // Cancelled + ApprovalRejected share the "blocked" glyph ⊘.
    // TimedOut + ApprovalTimedOut share the clock glyph ⏱.
    // All four are semantically "actor exited because its work was blocked
    // or timed out" rather than a pure runtime error.
    let icon = match status {
        TileStatus::Pending => "…",
        TileStatus::Running => "●",
        TileStatus::Done(TaskStatus::Success) => "✓",
        TileStatus::Done(TaskStatus::Failed) => "✗",
        TileStatus::Done(TaskStatus::SpawnFailed) => "!",
        TileStatus::Done(TaskStatus::TimedOut | TaskStatus::ApprovalTimedOut) => "⏱",
        TileStatus::Done(TaskStatus::Cancelled | TaskStatus::ApprovalRejected) => "⊘",
    };
    (icon, theme::tile_status_color(status))
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
        TileStatus::Done(TaskStatus::ApprovalRejected) => "AppR",
        TileStatus::Done(TaskStatus::ApprovalTimedOut) => "AppT",
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

/// Policy editor overlay — a centred list of `[[approval_policy]]` rules with
/// an action-cycling UI. The selected rule is highlighted; all others are muted.
fn render_policy_editor(
    frame: &mut Frame,
    area: Rect,
    rules: &[pitboss_cli::mcp::policy::ApprovalRule],
    selected: usize,
) {
    let popup = centered_rect(70, 80, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Policy Editor  [j/k] nav  [Space] cycle action  [n] add  [d] del  [s/F2] save  [Esc] cancel ")
        .border_style(Style::default().fg(theme::OVERLAY_ACCENT_INFO));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if rules.is_empty() {
        let hint = Paragraph::new(" (no rules — press n to add one) ").style(theme::muted_style());
        frame.render_widget(hint, inner);
        return;
    }

    let items: Vec<ListItem> = rules
        .iter()
        .enumerate()
        .map(|(i, rule)| {
            let action_label = match rule.action {
                pitboss_cli::mcp::policy::ApprovalAction::AutoApprove => "auto_approve",
                pitboss_cli::mcp::policy::ApprovalAction::AutoReject => "auto_reject ",
                pitboss_cli::mcp::policy::ApprovalAction::Block => "block       ",
            };
            let actor = rule
                .r#match
                .actor
                .as_deref()
                .unwrap_or("*");
            let category = rule
                .r#match
                .category
                .map_or_else(|| "*".into(), |c| format!("{c:?}"));
            let tool = rule
                .r#match
                .tool_name
                .as_deref()
                .unwrap_or("*");
            let cost = rule
                .r#match
                .cost_over
                .map_or_else(|| "*".into(), |v| format!(">${v:.2}"));

            let text = format!(
                " [{action_label}]  actor:{actor:<20} cat:{category:<12} tool:{tool:<16} cost:{cost}"
            );
            let style = if i == selected {
                Style::default()
                    .fg(theme::OVERLAY_ACCENT_INFO)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(text).style(style)
        })
        .collect();

    let list = List::new(items);
    let mut list_state = ListState::default();
    list_state.select(Some(selected));
    frame.render_stateful_widget(list, inner, &mut list_state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, TileState};
    use pitboss_core::store::TaskStatus;
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
            worktree_path: None,
            completed_at: None,
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
            control_client: None,
            control_connected: false,
            cached_git_diff: std::collections::HashMap::new(),
            detail_log_viewport: std::sync::atomic::AtomicUsize::new(0),
            detail_log_total_rows: std::sync::atomic::AtomicUsize::new(0),
            runtime_handle: None,
            store_activity: std::collections::HashMap::new(),
            tile_hit_rects: std::sync::Mutex::new(Vec::new()),
            picker_hit_rects: std::sync::Mutex::new(Vec::new()),
            completed_hit_rects: std::sync::Mutex::new(Vec::new()),
            subtrees: std::collections::HashMap::new(),
            expanded: std::collections::HashMap::new(),
            focused_subtree_idx: 0,
            pane_focus: crate::state::PaneFocus::Grid,
            approval_list: crate::approval_list::ApprovalListState::default(),
            policy_rules: Vec::new(),
            completed_after_secs: crate::state::COMPLETED_COOLDOWN_DEFAULT_SECS,
            compact_tiles: false,
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
    fn short_run_id_returns_tail_of_uuidv7() {
        // Real UUIDv7 shape — want the last segment, not the time-prefix.
        assert_eq!(
            short_run_id("019da1b8-7820-7d73-92ea-146e21f77dd8"),
            "146e21f77dd8"
        );
    }

    #[test]
    fn short_run_id_no_hyphen_returns_whole_string() {
        assert_eq!(short_run_id("test-run"), "run");
        assert_eq!(short_run_id("literal"), "literal");
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
        s.mode = Mode::Detail {
            task_id: task_id.to_string(),
            scroll: 0,
            at_bottom: true,
            return_to: Box::new(Mode::Normal),
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
        s.mode = Mode::Detail {
            task_id: task_id.to_string(),
            scroll: 0,
            at_bottom: true,
            return_to: Box::new(Mode::Normal),
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
        assert_eq!(pitboss_core::prices::fmt_cost(None), "\u{2014}");
    }

    #[test]
    fn fmt_cost_two_decimal_places() {
        assert_eq!(pitboss_core::prices::fmt_cost(Some(0.867)), "$0.87");
        assert_eq!(pitboss_core::prices::fmt_cost(Some(1.00)), "$1.00");
        assert_eq!(pitboss_core::prices::fmt_cost(Some(0.00)), "$0.00");
    }

    // -----------------------------------------------------------------------
    // run_stats includes cost
    // -----------------------------------------------------------------------

    #[test]
    fn run_stats_includes_cost_for_known_model() {
        let mut t = tile(
            "a",
            TileStatus::Done(pitboss_core::store::TaskStatus::Success),
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
            TileStatus::Done(pitboss_core::store::TaskStatus::Success),
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
    fn tile_role_distinguishes_lead_from_worker() {
        // The lead is the tile whose parent_task_id is None and whose id
        // appears as a parent of at least one other tile. Replaced the old
        // `[LEAD]` text prefix with a single glyph (`★`) that renders as a
        // separate styled span in the title bar.
        let tiles = vec![
            tile("triage-lead", TileStatus::Running, None, 0, 0),
            tile_with_parent("worker-1", TileStatus::Running, Some("triage-lead".into())),
        ];
        let s = state(tiles);

        // Title is just the id now — role is communicated via glyph span.
        assert_eq!(crate::tui::format_tile_title(&s, 0), "triage-lead");
        assert_eq!(crate::tui::format_tile_title(&s, 1), "worker-1");

        assert!(crate::tui::tile_is_lead(&s, 0));
        assert!(!crate::tui::tile_is_lead(&s, 1));

        assert_eq!(crate::tui::tile_role_glyph(&s, 0), "\u{2605}"); // ★
        assert_eq!(crate::tui::tile_role_glyph(&s, 1), "\u{25B8}"); // ▸
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
