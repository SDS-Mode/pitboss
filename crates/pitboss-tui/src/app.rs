//! Event loop, input handling, and top-level TUI runner.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};

use crate::state::{AppSnapshot, AppState, Mode, PaneFocus, SortKey};
use crate::watcher;

// ---------------------------------------------------------------------------
// Channel type aliases for clarity
// ---------------------------------------------------------------------------

type SnapshotRx = mpsc::Receiver<AppSnapshot>;
type FocusTx = mpsc::Sender<String>;

/// Spawn a fresh watcher thread and return new channel endpoints.
///
/// The old `snapshot_rx` and `focus_tx` are dropped here; once the old
/// watcher tries to send on its `snapshot_tx` it will see `Err` and exit.
fn spawn_watcher(run_dir: PathBuf) -> (SnapshotRx, FocusTx) {
    let (snapshot_tx, snapshot_rx) = mpsc::sync_channel(4);
    let (focus_tx, focus_rx) = mpsc::channel::<String>();
    watcher::watch(run_dir, snapshot_tx, focus_rx);
    (snapshot_rx, focus_tx)
}

/// Run the TUI against the given run directory.
#[allow(clippy::too_many_lines)]
pub fn run(run_dir: PathBuf, run_id: String) -> anyhow::Result<()> {
    let mut terminal = crate::tui::init()?;

    let mut state = AppState::new(run_dir.clone(), run_id);

    // Build a tokio runtime for the ControlClient + its background reader task.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let (ctrl_events_tx, ctrl_events_rx) =
        std::sync::mpsc::channel::<pitboss_cli::control::protocol::ControlEvent>();

    // Connect to the run's control socket. Extracted into a closure so the
    // same path can be used on both initial startup and when the user
    // switches runs — without a fresh client, control ops after SwitchRun
    // would still target the old run's socket.
    let connect_control = |state: &mut AppState| {
        let client = match uuid::Uuid::parse_str(&state.run_id) {
            Ok(uuid) => {
                let socket_path = pitboss_cli::control::control_socket_path(uuid, &state.run_dir);
                let (bridge_tx, mut bridge_rx) =
                    tokio::sync::mpsc::channel::<pitboss_cli::control::protocol::ControlEvent>(64);
                let forward_tx = ctrl_events_tx.clone();
                runtime.spawn(async move {
                    while let Some(ev) = bridge_rx.recv().await {
                        if forward_tx.send(ev).is_err() {
                            break;
                        }
                    }
                });
                runtime
                    .block_on(crate::control::ControlClient::connect(
                        socket_path,
                        bridge_tx,
                    ))
                    .ok()
                    .map(Arc::new)
            }
            Err(_) => None,
        };
        state.control_connected = client.as_ref().is_some_and(|c| c.is_connected());
        state.control_client = client;
        state.runtime_handle = Some(runtime.handle().clone());
    };

    connect_control(&mut state);
    // `ctrl_events_tx` and `runtime` are both held by `connect_control`
    // (the closure clones the sender and spawns tasks on the runtime). They
    // have to stay alive for the full event loop so a SwitchRun can rebuild
    // the control client. Both fall out of scope at the end of `run()`.

    // Mutable channel endpoints so we can swap them when switching runs.
    let (mut snapshot_rx, mut focus_tx) = spawn_watcher(run_dir);

    // Send the initial focus (empty → watcher will tail first tile).
    let _ = focus_tx.send(String::new());

    // When true, the physical terminal may have stale cells that ratatui's
    // diff won't repaint (e.g., after a resize, focus change, or mode
    // transition — some terminal emulators don't reliably apply every cell
    // update emitted by crossterm). A `terminal.clear()` emits `\x1b[2J`
    // and resets the back buffer, forcing a clean full redraw.
    let mut dirty = false;
    loop {
        // --- Render ---
        if dirty {
            terminal.clear()?;
            dirty = false;
        }
        terminal.draw(|frame| crate::tui::render(frame, &state))?;

        // --- Input (50ms poll) ---
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Resize(_, _) => {
                    dirty = true;
                }
                Event::Key(key) => {
                    let prev_focus = state.focus;
                    let prev_mode_disc = std::mem::discriminant(&state.mode);

                    let action = handle_key(&mut state, key.code, key.modifiers);
                    match action {
                        Action::Quit => break,
                        Action::SwitchRun { run_dir, run_id } => {
                            // Restart the watcher on the new run dir.
                            let (new_rx, new_tx) = spawn_watcher(run_dir.clone());
                            snapshot_rx = new_rx;
                            focus_tx = new_tx;
                            reset_state_for_switch(&mut state, run_dir, run_id);
                            // Drain in-flight events from the old run's socket
                            // reader before connecting to the new run. The old
                            // read_loop keeps running until its Unix socket
                            // closes and still forwards through the shared
                            // ctrl_events_tx. Without this drain, stale events
                            // (including ApprovalRequest) can arrive after
                            // reset_state_for_switch cleared the approval list,
                            // pass the de-dup guard as "new", and open a modal
                            // whose request_id the new dispatcher cannot ack
                            // (#104).
                            while ctrl_events_rx.try_recv().is_ok() {}
                            // Rebuild the control client against the new run's
                            // socket; without this, post-switch control ops
                            // (cancel/pause/approve/reprompt) keep targeting
                            // the previous run.
                            connect_control(&mut state);
                            let _ = focus_tx.send(String::new());
                            dirty = true;
                            continue;
                        }
                        Action::Continue => {}
                    }

                    // Mark dirty if focus or mode changed — those are the
                    // transitions where terminal-side cell staleness shows.
                    if prev_focus != state.focus
                        || prev_mode_disc != std::mem::discriminant(&state.mode)
                    {
                        dirty = true;
                    }

                    // Notify watcher of new focus.
                    if let Some(tile) = state.focused_tile() {
                        let _ = focus_tx.send(tile.id.clone());
                    }
                }
                Event::Mouse(mouse) => {
                    let prev_focus = state.focus;
                    let prev_mode_disc = std::mem::discriminant(&state.mode);
                    let action = handle_mouse(&mut state, mouse);
                    match action {
                        Action::Quit => break,
                        Action::SwitchRun { run_dir, run_id } => {
                            // Same transition as the keyboard-driven
                            // SwitchRun path above — restart the watcher,
                            // rebuild the control client, and reset
                            // run-local state via the shared helper so mouse
                            // and key paths can't drift on per-run cleanup
                            // (e.g. missing approval_list clear — #95).
                            let (new_rx, new_tx) = spawn_watcher(run_dir.clone());
                            snapshot_rx = new_rx;
                            focus_tx = new_tx;
                            reset_state_for_switch(&mut state, run_dir, run_id);
                            while ctrl_events_rx.try_recv().is_ok() {} // #104
                            connect_control(&mut state);
                            let _ = focus_tx.send(String::new());
                            dirty = true;
                            continue;
                        }
                        Action::Continue => {}
                    }

                    // Mouse clicks can change focus + mode (tile click →
                    // Detail, right-click → exit Detail). Mirror the
                    // key-path's post-handler bookkeeping so the watcher
                    // tails the new focus and the terminal clears on
                    // mode transitions.
                    if prev_focus != state.focus
                        || prev_mode_disc != std::mem::discriminant(&state.mode)
                    {
                        dirty = true;
                    }
                    if let Some(tile) = state.focused_tile() {
                        let _ = focus_tx.send(tile.id.clone());
                    }
                }
                _ => {}
            }
        }

        // --- Snapshot from watcher (non-blocking) ---
        // Drain: if multiple snapshots queued up while the main loop was
        // busy (e.g. a slow render tick), keep the most recent and drop the
        // stale ones. This also prevents the 4-capacity channel from
        // staying full and tripping the watcher's backpressure path.
        let mut latest_snapshot: Option<AppSnapshot> = None;
        while let Ok(snapshot) = snapshot_rx.try_recv() {
            latest_snapshot = Some(snapshot);
        }
        if let Some(snapshot) = latest_snapshot {
            state.apply_snapshot(snapshot);
            // If we're in snap-in mode and at the bottom, keep the view
            // scrolled to the last line as new log lines arrive.
            if matches!(state.mode, Mode::Detail { .. }) {
                state.detail_auto_scroll(DETAIL_VISIBLE_ROWS);
            }
            // Notify watcher of current focus after snapshot (may have changed
            // via ensure_active_focus or Completed→Normal fallback).
            if let Some(tile) = state.focused_tile() {
                let _ = focus_tx.send(tile.id.clone());
            }
        }

        // --- Drain any queued control events (non-blocking). ---
        while let Ok(ev) = ctrl_events_rx.try_recv() {
            apply_control_event(&mut state, ev);
        }
    }

    crate::tui::teardown(&mut terminal)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Action returned from key handlers
// ---------------------------------------------------------------------------

enum Action {
    Continue,
    Quit,
    SwitchRun { run_dir: PathBuf, run_id: String },
}

/// Reset all per-run state when the operator switches runs from the picker.
/// Leaves non-run state (runtime handle, `control_client` slot which the caller
/// rebuilds) untouched. Kept as a free function so the `SwitchRun` path can be
/// unit-tested without running the full event loop.
fn reset_state_for_switch(state: &mut AppState, run_dir: PathBuf, run_id: String) {
    state.run_dir = run_dir;
    state.run_id = run_id;
    state.tasks = vec![];
    state.focus = 0;
    state.run_list.clear();
    state.mode = Mode::Normal;
    // Pending approvals from the prior run's dispatcher reference request_ids
    // that no longer exist on the new run's bridge. Leaving them in the list
    // makes the approval-list pane lie about state; the new server will
    // re-emit its own pending approvals on Hello and those must land cleanly.
    state.approval_list.items.clear();
    state.approval_list.selected_idx = 0;
    state.policy_rules.clear();
    state.subtrees.clear();
    state.expanded.clear();
    state.focused_subtree_idx = 0;
    state.pane_focus = crate::state::PaneFocus::Grid;
    state.store_activity.clear();
    state.cached_git_diff.clear();
    state.failed_count = 0;
    state.run_started_at = None;
    state.focus_log.clear();
    // Clear before connect_control overwrites it; prevents a stale indicator
    // from showing on the status bar during the one render tick between this
    // reset and the new connection being established (#113).
    state.control_connected = false;
    // Clear completed-page hit cache so stale rects from the old run don't
    // produce phantom clicks on the new run's layout.
    if let Ok(mut rects) = state.completed_hit_rects.lock() {
        rects.clear();
    }
}

/// The visible height used for snap-in scroll calculations.
///
/// We can't query the terminal size from the key handler, so we use a
/// representative constant. The real render uses `area.height`, but
/// scroll clamping is soft (extra scroll just shows blank) so this is fine
/// for the handler. The render pass also calls `detail_auto_scroll` with the
/// real `visible_rows`.
const DETAIL_VISIBLE_ROWS: usize = 40;

/// Handle a single mouse event. Returns the same [`Action`] type as
/// `handle_key` so the event-loop's existing `SwitchRun` dispatch can
/// handle picker-click → open-run in one place.
fn handle_mouse(state: &mut AppState, mouse: crossterm::event::MouseEvent) -> Action {
    match (state.mode.clone(), mouse.kind) {
        // Wheel scroll inside Detail view — 5 rows/tick, matches J/K
        // shift-scroll cadence. Exit-by-overscroll was tried briefly
        // in #114 but removed — too easy to accidentally exit while
        // reading the top of a log. Right-click and Esc remain the
        // explicit exit gestures.
        (Mode::Detail { .. }, MouseEventKind::ScrollUp) => {
            state.detail_scroll_up(5);
        }
        (Mode::Detail { .. }, MouseEventKind::ScrollDown) => {
            state.detail_scroll_down(5, DETAIL_VISIBLE_ROWS);
        }
        // Right-click inside Detail view exits to the return_to mode.
        (Mode::Detail { .. }, MouseEventKind::Down(MouseButton::Right)) => {
            state.exit_detail();
        }
        // Left-click on a completed table row: open Detail for that task.
        (Mode::Completed { .. }, MouseEventKind::Down(MouseButton::Left)) => {
            let task_id = state.completed_hit_rects.lock().ok().and_then(|rects| {
                rects.iter().find_map(|&(task_idx, r)| {
                    if mouse.column >= r.x
                        && mouse.column < r.x + r.width
                        && mouse.row >= r.y
                        && mouse.row < r.y + r.height
                    {
                        Some(state.tasks[task_idx].id.clone())
                    } else {
                        None
                    }
                })
            });
            if let Some(task_id) = task_id {
                state.enter_detail_for(task_id);
            }
        }
        // Left-click on a tile in the grid: focus + enter Detail
        // (equivalent to hjkl + Enter). Clicks on the already-focused
        // tile also enter Detail.
        (Mode::Normal, MouseEventKind::Down(MouseButton::Left)) => {
            if let Some(idx) = state.tile_at(mouse.column, mouse.row) {
                state.focus = idx;
                if let Some(tile) = state.focused_tile() {
                    // The focus_tx is owned by run(); we can't touch it
                    // from this helper. Caller observes Action::Continue
                    // + re-reads focused_tile in the normal focus-notify
                    // path at the top of the event loop.
                    let _ = tile;
                }
                state.enter_detail();
            }
        }
        // Wheel scroll over a tile in the grid: "zoom in" — focus the
        // tile under the cursor and enter Detail. Either scroll direction
        // performs the gesture. Scroll outside any tile is a no-op. The
        // symmetric "zoom out" is the scroll-up-at-top branch above.
        (Mode::Normal, MouseEventKind::ScrollDown | MouseEventKind::ScrollUp) => {
            if let Some(idx) = state.tile_at(mouse.column, mouse.row) {
                state.focus = idx;
                state.enter_detail();
            }
        }
        // Left-click on a picker row: open that run (equivalent to
        // highlighting + pressing Enter). The event-loop dispatches
        // `SwitchRun` — same transition as keyboard select.
        (Mode::PickingRun { .. }, MouseEventKind::Down(MouseButton::Left)) => {
            if let Some(idx) = state.picker_row_at(mouse.column, mouse.row) {
                if let Some(entry) = state.run_list.get(idx) {
                    return Action::SwitchRun {
                        run_dir: entry.run_dir.clone(),
                        run_id: entry.run_id.clone(),
                    };
                }
            }
        }
        _ => {}
    }
    Action::Continue
}

/// Handle a single key press. Returns an [`Action`] describing what to do next.
fn handle_key(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> Action {
    // Ctrl-C always quits.
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        return Action::Quit;
    }

    match state.mode {
        Mode::Normal => handle_normal(state, code),
        Mode::Help => handle_help(state, code),
        Mode::PickingRun { .. } => handle_picking_run(state, code),
        Mode::Detail { .. } => handle_detail(state, code, modifiers),
        Mode::ConfirmKill { .. } => handle_confirm_kill(state, code),
        Mode::PromptReprompt { .. } => handle_prompt_reprompt(state, code, modifiers),
        Mode::ApprovalModal { .. } => handle_approval_modal(state, code, modifiers),
        Mode::PolicyEditor { .. } => handle_policy_editor(state, code),
        Mode::Completed { .. } => handle_completed(state, code),
    }
}

#[allow(clippy::too_many_lines)]
fn handle_normal(state: &mut AppState, code: KeyCode) -> Action {
    // When the approval list pane is focused, Up/Down/Enter navigate it;
    // Esc returns focus to the grid. All other keys fall through to the
    // grid handler below so global shortcuts (q, ?, o, …) still work.
    if state.pane_focus == PaneFocus::ApprovalList {
        match code {
            KeyCode::Down | KeyCode::Char('j') => {
                state.approval_list.move_selection_down();
                return Action::Continue;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                state.approval_list.move_selection_up();
                return Action::Continue;
            }
            KeyCode::Enter => {
                if let Some(item) = state.approval_list.current().cloned() {
                    // Preserve plan + kind so a dismissed-and-reopened
                    // `propose_plan` approval still shows its structured
                    // plan view and its "PRE-FLIGHT PLAN" badge. Before
                    // the list learned to carry those, re-opens lost the
                    // payload and the operator got a less-informative
                    // modal than the first presentation.
                    state.mode = Mode::ApprovalModal {
                        request_id: item.id.clone(),
                        task_id: item.actor_path.clone(),
                        summary: item.summary.clone(),
                        plan: item.plan.clone(),
                        kind: item.kind,
                        sub_mode: crate::state::ApprovalSubMode::Overview,
                    };
                }
                return Action::Continue;
            }
            KeyCode::Esc => {
                state.pane_focus = PaneFocus::Grid;
                return Action::Continue;
            }
            _ => {}
        }
    }

    match code {
        // Quit
        KeyCode::Char('q') => return Action::Quit,

        // Tab: cycle grouped-grid focus across containers (root → S1 → S2 → root…).
        // Only meaningful for depth-2 runs; no-op cost for depth-1.
        KeyCode::Tab => {
            state.cycle_focus_to_next_subtree();
        }

        // Focus the approval list pane.
        KeyCode::Char('A') => {
            state.pane_focus = PaneFocus::ApprovalList;
        }

        // Enter the Completed page (only if any promoted tiles exist).
        KeyCode::Char('C') => {
            let completed = state.completed_tile_indices();
            if !completed.is_empty() {
                let first_id = state.tasks[completed[0]].id.clone();
                state.mode = Mode::Completed {
                    selected_task_id: first_id,
                    scroll_offset: 0,
                    sort_key: SortKey::EndedAtDesc,
                    filter_status: None,
                };
            }
        }

        // Toggle compact 2-line tile rendering on the Active grid.
        KeyCode::Char('v') => {
            state.compact_tiles = !state.compact_tiles;
        }

        // Navigation
        KeyCode::Char('h') | KeyCode::Left => state.focus_left(),
        KeyCode::Char('l') | KeyCode::Right => state.focus_right(),
        KeyCode::Char('k') | KeyCode::Up => state.focus_up(),
        KeyCode::Char('j') | KeyCode::Down => state.focus_down(),

        // Help overlay
        KeyCode::Char('?') => state.mode = Mode::Help,

        // Run picker
        KeyCode::Char('o') => state.enter_picker(),

        // Enter: toggle sub-tree collapse if a header is focused; otherwise snap-in.
        KeyCode::Enter if state.focused_subtree_header() => {
            if let Some(sublead_id) = state.focused_sublead_id() {
                let cur = state.expanded.get(&sublead_id).copied().unwrap_or(true);
                state.expanded.insert(sublead_id, !cur);
            }
        }

        // Snap-in: enter full-screen view for the focused tile.
        KeyCode::Enter => state.enter_detail(),

        // v0.4 — cancel focused worker.
        KeyCode::Char('x') => {
            if let Some(tile) = state.focused_tile() {
                state.mode = Mode::ConfirmKill {
                    target: crate::state::KillTarget::Worker(tile.id.clone()),
                };
            }
        }
        // v0.4 — cancel entire run.
        KeyCode::Char('X') => {
            state.mode = Mode::ConfirmKill {
                target: crate::state::KillTarget::Run,
            };
        }

        // v0.4 — pause focused worker.
        KeyCode::Char('p') => {
            if let Some(tile) = state.focused_tile().cloned() {
                spawn_control_op(
                    state,
                    pitboss_cli::control::protocol::ControlOp::PauseWorker {
                        task_id: tile.id,
                        mode: pitboss_cli::control::protocol::PauseMode::default(),
                    },
                );
            }
        }
        // v0.4 — continue focused worker (if paused).
        KeyCode::Char('c') => {
            if let Some(tile) = state.focused_tile().cloned() {
                spawn_control_op(
                    state,
                    pitboss_cli::control::protocol::ControlOp::ContinueWorker {
                        task_id: tile.id,
                        prompt: None,
                    },
                );
            }
        }

        // v0.4 — reprompt focused worker.
        KeyCode::Char('r') => {
            if let Some(tile) = state.focused_tile().cloned() {
                state.mode = Mode::PromptReprompt {
                    task_id: tile.id,
                    draft: String::new(),
                };
            }
        }

        // v0.8 — open policy editor overlay.
        KeyCode::Char('P') => {
            state.enter_policy_editor();
        }

        // Refresh — watcher already polls every 500ms; render at loop top
        // covers forced redraw. All other keys are intentionally ignored.
        _ => {}
    }
    Action::Continue
}

fn handle_detail(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> Action {
    match code {
        // Exit back to grid.
        KeyCode::Esc => state.exit_detail(),

        // Quit the whole app.
        KeyCode::Char('q') => return Action::Quit,

        // Scroll down one line.
        KeyCode::Char('j') | KeyCode::Down => state.detail_scroll_down(1, DETAIL_VISIBLE_ROWS),

        // Scroll up one line.
        KeyCode::Char('k') | KeyCode::Up => state.detail_scroll_up(1),

        // Medium-speed scroll: 5 lines. Sits between single-line j/k and
        // half-page Ctrl-D/U. Shift-variants of the vim keys keep muscle
        // memory intact for users who live on j/k=1.
        KeyCode::Char('J') => state.detail_scroll_down(5, DETAIL_VISIBLE_ROWS),
        KeyCode::Char('K') => state.detail_scroll_up(5),

        // Page down (Ctrl-D or PageDown).
        KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
            state.detail_scroll_down(10, DETAIL_VISIBLE_ROWS);
        }
        KeyCode::PageDown => state.detail_scroll_down(10, DETAIL_VISIBLE_ROWS),

        // Page up (Ctrl-U or PageUp).
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            state.detail_scroll_up(10);
        }
        KeyCode::PageUp => state.detail_scroll_up(10),

        // Jump to bottom, re-enable auto-scroll.
        KeyCode::Char('G') => state.detail_jump_bottom(DETAIL_VISIBLE_ROWS),

        // Jump to top, disable auto-scroll.
        KeyCode::Char('g') => state.detail_jump_top(),

        _ => {}
    }
    Action::Continue
}

#[allow(clippy::too_many_lines)]
fn handle_completed(state: &mut AppState, code: KeyCode) -> Action {
    // Compute completed indices before borrowing state.mode mutably.
    let completed = state.completed_tile_indices();
    let count = completed.len();

    let Mode::Completed {
        ref selected_task_id,
        ref mut scroll_offset,
        ref mut sort_key,
        ref mut filter_status,
    } = state.mode
    else {
        return Action::Continue;
    };
    // Resolve the current selection to a position within the completed list.
    let cur_pos = completed
        .iter()
        .position(|&i| state.tasks[i].id == *selected_task_id)
        .unwrap_or(0);

    match code {
        // Exit back to Active grid.
        KeyCode::Char('A' | 'q') | KeyCode::Esc => {
            state.mode = Mode::Normal;
            return Action::Continue;
        }

        // Enter Detail for the selected tile.
        KeyCode::Enter => {
            if let Some(&task_idx) = completed.get(cur_pos) {
                let task_id = state.tasks[task_idx].id.clone();
                state.enter_detail_for(task_id);
                return Action::Continue;
            }
        }

        // Navigation — move selection down.
        KeyCode::Char('j') | KeyCode::Down => {
            if count > 0 {
                let new_pos = (cur_pos + 1).min(count - 1);
                let task_id = state.tasks[completed[new_pos]].id.clone();
                if let Mode::Completed {
                    ref mut selected_task_id,
                    ref mut scroll_offset,
                    ..
                } = state.mode
                {
                    *selected_task_id = task_id;
                    // Scroll viewport to follow selection.
                    if new_pos >= *scroll_offset + 40 {
                        *scroll_offset = new_pos.saturating_sub(39);
                    }
                }
            }
            return Action::Continue;
        }

        // Navigation — move selection up.
        KeyCode::Char('k') | KeyCode::Up => {
            if count > 0 {
                let new_pos = cur_pos.saturating_sub(1);
                let task_id = state.tasks[completed[new_pos]].id.clone();
                if let Mode::Completed {
                    ref mut selected_task_id,
                    ref mut scroll_offset,
                    ..
                } = state.mode
                {
                    *selected_task_id = task_id;
                    if new_pos < *scroll_offset {
                        *scroll_offset = new_pos;
                    }
                }
            }
            return Action::Continue;
        }

        // Jump to top.
        KeyCode::Char('g') => {
            if count > 0 {
                let task_id = state.tasks[completed[0]].id.clone();
                if let Mode::Completed {
                    ref mut selected_task_id,
                    ref mut scroll_offset,
                    ..
                } = state.mode
                {
                    *selected_task_id = task_id;
                    *scroll_offset = 0;
                }
            }
            return Action::Continue;
        }

        // Jump to bottom.
        KeyCode::Char('G') => {
            if count > 0 {
                let last = count - 1;
                let task_id = state.tasks[completed[last]].id.clone();
                if let Mode::Completed {
                    ref mut selected_task_id,
                    ref mut scroll_offset,
                    ..
                } = state.mode
                {
                    *selected_task_id = task_id;
                    *scroll_offset = last.saturating_sub(39);
                }
            }
            return Action::Continue;
        }

        // Cycle sort key: EndedAtDesc → DurationDesc → StatusAsc → EndedAtDesc
        KeyCode::Char('s') => {
            if let Mode::Completed {
                ref mut sort_key, ..
            } = state.mode
            {
                *sort_key = match sort_key {
                    SortKey::EndedAtDesc => SortKey::DurationDesc,
                    SortKey::DurationDesc => SortKey::StatusAsc,
                    SortKey::StatusAsc => SortKey::EndedAtDesc,
                };
            }
            return Action::Continue;
        }

        // Quit the whole app.
        KeyCode::Char('Q') => return Action::Quit,

        _ => {}
    }

    // Suppress unused warnings from the borrow of sort_key / filter_status.
    let _ = (sort_key, filter_status, scroll_offset);
    Action::Continue
}

fn handle_help(state: &mut AppState, code: KeyCode) -> Action {
    match code {
        KeyCode::Char('?') | KeyCode::Esc => state.mode = Mode::Normal,
        KeyCode::Char('q') => return Action::Quit,
        _ => {}
    }
    Action::Continue
}

fn handle_picking_run(state: &mut AppState, code: KeyCode) -> Action {
    let Mode::PickingRun { selected } = state.mode else {
        return Action::Continue;
    };

    match code {
        // Navigate
        KeyCode::Char('j') | KeyCode::Down => state.picker_down(),
        KeyCode::Char('k') | KeyCode::Up => state.picker_up(),

        // Cancel
        KeyCode::Esc => state.cancel_picker(),

        // Quit whole TUI
        KeyCode::Char('q') => return Action::Quit,

        // Select
        KeyCode::Enter => {
            if let Some(entry) = state.run_list.get(selected) {
                let run_dir = entry.run_dir.clone();
                let run_id = entry.run_id.clone();
                return Action::SwitchRun { run_dir, run_id };
            }
            // Nothing selected (empty list) — just cancel.
            state.cancel_picker();
        }

        // 'o' while already picking is a no-op.
        _ => {}
    }
    Action::Continue
}

/// Apply a single control-socket event to the app state. Called for each event
/// drained from the async-to-sync bridge channel once per event-loop tick.
fn apply_control_event(state: &mut AppState, ev: pitboss_cli::control::protocol::ControlEvent) {
    use crate::state::SubtreeView;
    use pitboss_cli::control::protocol::ControlEvent as E;
    match ev {
        E::Hello { policy_rules, .. } => {
            // Sync our local rule cache from the dispatcher's snapshot.
            state.policy_rules = policy_rules;
        }
        E::ApprovalRequest {
            request_id,
            task_id,
            summary,
            plan,
            kind,
        } => {
            // Push into the approval-list queue in addition to opening the
            // modal. Without this, hitting Esc to dismiss the modal
            // stranded the request: it stayed pending server-side but
            // nothing in the TUI referenced it, so `'a'` opened an
            // empty list pane and the operator had no retrieval path.
            // De-dup by id so the same request never lands twice (can
            // happen if a server restart replays events).
            let actor_path = if task_id.is_empty() {
                "root".to_string()
            } else {
                task_id.clone()
            };
            let category = match kind {
                pitboss_cli::control::protocol::ApprovalKind::Plan => "plan",
                pitboss_cli::control::protocol::ApprovalKind::Action => "action",
            }
            .to_string();
            if !state.approval_list.items.iter().any(|i| i.id == request_id) {
                state
                    .approval_list
                    .items
                    .push_back(crate::state::ApprovalListItem {
                        id: request_id.clone(),
                        actor_path,
                        category,
                        summary: summary.clone(),
                        plan: plan.clone(),
                        kind,
                        created_at: chrono::Utc::now(),
                    });
            }
            state.mode = Mode::ApprovalModal {
                request_id,
                task_id,
                summary,
                plan,
                kind,
                sub_mode: crate::state::ApprovalSubMode::Overview,
            };
        }
        E::Superseded | E::RunFinished { .. } => {
            state.control_connected = false;
        }
        E::StoreActivity { counters } => {
            // Rebuild the map from scratch each broadcast — the server
            // sends the full snapshot, so we don't need to merge.
            state.store_activity = counters
                .into_iter()
                .map(|e| {
                    (
                        e.actor_id,
                        crate::state::StoreActivityCounters {
                            kv_ops: e.kv_ops,
                            lease_ops: e.lease_ops,
                        },
                    )
                })
                .collect();
        }
        // Sub-lead lifecycle: create/destroy grouped-grid containers.
        E::SubleadSpawned {
            sublead_id,
            budget_usd,
            read_down,
            ..
        } => {
            state.subtrees.insert(
                sublead_id.clone(),
                SubtreeView {
                    workers: std::collections::HashMap::new(),
                    spent_usd: 0.0,
                    budget_usd,
                    pending_approvals: 0,
                    read_down,
                },
            );
            // Expand by default on spawn.
            state.expanded.insert(sublead_id, true);
        }
        E::SubleadTerminated { sublead_id, .. } => {
            state.subtrees.remove(&sublead_id);
            state.expanded.remove(&sublead_id);
        }
        _ => {}
    }
}

fn handle_confirm_kill(state: &mut AppState, code: KeyCode) -> Action {
    let Mode::ConfirmKill { target } = state.mode.clone() else {
        return Action::Continue;
    };
    match code {
        KeyCode::Char('y' | 'Y') => {
            let op = match target {
                crate::state::KillTarget::Worker(id) => {
                    pitboss_cli::control::protocol::ControlOp::CancelWorker { task_id: id }
                }
                crate::state::KillTarget::Run => {
                    pitboss_cli::control::protocol::ControlOp::CancelRun
                }
            };
            spawn_control_op(state, op);
            state.mode = Mode::Normal;
        }
        _ => state.mode = Mode::Normal,
    }
    Action::Continue
}

fn handle_prompt_reprompt(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> Action {
    let Mode::PromptReprompt { task_id, draft } = state.mode.clone() else {
        return Action::Continue;
    };
    let mut draft = draft;
    match code {
        KeyCode::Esc => {
            state.mode = Mode::Normal;
            return Action::Continue;
        }
        // Submit the reprompt. Multiple accepted chords because most
        // terminal emulators (konsole, gnome-terminal, default VTE-based
        // terminals, default xterm/alacritty/tilix without CSI-u /
        // kitty-keyboard) DON'T distinguish Ctrl+Enter from Enter —
        // crossterm only sees `Enter` with no CTRL modifier, and we'd
        // silently swallow the submission. F2 is unambiguous across
        // every terminal I know of; Alt+Enter is reliably distinct in
        // most terminals because the Alt/Meta modifier survives even
        // when Ctrl doesn't. Kept Ctrl+Enter for the terminals that
        // DO route it (kitty with keyboard protocol, wezterm, iTerm2).
        KeyCode::F(2) | KeyCode::Enter
            if matches!(code, KeyCode::F(2))
                || modifiers.contains(KeyModifiers::CONTROL)
                || modifiers.contains(KeyModifiers::ALT) =>
        {
            if !draft.is_empty() {
                spawn_control_op(
                    state,
                    pitboss_cli::control::protocol::ControlOp::RepromptWorker {
                        task_id: task_id.clone(),
                        prompt: draft.clone(),
                    },
                );
            }
            state.mode = Mode::Normal;
            return Action::Continue;
        }
        KeyCode::Char(c) => draft.push(c),
        KeyCode::Backspace => {
            draft.pop();
        }
        KeyCode::Enter => draft.push('\n'),
        _ => {}
    }
    state.mode = Mode::PromptReprompt { task_id, draft };
    Action::Continue
}

/// Non-draft fields of `Mode::ApprovalModal`, shared across sub-mode handlers.
struct ApprovalCtx {
    request_id: String,
    task_id: String,
    summary: String,
    plan: Option<pitboss_cli::control::protocol::ApprovalPlanWire>,
    kind: pitboss_cli::control::protocol::ApprovalKind,
}

fn handle_approval_modal(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> Action {
    use crate::state::ApprovalSubMode;
    let Mode::ApprovalModal {
        request_id,
        task_id,
        summary,
        plan,
        kind,
        sub_mode,
    } = state.mode.clone()
    else {
        return Action::Continue;
    };
    let ctx = ApprovalCtx {
        request_id,
        task_id,
        summary,
        plan,
        kind,
    };

    match sub_mode {
        ApprovalSubMode::Overview => handle_approval_overview(state, code, ctx),
        ApprovalSubMode::Editing { draft } => {
            handle_approval_draft(state, code, modifiers, ctx, draft, true);
        }
        ApprovalSubMode::Rejecting { draft } => {
            handle_approval_draft(state, code, modifiers, ctx, draft, false);
        }
    }
    Action::Continue
}

fn handle_approval_overview(state: &mut AppState, code: KeyCode, ctx: ApprovalCtx) {
    use crate::state::ApprovalSubMode;
    match code {
        KeyCode::Char('y') => {
            send_approve(state, &ctx.request_id, true, None, None);
            state.mode = Mode::Normal;
        }
        KeyCode::Char('n') => {
            state.mode = Mode::ApprovalModal {
                request_id: ctx.request_id,
                task_id: ctx.task_id,
                summary: ctx.summary,
                plan: ctx.plan,
                kind: ctx.kind,
                sub_mode: ApprovalSubMode::Rejecting {
                    draft: String::new(),
                },
            };
        }
        KeyCode::Char('e') => {
            let draft = ctx.summary.clone();
            state.mode = Mode::ApprovalModal {
                request_id: ctx.request_id,
                task_id: ctx.task_id,
                summary: ctx.summary,
                plan: ctx.plan,
                kind: ctx.kind,
                sub_mode: ApprovalSubMode::Editing { draft },
            };
        }
        KeyCode::Esc => {
            // Dismiss the modal without responding. The request stays
            // pending server-side and is discoverable via the approval-
            // list pane we just dropped the user into. Previously Esc
            // transitioned to `Mode::Normal` with grid focus and the
            // queue was invisible — the request looked lost.
            state.mode = Mode::Normal;
            state.pane_focus = PaneFocus::ApprovalList;
            if let Some(pos) = state
                .approval_list
                .items
                .iter()
                .position(|i| i.id == ctx.request_id)
            {
                state.approval_list.selected_idx = pos;
            }
        }
        _ => {}
    }
}

/// Shared draft-editing handler for both the `Editing` and `Rejecting`
/// sub-modes. `editing` distinguishes which branch we're in: `true` means
/// an edit-summary draft (Ctrl+Enter sends approve with `edited_summary`);
/// `false` means a rejection-comment draft (Ctrl+Enter sends reject with
/// `comment`).
fn handle_approval_draft(
    state: &mut AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
    ctx: ApprovalCtx,
    mut draft: String,
    editing: bool,
) {
    use crate::state::ApprovalSubMode;
    match code {
        // Submit. Same multi-chord fallback as the reprompt modal — see
        // handle_prompt_reprompt for the full rationale. Ctrl+Enter is
        // often swallowed by VTE-based terminals; F2 and Alt+Enter
        // survive.
        KeyCode::F(2) | KeyCode::Enter
            if matches!(code, KeyCode::F(2))
                || modifiers.contains(KeyModifiers::CONTROL)
                || modifiers.contains(KeyModifiers::ALT) =>
        {
            if editing {
                send_approve(state, &ctx.request_id, true, None, Some(draft));
            } else {
                send_approve(state, &ctx.request_id, false, Some(draft), None);
            }
            state.mode = Mode::Normal;
            return;
        }
        KeyCode::Esc => {
            // Dismiss draft — request stays pending, visible in the
            // approval list pane. Matches the Overview Esc behavior so
            // the operator gets one consistent "escape" contract at
            // every modal sub-mode.
            state.mode = Mode::Normal;
            state.pane_focus = PaneFocus::ApprovalList;
            if let Some(pos) = state
                .approval_list
                .items
                .iter()
                .position(|i| i.id == ctx.request_id)
            {
                state.approval_list.selected_idx = pos;
            }
            return;
        }
        KeyCode::Char(c) => draft.push(c),
        KeyCode::Backspace => {
            draft.pop();
        }
        KeyCode::Enter => draft.push('\n'),
        _ => return,
    }
    let sub_mode = if editing {
        ApprovalSubMode::Editing { draft }
    } else {
        ApprovalSubMode::Rejecting { draft }
    };
    state.mode = Mode::ApprovalModal {
        request_id: ctx.request_id,
        task_id: ctx.task_id,
        summary: ctx.summary,
        plan: ctx.plan,
        kind: ctx.kind,
        sub_mode,
    };
}

/// Handle key presses inside the policy-editor overlay.
///
/// Navigation: j/k or arrow keys move the selection.
/// Mutation:
///   Space / Enter  — cycle the selected rule's action
///                    (`AutoApprove` → `AutoReject` → `Block` → `AutoApprove`).
///   n              — append a blank catch-all / `AutoApprove` rule.
///   d              — delete the selected rule.
///   s / F2         — send `UpdatePolicy` and close (saves to server).
///   Esc            — cancel without saving.
fn handle_policy_editor(state: &mut AppState, code: KeyCode) -> Action {
    use pitboss_cli::mcp::policy::{ApprovalAction, ApprovalMatch, ApprovalRule};

    let Mode::PolicyEditor {
        ref mut rules,
        ref mut selected,
    } = state.mode
    else {
        return Action::Continue;
    };

    match code {
        // Navigation.
        KeyCode::Char('j') | KeyCode::Down if !rules.is_empty() => {
            *selected = (*selected + 1) % rules.len();
        }
        KeyCode::Char('k') | KeyCode::Up if !rules.is_empty() => {
            if *selected == 0 {
                *selected = rules.len() - 1;
            } else {
                *selected -= 1;
            }
        }

        // Cycle action of the selected rule.
        KeyCode::Char(' ') | KeyCode::Enter if !rules.is_empty() => {
            let idx = *selected;
            let next = match rules[idx].action {
                ApprovalAction::AutoApprove => ApprovalAction::AutoReject,
                ApprovalAction::AutoReject => ApprovalAction::Block,
                ApprovalAction::Block => ApprovalAction::AutoApprove,
            };
            rules[idx].action = next;
        }

        // Append a blank catch-all rule.
        KeyCode::Char('n') => {
            rules.push(ApprovalRule {
                r#match: ApprovalMatch::default(),
                action: ApprovalAction::AutoApprove,
            });
            *selected = rules.len() - 1;
        }

        // Delete selected rule.
        KeyCode::Char('d') if !rules.is_empty() => {
            let idx = *selected;
            rules.remove(idx);
            if !rules.is_empty() && *selected >= rules.len() {
                *selected = rules.len() - 1;
            }
        }

        // Save and close.
        KeyCode::Char('s') | KeyCode::F(2) => {
            let rules_to_send = rules.clone();
            // Update our local cache so the editor re-opens with the latest
            // rules rather than the stale snapshot from connect time.
            state.policy_rules.clone_from(&rules_to_send);
            state.mode = Mode::Normal;
            spawn_control_op(
                state,
                pitboss_cli::control::protocol::ControlOp::UpdatePolicy {
                    rules: rules_to_send,
                },
            );
            return Action::Continue;
        }

        // Cancel without saving.
        KeyCode::Esc | KeyCode::Char('q') => {
            state.mode = Mode::Normal;
        }

        _ => {}
    }
    Action::Continue
}

fn send_approve(
    state: &mut AppState,
    request_id: &str,
    approved: bool,
    comment: Option<String>,
    edited_summary: Option<String>,
) {
    // When rejecting, forward the comment text also via the `reason` field
    // introduced in Task 4.3 so the requesting actor receives the rejection
    // rationale in `ApprovalResponse.reason`.
    let reason = if approved { None } else { comment.clone() };
    spawn_control_op(
        state,
        pitboss_cli::control::protocol::ControlOp::Approve {
            request_id: request_id.to_string(),
            approved,
            comment,
            edited_summary,
            reason,
        },
    );
    // Drop from the approval-list queue optimistically — the server will
    // ack the op before any subsequent ApprovalRequest with the same id
    // could arrive, so a race that re-inserts a stale entry isn't
    // reachable. Clamp `selected_idx` so an out-of-range index can't
    // persist past the removal.
    if let Some(pos) = state
        .approval_list
        .items
        .iter()
        .position(|i| i.id == request_id)
    {
        state.approval_list.items.remove(pos);
    }
    let len = state.approval_list.items.len();
    if len == 0 {
        state.approval_list.selected_idx = 0;
    } else if state.approval_list.selected_idx >= len {
        state.approval_list.selected_idx = len - 1;
    }
}

// Fire-and-forget dispatch of a control op onto the runtime the
// `ControlClient` was built under. Earlier versions built a fresh
// `new_current_thread()` runtime per call — that ran the socket write
// through a reactor that didn't own the writer half, so every op hung
// silently. We now spawn on the original handle (kept alive in AppState).
// If either the client or runtime handle is missing (observe-only mode,
// tests), the call is a no-op.
fn spawn_control_op(state: &AppState, op: pitboss_cli::control::protocol::ControlOp) {
    let (Some(client), Some(handle)) =
        (state.control_client.clone(), state.runtime_handle.as_ref())
    else {
        return;
    };
    // Detach the JoinHandle — fire-and-forget send; send_op handles its own errors.
    std::mem::drop(handle.spawn(async move {
        let _ = client.send_op(op).await;
    }));
}

#[cfg(test)]
mod approval_queue_tests {
    //! Regression coverage for the "Esc on approval modal strands the
    //! request" papercut. Before these fixes, `ApprovalRequest` events
    //! only opened the modal — they never populated the approval list
    //! pane — so the moment the operator dismissed the modal the
    //! request became unreachable from the TUI.

    use super::*;
    use pitboss_cli::control::protocol::{ApprovalKind, ControlEvent};

    fn fresh_state() -> AppState {
        AppState::new(PathBuf::from("/tmp/t"), "test-run".to_string())
    }

    fn mk_approval_event(request_id: &str, task_id: &str) -> ControlEvent {
        ControlEvent::ApprovalRequest {
            request_id: request_id.to_string(),
            task_id: task_id.to_string(),
            summary: "destructive rm -rf /".to_string(),
            plan: None,
            kind: ApprovalKind::Action,
        }
    }

    #[test]
    fn approval_request_populates_list_and_opens_modal() {
        let mut state = fresh_state();
        apply_control_event(&mut state, mk_approval_event("req-A", "root"));
        assert_eq!(state.approval_list.items.len(), 1);
        assert_eq!(state.approval_list.items[0].id, "req-A");
        assert!(matches!(state.mode, Mode::ApprovalModal { .. }));
    }

    #[test]
    fn empty_task_id_renders_as_root_actor_path() {
        // Server emits `task_id: ""` for the root lead. An empty
        // actor_path would render weirdly in the list line ("[]" prefix),
        // so we substitute a stable label at event-receive time.
        let mut state = fresh_state();
        apply_control_event(&mut state, mk_approval_event("req-A", ""));
        assert_eq!(state.approval_list.items[0].actor_path, "root");
    }

    #[test]
    fn duplicate_request_id_does_not_double_enqueue() {
        // Server restarts may replay events; the queue must stay a set
        // keyed on request_id or a single approval shows twice.
        let mut state = fresh_state();
        apply_control_event(&mut state, mk_approval_event("req-A", "root"));
        apply_control_event(&mut state, mk_approval_event("req-A", "root"));
        assert_eq!(state.approval_list.items.len(), 1);
    }

    #[test]
    fn send_approve_removes_matching_item_from_list() {
        let mut state = fresh_state();
        apply_control_event(&mut state, mk_approval_event("req-A", "root"));
        apply_control_event(&mut state, mk_approval_event("req-B", "sublead-1"));
        send_approve(&mut state, "req-A", true, None, None);
        assert_eq!(state.approval_list.items.len(), 1);
        assert_eq!(state.approval_list.items[0].id, "req-B");
        // selected_idx must stay in bounds after a removal.
        assert!(state.approval_list.selected_idx < state.approval_list.items.len());
    }

    #[test]
    fn send_approve_clamps_selected_idx_when_last_item_removed() {
        let mut state = fresh_state();
        apply_control_event(&mut state, mk_approval_event("req-A", "root"));
        apply_control_event(&mut state, mk_approval_event("req-B", "sublead-1"));
        state.approval_list.selected_idx = 1;
        send_approve(&mut state, "req-B", false, None, None);
        assert_eq!(state.approval_list.items.len(), 1);
        assert_eq!(state.approval_list.selected_idx, 0);
    }

    #[test]
    fn switch_run_clears_stale_approval_list_and_policy_rules() {
        // Issue #95: when the operator navigates from the run selector to a
        // different run, per-run state (approval queue, policy rules, subtree
        // cache) must be cleared so the new run's Hello + queue-drain lands
        // against a fresh slate. Leaving stale entries behind means the
        // approval list pane shows requests that no longer have valid
        // responders on the new run's bridge.
        let mut state = fresh_state();
        apply_control_event(&mut state, mk_approval_event("old-req-A", "root"));
        apply_control_event(&mut state, mk_approval_event("old-req-B", "sublead-1"));
        state
            .policy_rules
            .push(pitboss_cli::mcp::policy::ApprovalRule {
                r#match: pitboss_cli::mcp::policy::ApprovalMatch::default(),
                action: pitboss_cli::mcp::policy::ApprovalAction::Block,
            });
        assert_eq!(state.approval_list.items.len(), 2);
        assert_eq!(state.policy_rules.len(), 1);

        reset_state_for_switch(
            &mut state,
            PathBuf::from("/tmp/new-run"),
            "new-run-id".to_string(),
        );

        assert_eq!(state.approval_list.items.len(), 0);
        assert_eq!(state.approval_list.selected_idx, 0);
        assert_eq!(state.policy_rules.len(), 0);
        assert!(matches!(state.mode, Mode::Normal));
        assert_eq!(state.run_id, "new-run-id");
        assert_eq!(state.run_dir, PathBuf::from("/tmp/new-run"));
    }

    #[test]
    fn switch_run_lets_new_approval_request_open_modal_cleanly() {
        // The reset is a precondition for the modal path: after it runs, a
        // fresh ApprovalRequest from the new run's dispatcher must be able
        // to populate the list AND open the modal with no stale siblings.
        let mut state = fresh_state();
        apply_control_event(&mut state, mk_approval_event("old-req", "root"));
        assert!(matches!(state.mode, Mode::ApprovalModal { .. }));

        reset_state_for_switch(
            &mut state,
            PathBuf::from("/tmp/new-run"),
            "new-run-id".to_string(),
        );
        assert!(matches!(state.mode, Mode::Normal));

        apply_control_event(&mut state, mk_approval_event("new-req", "root"));
        assert_eq!(state.approval_list.items.len(), 1);
        assert_eq!(state.approval_list.items[0].id, "new-req");
        assert!(matches!(state.mode, Mode::ApprovalModal { .. }));
    }
}
