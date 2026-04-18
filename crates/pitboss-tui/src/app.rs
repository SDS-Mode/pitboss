//! Event loop, input handling, and top-level TUI runner.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};

use crate::state::{AppSnapshot, AppState, Mode};
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

    // Resolve the control socket path from the run dir. Best-effort: if the
    // uuid parse or socket open fails, the TUI continues in observe-only mode.
    let control_client = match uuid::Uuid::parse_str(&state.run_id) {
        Ok(uuid) => {
            let socket_path = pitboss_cli::control::control_socket_path(uuid, &state.run_dir);
            let (bridge_tx, mut bridge_rx) =
                tokio::sync::mpsc::channel::<pitboss_cli::control::protocol::ControlEvent>(64);
            // Forward async → sync so the render loop can pull without tokio.
            let forward_tx = ctrl_events_tx.clone();
            runtime.spawn(async move {
                while let Some(ev) = bridge_rx.recv().await {
                    if forward_tx.send(ev).is_err() {
                        break;
                    }
                }
            });
            let client = runtime
                .block_on(crate::control::ControlClient::connect(
                    socket_path,
                    bridge_tx,
                ))
                .ok();
            client.map(Arc::new)
        }
        Err(_) => None,
    };
    state.control_connected = control_client.as_ref().is_some_and(|c| c.is_connected());
    state.control_client = control_client;
    // Stash a handle to THE runtime the ControlClient was built under.
    // `send_op` callers must spawn on this handle — spinning up a fresh
    // runtime per call (as the removed `futures_block_on` helper did)
    // left the socket writer registered with a dead reactor and every
    // kill/pause/reprompt op hung without error.
    state.runtime_handle = Some(runtime.handle().clone());
    // `runtime` owns the background reader + forward tasks and must be kept
    // alive for the full event loop. It falls out of scope at the end of
    // `run()`; the tasks are dropped cleanly then.
    let _runtime = runtime;
    // Our local handle to `ctrl_events_tx` is unused past this point — the
    // forward task owns the only clone that matters. Dropping makes the
    // receiver observe a closed channel once the forwarder exits.
    drop(ctrl_events_tx);

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
                            state.run_dir = run_dir;
                            state.run_id = run_id;
                            state.tasks = vec![];
                            state.focus = 0;
                            state.run_list.clear();
                            state.mode = Mode::Normal;
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
                            // SwitchRun path above — restart the watcher
                            // and reset run-local state.
                            let (new_rx, new_tx) = spawn_watcher(run_dir.clone());
                            snapshot_rx = new_rx;
                            focus_tx = new_tx;
                            state.run_dir = run_dir;
                            state.run_id = run_id;
                            state.tasks = vec![];
                            state.focus = 0;
                            state.run_list.clear();
                            state.mode = Mode::Normal;
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
        if let Ok(snapshot) = snapshot_rx.try_recv() {
            state.apply_snapshot(snapshot);
            // If we're in snap-in mode and at the bottom, keep the view
            // scrolled to the last line as new log lines arrive.
            if matches!(state.mode, Mode::Detail { .. }) {
                state.detail_auto_scroll(DETAIL_VISIBLE_ROWS);
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
        // shift-scroll cadence.
        (Mode::Detail { .. }, MouseEventKind::ScrollDown) => {
            state.detail_scroll_down(5, DETAIL_VISIBLE_ROWS);
        }
        (Mode::Detail { .. }, MouseEventKind::ScrollUp) => {
            state.detail_scroll_up(5);
        }
        // Right-click inside Detail view exits back to the grid —
        // symmetric with Esc, easier than reaching for the keyboard.
        (Mode::Detail { .. }, MouseEventKind::Down(MouseButton::Right)) => {
            state.exit_detail();
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
    }
}

fn handle_normal(state: &mut AppState, code: KeyCode) -> Action {
    match code {
        // Quit
        KeyCode::Char('q') => return Action::Quit,

        // Navigation
        KeyCode::Char('h') | KeyCode::Left => state.focus_left(),
        KeyCode::Char('l') | KeyCode::Right => state.focus_right(),
        KeyCode::Char('k') | KeyCode::Up => state.focus_up(),
        KeyCode::Char('j') | KeyCode::Down => state.focus_down(),

        // Help overlay
        KeyCode::Char('?') => state.mode = Mode::Help,

        // Run picker
        KeyCode::Char('o') => state.enter_picker(),

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
                    pitboss_cli::control::protocol::ControlOp::PauseWorker { task_id: tile.id },
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
    use pitboss_cli::control::protocol::ControlEvent as E;
    match ev {
        E::ApprovalRequest {
            request_id,
            task_id,
            summary,
        } => {
            state.mode = Mode::ApprovalModal {
                request_id,
                task_id,
                summary,
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
        KeyCode::Enter if modifiers.contains(KeyModifiers::CONTROL) => {
            // Ctrl+Enter: submit.
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
}

fn handle_approval_modal(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> Action {
    use crate::state::ApprovalSubMode;
    let Mode::ApprovalModal {
        request_id,
        task_id,
        summary,
        sub_mode,
    } = state.mode.clone()
    else {
        return Action::Continue;
    };
    let ctx = ApprovalCtx {
        request_id,
        task_id,
        summary,
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
                sub_mode: ApprovalSubMode::Editing { draft },
            };
        }
        KeyCode::Esc => state.mode = Mode::Normal,
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
        KeyCode::Enter if modifiers.contains(KeyModifiers::CONTROL) => {
            if editing {
                send_approve(state, &ctx.request_id, true, None, Some(draft));
            } else {
                send_approve(state, &ctx.request_id, false, Some(draft), None);
            }
            state.mode = Mode::Normal;
            return;
        }
        KeyCode::Esc => {
            state.mode = Mode::Normal;
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
        sub_mode,
    };
}

fn send_approve(
    state: &mut AppState,
    request_id: &str,
    approved: bool,
    comment: Option<String>,
    edited_summary: Option<String>,
) {
    spawn_control_op(
        state,
        pitboss_cli::control::protocol::ControlOp::Approve {
            request_id: request_id.to_string(),
            approved,
            comment,
            edited_summary,
        },
    );
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
