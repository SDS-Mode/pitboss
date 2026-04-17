//! Event loop, input handling, and top-level TUI runner.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};

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
pub fn run(run_dir: PathBuf, run_id: String) -> anyhow::Result<()> {
    let mut terminal = crate::tui::init()?;

    let mut state = AppState::new(run_dir.clone(), run_id);

    // Mutable channel endpoints so we can swap them when switching runs.
    let (mut snapshot_rx, mut focus_tx) = spawn_watcher(run_dir);

    // Send the initial focus (empty → watcher will tail first tile).
    let _ = focus_tx.send(String::new());

    loop {
        // --- Render ---
        terminal.draw(|frame| crate::tui::render(frame, &state))?;

        // --- Input (50ms poll) ---
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
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
                        continue;
                    }
                    Action::Continue => {}
                }

                // Notify watcher of new focus.
                if let Some(tile) = state.focused_tile() {
                    let _ = focus_tx.send(tile.id.clone());
                }
            }
        }

        // --- Snapshot from watcher (non-blocking) ---
        if let Ok(snapshot) = snapshot_rx.try_recv() {
            state.apply_snapshot(snapshot);
            // If we're in snap-in mode and at the bottom, keep the view
            // scrolled to the last line as new log lines arrive.
            if matches!(state.mode, Mode::SnapIn { .. }) {
                state.snap_auto_scroll(SNAP_VISIBLE_ROWS);
            }
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
/// for the handler. The render pass also calls `snap_auto_scroll` with the
/// real `visible_rows`.
const SNAP_VISIBLE_ROWS: usize = 40;

/// Handle a single key press. Returns an [`Action`] describing what to do next.
fn handle_key(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> Action {
    // Ctrl-C always quits.
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        return Action::Quit;
    }

    match state.mode {
        Mode::Normal => handle_normal(state, code),
        Mode::ViewingLog => handle_viewing_log(state, code),
        Mode::Help => handle_help(state, code),
        Mode::PickingRun { .. } => handle_picking_run(state, code),
        Mode::SnapIn { .. } => handle_snap_in(state, code, modifiers),
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

        // Log overlay
        KeyCode::Char('L') => state.mode = Mode::ViewingLog,

        // Help overlay
        KeyCode::Char('?') => state.mode = Mode::Help,

        // Run picker
        KeyCode::Char('o') => state.enter_picker(),

        // Snap-in: enter full-screen view for the focused tile.
        KeyCode::Enter => state.enter_snap_in(),

        // Refresh — watcher already polls every 500ms; render at loop top
        // covers forced redraw. All other keys are intentionally ignored.
        _ => {}
    }
    Action::Continue
}

fn handle_snap_in(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> Action {
    match code {
        // Exit back to grid.
        KeyCode::Esc => state.exit_snap_in(),

        // Quit the whole app.
        KeyCode::Char('q') => return Action::Quit,

        // Scroll down one line.
        KeyCode::Char('j') | KeyCode::Down => state.snap_scroll_down(1, SNAP_VISIBLE_ROWS),

        // Scroll up one line.
        KeyCode::Char('k') | KeyCode::Up => state.snap_scroll_up(1),

        // Page down (Ctrl-D or PageDown).
        KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
            state.snap_scroll_down(10, SNAP_VISIBLE_ROWS);
        }
        KeyCode::PageDown => state.snap_scroll_down(10, SNAP_VISIBLE_ROWS),

        // Page up (Ctrl-U or PageUp).
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            state.snap_scroll_up(10);
        }
        KeyCode::PageUp => state.snap_scroll_up(10),

        // Jump to bottom, re-enable auto-scroll.
        KeyCode::Char('G') => state.snap_jump_bottom(SNAP_VISIBLE_ROWS),

        // Jump to top, disable auto-scroll.
        KeyCode::Char('g') => state.snap_jump_top(),

        _ => {}
    }
    Action::Continue
}

fn handle_viewing_log(state: &mut AppState, code: KeyCode) -> Action {
    match code {
        KeyCode::Char('L') | KeyCode::Esc => state.mode = Mode::Normal,
        KeyCode::Char('q') => return Action::Quit,
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
