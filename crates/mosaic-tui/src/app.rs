//! Event loop, input handling, and top-level TUI runner.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};

use crate::state::{AppState, Mode};
use crate::watcher;

/// Run the TUI against the given run directory.
pub fn run(run_dir: PathBuf, run_id: String) -> anyhow::Result<()> {
    let mut terminal = crate::tui::init()?;

    let mut state = AppState::new(run_dir.clone(), run_id);

    // Channel: watcher → app (snapshot updates).
    let (snapshot_tx, snapshot_rx) = mpsc::sync_channel(4);
    // Channel: app → watcher (focused task id).
    let (focus_tx, focus_rx) = mpsc::channel::<String>();

    // Spawn the watcher thread.
    watcher::watch(run_dir, snapshot_tx, focus_rx);

    // Send the initial focus (empty → watcher will tail first tile).
    let _ = focus_tx.send(String::new());

    loop {
        // --- Render ---
        terminal.draw(|frame| crate::tui::render(frame, &state))?;

        // --- Input (50ms poll) ---
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                let quit = handle_key(&mut state, key.code, key.modifiers);
                if quit {
                    break;
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
        }
    }

    crate::tui::teardown(&mut terminal)?;
    Ok(())
}

/// Handle a single key press. Returns `true` if the app should quit.
fn handle_key(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> bool {
    // Ctrl-C always quits.
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        return true;
    }

    match state.mode {
        Mode::Normal => handle_normal(state, code),
        Mode::ViewingLog => handle_viewing_log(state, code),
        Mode::Help => handle_help(state, code),
    }
}

fn handle_normal(state: &mut AppState, code: KeyCode) -> bool {
    match code {
        // Quit
        KeyCode::Char('q') => return true,

        // Navigation
        KeyCode::Char('h') | KeyCode::Left => state.focus_left(),
        KeyCode::Char('l') | KeyCode::Right => state.focus_right(),
        KeyCode::Char('k') | KeyCode::Up => state.focus_up(),
        KeyCode::Char('j') | KeyCode::Down => state.focus_down(),

        // Log overlay
        KeyCode::Char('L') => state.mode = Mode::ViewingLog,

        // Help overlay
        KeyCode::Char('?') => state.mode = Mode::Help,

        // Refresh — watcher already polls every 500ms; render at loop top
        // covers forced redraw. All other keys are intentionally ignored.
        _ => {}
    }
    false
}

fn handle_viewing_log(state: &mut AppState, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('L') | KeyCode::Esc => state.mode = Mode::Normal,
        KeyCode::Char('q') => return true,
        _ => {}
    }
    false
}

fn handle_help(state: &mut AppState, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('?') | KeyCode::Esc => state.mode = Mode::Normal,
        KeyCode::Char('q') => return true,
        _ => {}
    }
    false
}
