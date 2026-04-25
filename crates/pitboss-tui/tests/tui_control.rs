//! TUI-side control tests. Uses ratatui's TestBackend to render a frame and
//! assert on the rendered buffer content. Does not drive a real control
//! socket — the keybinding → op plumbing is covered by the dispatcher-side
//! tests in `pitboss-cli/tests/control_flows.rs`.

use pitboss_tui::state::*;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

#[test]
fn confirm_kill_modal_renders_for_worker_target() {
    let mut state = AppState::new(std::path::PathBuf::from("/tmp"), "run-1".into());
    state.mode = Mode::ConfirmKill {
        target: KillTarget::Worker("worker-abc".into()),
    };

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| pitboss_tui::tui::render(frame, &state))
        .unwrap();
    let buf = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..20 {
        for x in 0..80 {
            text.push_str(buf.cell((x, y)).unwrap().symbol());
        }
        text.push('\n');
    }
    assert!(
        text.contains("worker-abc"),
        "modal should mention target id, got:\n{text}"
    );
}

#[test]
fn confirm_kill_modal_renders_for_run_target() {
    let mut state = AppState::new(std::path::PathBuf::from("/tmp"), "run-1".into());
    state.mode = Mode::ConfirmKill {
        target: KillTarget::Run,
    };

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| pitboss_tui::tui::render(frame, &state))
        .unwrap();
    let buf = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..20 {
        for x in 0..80 {
            text.push_str(buf.cell((x, y)).unwrap().symbol());
        }
        text.push('\n');
    }
    assert!(
        text.contains("ENTIRE RUN") || text.contains("entire run"),
        "modal should mention run-wide cancellation, got:\n{text}"
    );
}

#[test]
fn approval_modal_overview_renders_summary() {
    let mut state = AppState::new(std::path::PathBuf::from("/tmp"), "run-1".into());
    state.mode = Mode::ApprovalModal {
        request_id: "req-1".into(),
        task_id: "lead".into(),
        summary: "SPAWN THREE HOBBITS".into(),
        plan: None,
        kind: pitboss_cli::control::protocol::ApprovalKind::Action,
        sub_mode: ApprovalSubMode::Overview,
    };

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| pitboss_tui::tui::render(frame, &state))
        .unwrap();
    let buf = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..24 {
        for x in 0..100 {
            text.push_str(buf.cell((x, y)).unwrap().symbol());
        }
        text.push('\n');
    }
    assert!(
        text.contains("SPAWN THREE HOBBITS"),
        "modal should include the summary, got:\n{text}"
    );
}

#[test]
fn approval_modal_shows_plan_badge_for_plan_kind() {
    let mut state = AppState::new(std::path::PathBuf::from("/tmp"), "run-1".into());
    state.mode = Mode::ApprovalModal {
        request_id: "req-1".into(),
        task_id: "lead".into(),
        summary: "phase-1 plan".into(),
        plan: None,
        kind: pitboss_cli::control::protocol::ApprovalKind::Plan,
        sub_mode: ApprovalSubMode::Overview,
    };
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| pitboss_tui::tui::render(frame, &state))
        .unwrap();
    let buf = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..24 {
        for x in 0..100 {
            text.push_str(buf.cell((x, y)).unwrap().symbol());
        }
        text.push('\n');
    }
    assert!(
        text.contains("PRE-FLIGHT PLAN"),
        "plan-kind modal should show the pre-flight badge, got:\n{text}"
    );
}

#[test]
fn approval_modal_shows_action_badge_for_default_kind() {
    let mut state = AppState::new(std::path::PathBuf::from("/tmp"), "run-1".into());
    state.mode = Mode::ApprovalModal {
        request_id: "req-1".into(),
        task_id: "lead".into(),
        summary: "delete staging index".into(),
        plan: None,
        kind: pitboss_cli::control::protocol::ApprovalKind::Action,
        sub_mode: ApprovalSubMode::Overview,
    };
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| pitboss_tui::tui::render(frame, &state))
        .unwrap();
    let buf = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..24 {
        for x in 0..100 {
            text.push_str(buf.cell((x, y)).unwrap().symbol());
        }
        text.push('\n');
    }
    assert!(
        text.contains("IN-FLIGHT ACTION"),
        "action-kind modal should show the in-flight badge, got:\n{text}"
    );
}

#[test]
fn empty_grid_cells_are_cleared() {
    // Render a state with many tasks, then render a state with fewer
    // tasks. Assert leftover content from the first render is not
    // visible in cells now occupied by dead space.
    use pitboss_tui::state::{AppState, TileState, TileStatus};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    fn mk_tile(id: &str) -> TileState {
        TileState {
            id: id.into(),
            status: TileStatus::Running,
            duration_ms: None,
            token_usage_input: 0,
            token_usage_output: 0,
            cache_read: 0,
            cache_creation: 0,
            exit_code: None,
            log_path: PathBuf::from("/tmp/nope"),
            model: None,
            parent_task_id: None,
            worktree_path: None,
            completed_at: None,
        }
    }

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    // First render: 8 tiles whose IDs contain a distinctive marker.
    let mut state = AppState::new(PathBuf::from("/tmp"), "run-1".into());
    state.tasks = (0..8)
        .map(|i| mk_tile(&format!("DEADBEEF-LEFTOVER-{i}")))
        .collect();
    terminal
        .draw(|frame| pitboss_tui::tui::render(frame, &state))
        .unwrap();

    // Second render: 2 tiles — leaves 6 grid cells previously filled
    // with DEADBEEF content empty.
    state.tasks = (0..2).map(|i| mk_tile(&format!("active-{i}"))).collect();
    terminal
        .draw(|frame| pitboss_tui::tui::render(frame, &state))
        .unwrap();

    // Scrape the buffer. "DEADBEEF" should not appear anywhere anymore.
    let buf = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..40 {
        for x in 0..120 {
            text.push_str(buf.cell((x, y)).unwrap().symbol());
        }
        text.push('\n');
    }
    assert!(
        !text.contains("DEADBEEF"),
        "grid retained leftover content from prior render:\n{text}"
    );
    assert!(
        !text.contains("LEFTOVER"),
        "grid retained leftover content from prior render:\n{text}"
    );
}

#[test]
fn focus_log_content_does_not_bleed_into_tile_grid() {
    // Simulates user's reported scenario: render a frame with focus_log
    // containing long lines (which wrap), then re-render with *different*
    // focus_log content. Assert no fragment from the first log's text
    // appears in the upper (tile grid) region of the buffer.
    use pitboss_tui::state::{AppState, TileState, TileStatus};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    fn mk_tile(id: &str) -> TileState {
        TileState {
            id: id.into(),
            status: TileStatus::Running,
            duration_ms: None,
            token_usage_input: 0,
            token_usage_output: 0,
            cache_read: 0,
            cache_creation: 0,
            exit_code: None,
            log_path: PathBuf::from("/tmp/nope"),
            model: None,
            parent_task_id: None,
            worktree_path: None,
            completed_at: None,
        }
    }

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut state = AppState::new(PathBuf::from("/tmp"), "run-1".into());
    state.tasks = (0..16).map(|i| mk_tile(&format!("worker-{i}"))).collect();
    state.focus = 0;
    state.focus_log = vec![
        "* Bash MAGICTOKENONE very long command with lots of content that WORDWRAPS".into(),
        "< tool_result MAGICTOKENONE response data here with additional details".into(),
        "> assistant text MAGICTOKENONE some explanation about what to do next".into(),
    ];
    terminal
        .draw(|frame| pitboss_tui::tui::render(frame, &state))
        .unwrap();

    // Now simulate tile-focus change: different focus + different log content.
    state.focus = 1;
    state.focus_log = vec!["* Bash short cmd".into(), "< ok".into()];
    terminal
        .draw(|frame| pitboss_tui::tui::render(frame, &state))
        .unwrap();

    // Scrape ONLY the upper region (tile grid area, roughly rows 1..24 out
    // of 40-row buffer — 60% body minus 1 title row). The string
    // MAGICTOKENONE is unique to the first log; it must not appear anywhere
    // in the tile grid.
    let buf = terminal.backend().buffer();
    let mut upper_region = String::new();
    for y in 0..24 {
        for x in 0..120 {
            upper_region.push_str(buf.cell((x, y)).unwrap().symbol());
        }
        upper_region.push('\n');
    }
    assert!(
        !upper_region.contains("MAGICTOKENONE"),
        "tile grid retained focus-log content from prior render:\n{upper_region}"
    );
}
