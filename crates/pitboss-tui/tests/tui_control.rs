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
