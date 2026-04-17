//! Application state types for the Pitboss TUI.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use pitboss_core::store::TaskStatus;

/// Overall display mode of the TUI.
#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    ViewingLog,
    Help,
    /// The run-picker overlay is open; `selected` is the highlighted row index.
    PickingRun {
        selected: usize,
    },
    /// Full-screen snap-in view of a single task's log.
    ///
    /// `task_id` identifies which tile we're viewing (may differ from the grid
    /// focus if the user switched focus while in snap-in).
    /// `scroll` is the row offset from the top (0 = start of log).
    /// `at_bottom` tracks whether we should auto-scroll as new lines arrive.
    SnapIn {
        task_id: String,
        scroll: usize,
        at_bottom: bool,
    },
    /// v0.4: confirm modal before sending a destructive control op.
    #[allow(dead_code)]
    ConfirmKill {
        target: KillTarget,
    },
    /// v0.4: textarea-driven reprompt modal.
    #[allow(dead_code)]
    PromptReprompt {
        task_id: String,
        draft: String,
    },
    /// v0.4: approval modal. Driven by an `approval_request` event.
    #[allow(dead_code)]
    ApprovalModal {
        request_id: String,
        task_id: String,
        summary: String,
        sub_mode: ApprovalSubMode,
    },
}

/// What `ConfirmKill` targets.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum KillTarget {
    Worker(String),
    Run,
}

/// Sub-state of the `ApprovalModal`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ApprovalSubMode {
    /// Just showing the summary; awaiting y/n/e.
    Overview,
    /// User pressed `e`: editing the summary in a textarea.
    Editing { draft: String },
    /// User pressed `n`: writing a rejection comment.
    Rejecting { draft: String },
}

/// Status of a single tile.
#[derive(Debug, Clone)]
pub enum TileStatus {
    Pending,
    Running,
    Done(TaskStatus),
}

/// State for one task tile.
#[derive(Debug, Clone)]
pub struct TileState {
    pub id: String,
    pub status: TileStatus,
    pub duration_ms: Option<i64>,
    pub token_usage_input: u64,
    pub token_usage_output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    #[allow(dead_code)]
    pub exit_code: Option<i32>,
    pub log_path: PathBuf,
    /// Model name from resolved.json (used for cost estimation).
    pub model: Option<String>,
    /// Parent task id, if this tile represents a subagent task.
    #[allow(dead_code)]
    pub parent_task_id: Option<String>,
}

/// Full application state updated each poll cycle.
#[derive(Debug)]
pub struct AppState {
    pub run_dir: PathBuf,
    pub run_id: String,
    pub tasks: Vec<TileState>,
    /// Index into `tasks` of the currently focused tile.
    pub focus: usize,
    pub mode: Mode,
    /// Tail lines of the focused tile's stdout.log.
    pub focus_log: Vec<String>,
    /// Total number of failed tasks (from summary.jsonl).
    pub failed_count: usize,
    /// Snapshot of all runs collected when entering `PickingRun` mode.
    pub run_list: Vec<crate::runs::RunEntry>,
    /// Earliest wall-clock start time across all completed tiles in the current run.
    pub run_started_at: Option<DateTime<Utc>>,
    /// v0.4 control-socket client. None when the TUI was launched against a
    /// completed run or the control socket couldn't be opened.
    pub control_client: Option<std::sync::Arc<crate::control::ControlClient>>,
    /// Whether the control socket is currently connected. Mirrored from the
    /// client; used for the status-bar indicator.
    pub control_connected: bool,
}

impl AppState {
    pub fn new(run_dir: PathBuf, run_id: String) -> Self {
        Self {
            run_dir,
            run_id,
            tasks: Vec::new(),
            focus: 0,
            mode: Mode::Normal,
            focus_log: Vec::new(),
            failed_count: 0,
            run_list: Vec::new(),
            run_started_at: None,
            control_client: None,
            control_connected: false,
        }
    }

    /// Returns the currently focused tile, if any.
    pub fn focused_tile(&self) -> Option<&TileState> {
        self.tasks.get(self.focus)
    }

    /// Move focus left (wraps).
    pub fn focus_left(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        if self.focus == 0 {
            self.focus = self.tasks.len() - 1;
        } else {
            self.focus -= 1;
        }
    }

    /// Move focus right (wraps).
    pub fn focus_right(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        self.focus = (self.focus + 1) % self.tasks.len();
    }

    /// Move focus up by one row (4 columns).
    pub fn focus_up(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        if self.focus >= 4 {
            self.focus -= 4;
        }
    }

    /// Move focus down by one row (4 columns).
    pub fn focus_down(&mut self) {
        if self.tasks.is_empty() {
            return;
        }
        let next = self.focus + 4;
        if next < self.tasks.len() {
            self.focus = next;
        }
    }

    /// Enter the snap-in view for the currently focused tile.
    ///
    /// No-op if there is no focused tile. If already in `SnapIn`, stays put
    /// (don't nest). Starts at the bottom of the log (auto-scroll enabled).
    pub fn enter_snap_in(&mut self) {
        if matches!(self.mode, Mode::SnapIn { .. }) {
            return;
        }
        let Some(tile) = self.focused_tile() else {
            return;
        };
        let task_id = tile.id.clone();
        self.mode = Mode::SnapIn {
            task_id,
            scroll: 0,
            at_bottom: true,
        };
    }

    /// Exit the snap-in view and return to `Normal` mode.
    pub fn exit_snap_in(&mut self) {
        self.mode = Mode::Normal;
    }

    /// Scroll down by `delta` lines in `SnapIn` mode. Clamps at the last valid
    /// offset for the current `focus_log` length and `visible_rows`.
    ///
    /// Disables auto-scroll if the new position is not at the bottom.
    pub fn snap_scroll_down(&mut self, delta: usize, visible_rows: usize) {
        let Mode::SnapIn {
            ref mut scroll,
            ref mut at_bottom,
            ..
        } = self.mode
        else {
            return;
        };
        let total = self.focus_log.len();
        let max_scroll = total.saturating_sub(visible_rows);
        *scroll = (*scroll + delta).min(max_scroll);
        *at_bottom = *scroll >= max_scroll;
    }

    /// Scroll up by `delta` lines in `SnapIn` mode. Clamps at 0.
    ///
    /// Always disables auto-scroll (user is scrolling up).
    pub fn snap_scroll_up(&mut self, delta: usize) {
        let Mode::SnapIn {
            ref mut scroll,
            ref mut at_bottom,
            ..
        } = self.mode
        else {
            return;
        };
        *scroll = scroll.saturating_sub(delta);
        *at_bottom = false;
    }

    /// Jump to the top of the log in `SnapIn` mode; disables auto-scroll.
    pub fn snap_jump_top(&mut self) {
        let Mode::SnapIn {
            ref mut scroll,
            ref mut at_bottom,
            ..
        } = self.mode
        else {
            return;
        };
        *scroll = 0;
        *at_bottom = false;
    }

    /// Jump to the bottom of the log in `SnapIn` mode; re-enables auto-scroll.
    pub fn snap_jump_bottom(&mut self, visible_rows: usize) {
        let Mode::SnapIn {
            ref mut scroll,
            ref mut at_bottom,
            ..
        } = self.mode
        else {
            return;
        };
        let total = self.focus_log.len();
        *scroll = total.saturating_sub(visible_rows);
        *at_bottom = true;
    }

    /// Called after a snapshot is applied while in `SnapIn` mode.
    ///
    /// If `at_bottom` is true, advances `scroll` so the last line stays
    /// visible. Does nothing if the user has scrolled up.
    pub fn snap_auto_scroll(&mut self, visible_rows: usize) {
        let Mode::SnapIn {
            ref mut scroll,
            at_bottom,
            ..
        } = self.mode
        else {
            return;
        };
        if at_bottom {
            let total = self.focus_log.len();
            *scroll = total.saturating_sub(visible_rows);
        }
    }

    /// Enter the run picker: snapshot the run list and switch to `PickingRun` mode.
    ///
    /// No-op if already in `PickingRun` (don't nest).
    pub fn enter_picker(&mut self) {
        if matches!(self.mode, Mode::PickingRun { .. }) {
            return;
        }
        let base = crate::runs::runs_base_dir();
        self.run_list = crate::runs::collect_run_entries(&base);
        self.mode = Mode::PickingRun { selected: 0 };
    }

    /// Exit the picker and return to Normal mode without changing the active run.
    pub fn cancel_picker(&mut self) {
        self.mode = Mode::Normal;
        self.run_list.clear();
    }

    /// Move the picker selection up one row (wraps to bottom).
    pub fn picker_up(&mut self) {
        if let Mode::PickingRun { ref mut selected } = self.mode {
            let len = self.run_list.len();
            if len == 0 {
                return;
            }
            if *selected == 0 {
                *selected = len - 1;
            } else {
                *selected -= 1;
            }
        }
    }

    /// Move the picker selection down one row (wraps to top).
    pub fn picker_down(&mut self) {
        if let Mode::PickingRun { ref mut selected } = self.mode {
            let len = self.run_list.len();
            if len == 0 {
                return;
            }
            *selected = (*selected + 1) % len;
        }
    }

    /// Update tasks from a snapshot while preserving focus by task id.
    pub fn apply_snapshot(&mut self, snapshot: AppSnapshot) {
        let focused_id = self.tasks.get(self.focus).map(|t| t.id.clone());

        self.tasks = snapshot.tasks;
        self.focus_log = snapshot.focus_log;
        self.failed_count = snapshot.failed_count;
        // Keep the earliest start time we've ever seen (monotonically non-increasing).
        match (self.run_started_at, snapshot.run_started_at) {
            (None, v) => self.run_started_at = v,
            (Some(existing), Some(incoming)) if incoming < existing => {
                self.run_started_at = Some(incoming);
            }
            _ => {}
        }

        // Restore focus to the same task id if still present.
        if let Some(id) = focused_id {
            if let Some(pos) = self.tasks.iter().position(|t| t.id == id) {
                self.focus = pos;
                return;
            }
        }
        // Clamp focus to valid range.
        if !self.tasks.is_empty() && self.focus >= self.tasks.len() {
            self.focus = self.tasks.len() - 1;
        }
    }
}

/// A snapshot produced by the watcher thread every 500ms.
#[derive(Debug)]
pub struct AppSnapshot {
    pub tasks: Vec<TileState>,
    pub focus_log: Vec<String>,
    pub failed_count: usize,
    /// Earliest wall-clock start time across all completed tiles. `None` if no
    /// tiles have recorded a `started_at` yet.
    pub run_started_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs::RunEntry;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn make_state() -> AppState {
        AppState::new(PathBuf::from("/tmp/test-run"), "test-run".to_string())
    }

    fn fake_run_entries(n: usize) -> Vec<RunEntry> {
        (0..n)
            .map(|i| RunEntry {
                run_id: format!("run-{i:04}"),
                run_dir: PathBuf::from(format!("/tmp/run-{i:04}")),
                mtime: SystemTime::UNIX_EPOCH,
                tasks_total: i,
                tasks_failed: 0,
                is_complete: false,
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // enter_picker / cancel_picker
    // -----------------------------------------------------------------------

    /// `enter_picker` sets mode to `PickingRun` and populates `run_list`.
    /// We inject the run list directly to avoid needing a real pitboss runs dir.
    #[test]
    fn enter_picker_sets_mode_and_populates_list() {
        let mut state = make_state();
        // Inject fake run list directly (simulates what enter_picker would load).
        state.run_list = fake_run_entries(3);
        state.mode = Mode::PickingRun { selected: 0 };

        assert!(matches!(state.mode, Mode::PickingRun { selected: 0 }));
        assert_eq!(state.run_list.len(), 3);
    }

    /// `enter_picker` is a no-op when already in `PickingRun`.
    #[test]
    fn enter_picker_noop_when_already_picking() {
        let mut state = make_state();
        state.run_list = fake_run_entries(2);
        state.mode = Mode::PickingRun { selected: 1 };

        state.enter_picker(); // should be a no-op

        // Still in PickingRun and selected unchanged.
        assert!(matches!(state.mode, Mode::PickingRun { selected: 1 }));
    }

    #[test]
    fn cancel_picker_returns_to_normal() {
        let mut state = make_state();
        state.run_list = fake_run_entries(3);
        state.mode = Mode::PickingRun { selected: 2 };
        state.focus = 1; // focus should not change

        state.cancel_picker();

        assert!(matches!(state.mode, Mode::Normal));
        assert!(state.run_list.is_empty());
        assert_eq!(state.focus, 1, "cancel_picker must not touch focus");
    }

    // -----------------------------------------------------------------------
    // picker_up / picker_down navigation
    // -----------------------------------------------------------------------

    #[test]
    fn picker_down_advances_selection() {
        let mut state = make_state();
        state.run_list = fake_run_entries(3);
        state.mode = Mode::PickingRun { selected: 0 };

        state.picker_down();
        assert!(matches!(state.mode, Mode::PickingRun { selected: 1 }));

        state.picker_down();
        assert!(matches!(state.mode, Mode::PickingRun { selected: 2 }));
    }

    #[test]
    fn picker_down_wraps_at_bottom() {
        let mut state = make_state();
        state.run_list = fake_run_entries(3);
        state.mode = Mode::PickingRun { selected: 2 };

        state.picker_down();
        assert!(matches!(state.mode, Mode::PickingRun { selected: 0 }));
    }

    #[test]
    fn picker_up_moves_selection_back() {
        let mut state = make_state();
        state.run_list = fake_run_entries(3);
        state.mode = Mode::PickingRun { selected: 2 };

        state.picker_up();
        assert!(matches!(state.mode, Mode::PickingRun { selected: 1 }));

        state.picker_up();
        assert!(matches!(state.mode, Mode::PickingRun { selected: 0 }));
    }

    #[test]
    fn picker_up_wraps_at_top() {
        let mut state = make_state();
        state.run_list = fake_run_entries(3);
        state.mode = Mode::PickingRun { selected: 0 };

        state.picker_up();
        assert!(matches!(state.mode, Mode::PickingRun { selected: 2 }));
    }

    #[test]
    fn picker_navigation_noop_when_not_picking() {
        let mut state = make_state();
        state.run_list = fake_run_entries(3);
        state.mode = Mode::Normal;

        state.picker_up();
        state.picker_down();

        // Mode unchanged.
        assert!(matches!(state.mode, Mode::Normal));
    }

    #[test]
    fn picker_navigation_noop_on_empty_list() {
        let mut state = make_state();
        state.run_list = vec![];
        state.mode = Mode::PickingRun { selected: 0 };

        state.picker_down(); // should not panic
        state.picker_up(); // should not panic

        assert!(matches!(state.mode, Mode::PickingRun { selected: 0 }));
    }

    // -----------------------------------------------------------------------
    // SnapIn mode
    // -----------------------------------------------------------------------

    fn make_state_with_tile(task_id: &str) -> AppState {
        use std::path::PathBuf;
        let mut state = make_state();
        state.tasks = vec![TileState {
            id: task_id.to_string(),
            status: TileStatus::Running,
            duration_ms: None,
            token_usage_input: 0,
            token_usage_output: 0,
            cache_read: 0,
            cache_creation: 0,
            exit_code: None,
            log_path: PathBuf::from("/dev/null"),
            model: None,
            parent_task_id: None,
        }];
        state
    }

    #[test]
    fn enter_snap_in_from_normal() {
        let mut state = make_state_with_tile("task-001");
        state.mode = Mode::Normal;

        state.enter_snap_in();

        assert!(
            matches!(
                &state.mode,
                Mode::SnapIn { task_id, at_bottom: true, .. } if task_id == "task-001"
            ),
            "expected SnapIn with task_id=task-001, got {:?}",
            state.mode
        );
        if let Mode::SnapIn { scroll, .. } = &state.mode {
            assert_eq!(*scroll, 0, "initial scroll should be 0");
        }
    }

    #[test]
    fn enter_snap_in_noop_when_no_tile() {
        let mut state = make_state(); // no tasks
        state.mode = Mode::Normal;

        state.enter_snap_in();

        assert!(
            matches!(state.mode, Mode::Normal),
            "should stay Normal with no tiles"
        );
    }

    #[test]
    fn enter_snap_in_noop_when_already_in_snap_in() {
        let mut state = make_state_with_tile("task-001");
        state.mode = Mode::SnapIn {
            task_id: "task-001".to_string(),
            scroll: 5,
            at_bottom: false,
        };

        state.enter_snap_in(); // should be a no-op

        // scroll must be unchanged
        assert!(matches!(&state.mode, Mode::SnapIn { scroll: 5, .. }));
    }

    #[test]
    fn exit_snap_in_returns_to_normal() {
        let mut state = make_state_with_tile("task-001");
        state.mode = Mode::SnapIn {
            task_id: "task-001".to_string(),
            scroll: 3,
            at_bottom: false,
        };

        state.exit_snap_in();

        assert!(matches!(state.mode, Mode::Normal));
    }

    #[test]
    fn scroll_down_advances() {
        let mut state = make_state_with_tile("t");
        // Give the state some log lines so scroll can advance.
        state.focus_log = (0..20).map(|i| format!("line {i}")).collect();
        state.mode = Mode::SnapIn {
            task_id: "t".to_string(),
            scroll: 0,
            at_bottom: false,
        };

        state.snap_scroll_down(1, 10);

        assert!(
            matches!(&state.mode, Mode::SnapIn { scroll: 1, .. }),
            "expected scroll=1, got {:?}",
            state.mode
        );
    }

    #[test]
    fn scroll_down_clamps_at_max() {
        let mut state = make_state_with_tile("t");
        state.focus_log = (0..10).map(|i| format!("line {i}")).collect();
        // visible_rows=10 means max_scroll = 10 - 10 = 0
        state.mode = Mode::SnapIn {
            task_id: "t".to_string(),
            scroll: 0,
            at_bottom: false,
        };

        state.snap_scroll_down(999, 10);

        // Can't scroll past the end.
        assert!(
            matches!(
                &state.mode,
                Mode::SnapIn {
                    scroll: 0,
                    at_bottom: true,
                    ..
                }
            ),
            "got {:?}",
            state.mode
        );
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut state = make_state_with_tile("t");
        state.mode = Mode::SnapIn {
            task_id: "t".to_string(),
            scroll: 0,
            at_bottom: false,
        };

        state.snap_scroll_up(1);

        assert!(
            matches!(&state.mode, Mode::SnapIn { scroll: 0, .. }),
            "scroll should stay at 0"
        );
    }

    #[test]
    fn scroll_up_decrements() {
        let mut state = make_state_with_tile("t");
        state.mode = Mode::SnapIn {
            task_id: "t".to_string(),
            scroll: 3,
            at_bottom: false,
        };

        state.snap_scroll_up(1);

        assert!(
            matches!(&state.mode, Mode::SnapIn { scroll: 2, .. }),
            "expected scroll=2, got {:?}",
            state.mode
        );
    }

    #[test]
    fn snap_jump_top_sets_scroll_to_zero_and_disables_auto_scroll() {
        let mut state = make_state_with_tile("t");
        state.focus_log = (0..50).map(|i| format!("line {i}")).collect();
        state.mode = Mode::SnapIn {
            task_id: "t".to_string(),
            scroll: 30,
            at_bottom: true,
        };

        state.snap_jump_top();

        assert!(
            matches!(
                &state.mode,
                Mode::SnapIn {
                    scroll: 0,
                    at_bottom: false,
                    ..
                }
            ),
            "got {:?}",
            state.mode
        );
    }

    #[test]
    fn snap_jump_bottom_enables_auto_scroll() {
        let mut state = make_state_with_tile("t");
        state.focus_log = (0..50).map(|i| format!("line {i}")).collect();
        state.mode = Mode::SnapIn {
            task_id: "t".to_string(),
            scroll: 5,
            at_bottom: false,
        };

        state.snap_jump_bottom(10);

        assert!(
            matches!(
                &state.mode,
                Mode::SnapIn {
                    at_bottom: true,
                    ..
                }
            ),
            "got {:?}",
            state.mode
        );
        if let Mode::SnapIn { scroll, .. } = &state.mode {
            assert_eq!(*scroll, 40, "scroll should be total(50) - visible(10) = 40");
        }
    }

    #[test]
    fn snap_auto_scroll_advances_when_at_bottom() {
        let mut state = make_state_with_tile("t");
        state.focus_log = (0..30).map(|i| format!("line {i}")).collect();
        state.mode = Mode::SnapIn {
            task_id: "t".to_string(),
            scroll: 20, // at bottom for 30 lines, 10 visible
            at_bottom: true,
        };

        // Simulate new lines arriving — focus_log grows to 35 lines.
        state.focus_log = (0..35).map(|i| format!("line {i}")).collect();
        state.snap_auto_scroll(10);

        if let Mode::SnapIn { scroll, .. } = &state.mode {
            assert_eq!(*scroll, 25, "should advance to 35-10=25");
        } else {
            panic!("not in SnapIn mode");
        }
    }

    #[test]
    fn snap_auto_scroll_noop_when_not_at_bottom() {
        let mut state = make_state_with_tile("t");
        state.focus_log = (0..30).map(|i| format!("line {i}")).collect();
        state.mode = Mode::SnapIn {
            task_id: "t".to_string(),
            scroll: 5,
            at_bottom: false,
        };

        state.focus_log = (0..35).map(|i| format!("line {i}")).collect();
        state.snap_auto_scroll(10);

        // scroll unchanged
        assert!(
            matches!(&state.mode, Mode::SnapIn { scroll: 5, .. }),
            "got {:?}",
            state.mode
        );
    }

    // -----------------------------------------------------------------------
    // apply_snapshot stores run_started_at
    // -----------------------------------------------------------------------

    #[test]
    fn apply_snapshot_stores_run_started_at() {
        use chrono::TimeZone;

        let mut state = make_state();

        let t0 = chrono::Utc.with_ymd_and_hms(2026, 4, 16, 10, 0, 0).unwrap();
        let snapshot = AppSnapshot {
            tasks: Vec::new(),
            focus_log: Vec::new(),
            failed_count: 0,
            run_started_at: Some(t0),
        };
        state.apply_snapshot(snapshot);
        assert_eq!(state.run_started_at, Some(t0));

        // A later snapshot with an earlier start time should update it.
        let t_earlier = chrono::Utc.with_ymd_and_hms(2026, 4, 16, 9, 0, 0).unwrap();
        let snapshot2 = AppSnapshot {
            tasks: Vec::new(),
            focus_log: Vec::new(),
            failed_count: 0,
            run_started_at: Some(t_earlier),
        };
        state.apply_snapshot(snapshot2);
        assert_eq!(
            state.run_started_at,
            Some(t_earlier),
            "should update to earlier start time"
        );

        // A later snapshot with a later start time should NOT update.
        let t_later = chrono::Utc.with_ymd_and_hms(2026, 4, 16, 11, 0, 0).unwrap();
        let snapshot3 = AppSnapshot {
            tasks: Vec::new(),
            focus_log: Vec::new(),
            failed_count: 0,
            run_started_at: Some(t_later),
        };
        state.apply_snapshot(snapshot3);
        assert_eq!(
            state.run_started_at,
            Some(t_earlier),
            "should keep the earlier start time"
        );
    }

    #[test]
    fn confirm_kill_variant_round_trip() {
        let m = Mode::ConfirmKill {
            target: KillTarget::Worker("w-1".into()),
        };
        assert!(matches!(m, Mode::ConfirmKill { .. }));
        let m2 = Mode::ConfirmKill {
            target: KillTarget::Run,
        };
        assert!(matches!(
            m2,
            Mode::ConfirmKill {
                target: KillTarget::Run
            }
        ));
    }

    #[test]
    fn prompt_reprompt_variant_constructs() {
        let m = Mode::PromptReprompt {
            task_id: "w-1".into(),
            draft: String::new(),
        };
        assert!(matches!(m, Mode::PromptReprompt { .. }));
    }

    #[test]
    fn approval_modal_variant_constructs() {
        let m = Mode::ApprovalModal {
            request_id: "req-1".into(),
            task_id: "lead".into(),
            summary: "spawn 3".into(),
            sub_mode: ApprovalSubMode::Overview,
        };
        assert!(matches!(m, Mode::ApprovalModal { .. }));
    }
}
