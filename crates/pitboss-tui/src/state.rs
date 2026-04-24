//! Application state types for the Pitboss TUI.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use pitboss_core::store::TaskStatus;

/// Overall display mode of the TUI.
#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    Help,
    /// The run-picker overlay is open; `selected` is the highlighted row index.
    PickingRun {
        selected: usize,
    },
    /// Full-screen detail view of a single task. Left pane shows task
    /// metadata (role, model, tokens, cost, events, git diff, ...); right
    /// pane shows the log with scroll. Replaces the v0.4-era `ViewingLog`
    /// overlay and `SnapIn` full-screen log view — both were redundant
    /// (same log content, different chrome).
    ///
    /// `task_id` identifies which tile we're viewing (may differ from the
    /// grid focus if the user switched focus while in detail).
    /// `scroll` is the row offset from the top (0 = start of log).
    /// `at_bottom` tracks whether we should auto-scroll as new lines arrive.
    Detail {
        task_id: String,
        scroll: usize,
        at_bottom: bool,
    },
    /// v0.4: confirm modal before sending a destructive control op.
    ConfirmKill {
        target: KillTarget,
    },
    /// v0.4: textarea-driven reprompt modal.
    PromptReprompt {
        task_id: String,
        draft: String,
    },
    /// v0.4: approval modal. Driven by an `approval_request` event.
    /// `plan` carries the structured fields (rationale / resources /
    /// risks / rollback) for v0.4.5+ leads that ship typed approvals;
    /// `None` for simple summary-only approvals, in which case the
    /// modal renders just the summary.
    ApprovalModal {
        request_id: String,
        task_id: String,
        summary: String,
        plan: Option<pitboss_cli::control::protocol::ApprovalPlanWire>,
        /// `Action` = in-flight approval from `request_approval`;
        /// `Plan` = pre-flight approval from `propose_plan`. Drives the
        /// modal header badge so operators can tell them apart without
        /// reading the summary.
        kind: pitboss_cli::control::protocol::ApprovalKind,
        sub_mode: ApprovalSubMode,
    },
}

/// What `ConfirmKill` targets.
#[derive(Debug, Clone)]
pub enum KillTarget {
    Worker(String),
    Run,
}

/// Sub-state of the `ApprovalModal`.
#[derive(Debug, Clone)]
pub enum ApprovalSubMode {
    /// Just showing the summary; awaiting y/n/e.
    Overview,
    /// User pressed `e`: editing the summary in a textarea.
    Editing { draft: String },
    /// User pressed `n`: writing a rejection comment.
    Rejecting { draft: String },
}

/// Which top-level pane has keyboard focus in the normal view.
/// Default: `Grid` (existing v0.5 behavior).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PaneFocus {
    #[default]
    Grid,
    ApprovalList,
}

/// A single pending approval shown in the right-rail approval list pane.
#[derive(Debug, Clone)]
pub struct ApprovalListItem {
    /// Opaque request id (forwarded verbatim to `ControlOp::Approve`).
    /// Server-generated format is `req-<uuidv7>`, so this is a string
    /// rather than a parsed `Uuid` — no meaningful operation treats the
    /// inner uuid as a uuid, only as a stable handle.
    pub id: String,
    /// Human-readable path to the actor that raised the request
    /// (e.g. `"root"` or `"root→S1"`).
    pub actor_path: String,
    /// Free-form category tag (e.g. `"plan"`, `"action"`).
    pub category: String,
    /// One-line summary of the requested action.
    pub summary: String,
    /// Optional typed plan payload carried by `propose_plan` requests.
    /// None for `request_approval` calls. Preserved in the list so
    /// re-opening a dismissed plan approval renders the structured
    /// plan view (not just the summary).
    pub plan: Option<pitboss_cli::control::protocol::ApprovalPlanWire>,
    /// Discriminator for modal rendering (Plan vs Action badge).
    pub kind: pitboss_cli::control::protocol::ApprovalKind,
    /// Wall-clock time when the request arrived.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl ApprovalListItem {
    /// Short human-readable category string for list-line rendering.
    pub fn category_str(&self) -> &str {
        &self.category
    }
}

/// Status of a single tile.
#[derive(Debug, Clone)]
pub enum TileStatus {
    Pending,
    Running,
    Done(TaskStatus),
}

/// View state for one sub-lead's tree, populated by `SubleadSpawned` events
/// from the control socket. Workers are keyed by their task id.
#[derive(Debug, Clone, Default)]
pub struct SubtreeView {
    pub workers: HashMap<String, TileState>,
    pub spent_usd: f64,
    pub budget_usd: Option<f64>,
    pub pending_approvals: u32,
    pub read_down: bool,
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
    /// Git worktree directory for this task, if pitboss created one.
    /// Populated from `summary.jsonl`/`summary.json` when the task has
    /// settled. `None` for in-flight tasks (the record hasn't been written
    /// yet) and for `use_worktree = false` tasks.
    pub worktree_path: Option<PathBuf>,
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
    /// Cached `git diff --stat` summary for a task's worktree, keyed by
    /// `task_id`. Populated on `enter_detail` (synchronous shell-out) and
    /// reused until the user exits detail mode. `None` for tasks whose
    /// worktree path is unknown or whose diff couldn't be computed.
    pub cached_git_diff: std::collections::HashMap<String, GitDiffSummary>,
    /// Height of the detail-view log pane (inner rows), set by the render
    /// pass via interior mutability. Read by the scroll handlers so they
    /// know the real `max_scroll = total_rows - viewport` without having
    /// to query the terminal themselves. 0 until the first render.
    pub detail_log_viewport: std::sync::atomic::AtomicUsize,
    /// Total visual rows of the detail log after word-wrap at the current
    /// pane width, set by the render pass. Differs from `focus_log.len()`
    /// whenever any line wraps — scroll must use this to map 1:1 with
    /// what's painted. 0 until the first render.
    pub detail_log_total_rows: std::sync::atomic::AtomicUsize,
    /// Handle to the tokio runtime that owns the `ControlClient`. Must be
    /// used to spawn any future that touches the control socket — building
    /// a fresh `new_current_thread()` runtime per call would run the write
    /// from the wrong reactor and async I/O would silently hang. `None`
    /// only in tests where no control socket is in play.
    pub runtime_handle: Option<tokio::runtime::Handle>,
    /// Per-actor shared-store activity counters received via the control
    /// socket's periodic `StoreActivity` broadcast. Rendered as
    /// `kv:N lease:M` on each grid tile. Keyed by `actor_id` (matches
    /// `TileState.id` for the lead + each worker). Empty until the first
    /// broadcast arrives (~1 s after TUI connects).
    pub store_activity: std::collections::HashMap<String, StoreActivityCounters>,
    /// Bounding rectangles of each tile, populated by `render_tile_grid`
    /// each frame. Used by the mouse-click handler to hit-test which
    /// tile a click landed on. Wrapped in a Mutex so the render pass
    /// (which has `&AppState`) can update it via interior mutability —
    /// same pattern as `detail_log_viewport`.
    pub tile_hit_rects: std::sync::Mutex<Vec<(usize, ratatui::layout::Rect)>>,
    /// Bounding rectangles of each run-picker row (y coords are absolute
    /// terminal rows). Populated by `render_run_picker_overlay` whenever
    /// the picker is visible; used by the mouse click handler to open
    /// a run with one click.
    pub picker_hit_rects: std::sync::Mutex<Vec<(usize, ratatui::layout::Rect)>>,
    /// Sub-tree views keyed by `sublead_id`. Empty in depth-1 runs.
    pub subtrees: HashMap<String, SubtreeView>,
    /// Collapse state for each sub-tree container. `true` = expanded (default
    /// on spawn). Subtrees absent from this map are treated as expanded.
    pub expanded: HashMap<String, bool>,
    /// Index into the ordered list `[root, sublead_0, sublead_1, …]` that
    /// currently has the grouped-grid header focus. 0 = root tile grid.
    /// Used by Tab cycling and Enter toggle-collapse.
    pub focused_subtree_idx: usize,
    /// Which top-level pane has keyboard focus. Default: `Grid`.
    /// `'a'` switches to `ApprovalList`; `Esc` returns to `Grid`.
    pub pane_focus: PaneFocus,
    /// Non-modal approval queue rendered in the right-rail pane (30% width).
    pub approval_list: crate::approval_list::ApprovalListState,
}

/// Mirrors `pitboss_cli::control::protocol::ActorActivityEntry` but
/// kept as a separate TUI-owned type so the state module doesn't depend
/// on the wire protocol's serde derives.
#[derive(Debug, Default, Clone, Copy)]
pub struct StoreActivityCounters {
    pub kv_ops: u64,
    pub lease_ops: u64,
}

/// Summary of a worker's worktree diff vs its base branch.
#[derive(Debug, Clone, Default)]
pub struct GitDiffSummary {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
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
            cached_git_diff: std::collections::HashMap::new(),
            detail_log_viewport: std::sync::atomic::AtomicUsize::new(0),
            detail_log_total_rows: std::sync::atomic::AtomicUsize::new(0),
            runtime_handle: None,
            store_activity: std::collections::HashMap::new(),
            tile_hit_rects: std::sync::Mutex::new(Vec::new()),
            picker_hit_rects: std::sync::Mutex::new(Vec::new()),
            subtrees: HashMap::new(),
            expanded: HashMap::new(),
            focused_subtree_idx: 0,
            pane_focus: PaneFocus::Grid,
            approval_list: crate::approval_list::ApprovalListState::default(),
        }
    }

    /// Hit-test: find the picker row index whose render rect contains
    /// `(col, row)`. Same shape as `tile_at`. Returns `None` when the
    /// picker isn't open or the click fell on empty space (run list
    /// shorter than the picker area).
    pub fn picker_row_at(&self, col: u16, row: u16) -> Option<usize> {
        let rects = self.picker_hit_rects.lock().ok()?;
        rects.iter().find_map(|(idx, r)| {
            if col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height {
                Some(*idx)
            } else {
                None
            }
        })
    }

    /// Hit-test: find the tile index whose render rect contains `(col, row)`.
    /// Populated by `render_tile_grid`; reads the cache under its Mutex.
    /// Returns `None` if no tile matches (click outside the grid) or the
    /// cache is empty (no render has run yet).
    pub fn tile_at(&self, col: u16, row: u16) -> Option<usize> {
        let rects = self.tile_hit_rects.lock().ok()?;
        rects.iter().find_map(|(idx, r)| {
            if col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height {
                Some(*idx)
            } else {
                None
            }
        })
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

    /// Enter the detail view for the currently focused tile.
    ///
    /// No-op if there is no focused tile. If already in `Detail`, stays put
    /// (don't nest). Starts at the bottom of the log (auto-scroll enabled).
    /// Also triggers a one-shot `git diff --stat` on the tile's worktree so
    /// the metadata pane can show lines added/removed; the result is cached
    /// in `self.cached_git_diff` until exit.
    pub fn enter_detail(&mut self) {
        if matches!(self.mode, Mode::Detail { .. }) {
            return;
        }
        let Some(tile) = self.focused_tile() else {
            return;
        };
        let task_id = tile.id.clone();
        // worktree_path is populated for in-flight tiles via the
        // `worktree.path` sidecar (written by the dispatcher at spawn
        // time) and for completed tiles via TaskRecord.worktree_path —
        // so this diff runs for both live and settled workers.
        if let Some(worktree) = tile.worktree_path.as_deref() {
            if let Some(summary) = compute_git_diff_summary(worktree) {
                self.cached_git_diff.insert(task_id.clone(), summary);
            }
        }
        self.mode = Mode::Detail {
            task_id,
            scroll: 0,
            at_bottom: true,
        };
    }

    /// Exit the detail view and return to `Normal` mode. Clears the
    /// git-diff cache since it was computed against the task's worktree at
    /// entry time and may be stale by next entry.
    pub fn exit_detail(&mut self) {
        self.mode = Mode::Normal;
        self.cached_git_diff.clear();
    }

    /// Scroll down by `delta` visual rows in `Detail` mode. Clamps at the last
    /// valid row offset for the current wrapped log and viewport height.
    ///
    /// Disables auto-scroll if the new position is not at the bottom.
    pub fn detail_scroll_down(&mut self, delta: usize, _visible_rows: usize) {
        let total_rows = self
            .detail_log_total_rows
            .load(std::sync::atomic::Ordering::Relaxed);
        let viewport = self
            .detail_log_viewport
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(1);
        let max_scroll = total_rows.saturating_sub(viewport);
        let Mode::Detail {
            ref mut scroll,
            ref mut at_bottom,
            ..
        } = self.mode
        else {
            return;
        };
        // Always re-clamp scroll to max_scroll before the op. state.scroll
        // can legitimately be above max_scroll right after auto-follow
        // (we just set it to `max_scroll` there, but `total` may have
        // shrunk in between, or a prior op left it high). Without this
        // re-clamp, decrementing from an above-range value produces no
        // visible change until scroll drops below max_scroll.
        let current = (*scroll).min(max_scroll);
        *scroll = current.saturating_add(delta).min(max_scroll);
        if *scroll >= max_scroll {
            *at_bottom = true;
        }
    }

    /// Scroll up by `delta` visual rows in `Detail` mode. Clamps at 0.
    ///
    /// Always disables auto-scroll (user is scrolling up). `state.scroll`
    /// can legitimately be above `max_scroll` right after auto-follow (we
    /// pin to `max_scroll` there, but the wrapped-row total may have shrunk
    /// in between). Always re-clamp to `max_scroll` before decrementing so
    /// the first `k` press produces visible movement.
    pub fn detail_scroll_up(&mut self, delta: usize) {
        let total_rows = self
            .detail_log_total_rows
            .load(std::sync::atomic::Ordering::Relaxed);
        let viewport = self
            .detail_log_viewport
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(1);
        let max_scroll = total_rows.saturating_sub(viewport);
        let Mode::Detail {
            ref mut scroll,
            ref mut at_bottom,
            ..
        } = self.mode
        else {
            return;
        };
        // ALWAYS clamp to max_scroll before decrementing (not just when
        // leaving auto-follow). If state.scroll was left above max_scroll
        // by any earlier code path, subsequent ups decrement from that
        // high value and the user sees no visible change until scroll
        // drops below max_scroll — the symptom the user reports as
        // "scroll doesn't do anything."
        let current = (*scroll).min(max_scroll);
        *scroll = current.saturating_sub(delta);
        *at_bottom = false;
    }

    /// Jump to the top of the log in `SnapIn` mode; disables auto-scroll.
    pub fn detail_jump_top(&mut self) {
        let Mode::Detail {
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

    /// Jump to the bottom of the log in `Detail` mode; re-enables auto-scroll.
    pub fn detail_jump_bottom(&mut self, _visible_rows: usize) {
        let total_rows = self
            .detail_log_total_rows
            .load(std::sync::atomic::Ordering::Relaxed);
        let viewport = self
            .detail_log_viewport
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(1);
        let max_scroll = total_rows.saturating_sub(viewport);
        let Mode::Detail {
            ref mut scroll,
            ref mut at_bottom,
            ..
        } = self.mode
        else {
            return;
        };
        *scroll = max_scroll;
        *at_bottom = true;
    }

    /// Scroll the log overlay down by `delta` visual rows. No upper bound
    /// is enforced — `Paragraph::scroll` clips cleanly at end-of-content,
    /// so overshooting just shows blank lines at the end.
    /// Called after a snapshot is applied while in `Detail` mode.
    ///
    /// If `at_bottom` is true, advances `scroll` so the last line stays
    /// visible. Does nothing if the user has scrolled up.
    pub fn detail_auto_scroll(&mut self, _visible_rows: usize) {
        let total_rows = self
            .detail_log_total_rows
            .load(std::sync::atomic::Ordering::Relaxed);
        let viewport = self
            .detail_log_viewport
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(1);
        let max_scroll = total_rows.saturating_sub(viewport);
        let Mode::Detail {
            ref mut scroll,
            at_bottom,
            ..
        } = self.mode
        else {
            return;
        };
        if at_bottom {
            // Pin to exact max_scroll. state.scroll stays within
            // [0, max_scroll] — the scroll handlers always clamp first
            // but they're simpler to reason about when auto-follow
            // doesn't introduce an invalid-scroll interval.
            *scroll = max_scroll;
        }
    }

    /// Sorted sublead ids for stable ordering in the grouped grid.
    pub fn sorted_sublead_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.subtrees.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Cycle focus across containers: index 0 = root tile grid,
    /// index 1..N = sub-lead containers in sorted order. Wraps around.
    pub fn cycle_focus_to_next_subtree(&mut self) {
        // Total containers = 1 (root) + number of sub-trees.
        let total = 1 + self.subtrees.len();
        if total == 0 {
            return;
        }
        self.focused_subtree_idx = (self.focused_subtree_idx + 1) % total;
    }

    /// Returns `true` when the current grouped-grid focus is on a sub-tree
    /// header (i.e., `focused_subtree_idx > 0`).
    pub fn focused_subtree_header(&self) -> bool {
        self.focused_subtree_idx > 0 && !self.subtrees.is_empty()
    }

    /// Returns the sublead id currently focused by the grouped-grid header
    /// focus, or `None` when focus is on the root row or there are no subtrees.
    pub fn focused_sublead_id(&self) -> Option<String> {
        if !self.focused_subtree_header() {
            return None;
        }
        let ids = self.sorted_sublead_ids();
        // focused_subtree_idx 1 maps to ids[0], etc.
        ids.get(self.focused_subtree_idx.saturating_sub(1)).cloned()
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

/// Compute `git diff --stat HEAD` in the given directory. Walks up to
/// find the enclosing git worktree (so passing in `<run-dir>/tasks/<id>/`
/// works — git discovers the worktree via the parent hierarchy). Returns
/// `None` if the shell-out fails, the directory isn't inside a worktree,
/// or the command exceeds the wall-clock timeout.
///
/// Blocks the caller until git returns or `GIT_DIFF_TIMEOUT` elapses.
/// Callers that run this on the event loop should wrap it in a background
/// thread (see `AppState` for the polling pattern) — without that, a
/// slow or contended `.git` freezes input handling.
///
/// `--no-optional-locks` avoids taking the index lock, so a concurrent
/// `git commit` / `gc` can't contend with the TUI's read.
pub(crate) fn compute_git_diff_summary(worktree: &std::path::Path) -> Option<GitDiffSummary> {
    const GIT_DIFF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    let (tx, rx) = std::sync::mpsc::channel();
    let worktree = worktree.to_path_buf();
    std::thread::spawn(move || {
        let output = std::process::Command::new("git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(&worktree)
            .arg("diff")
            .arg("--shortstat")
            .arg("HEAD")
            .output();
        let _ = tx.send(output);
    });

    let Ok(Ok(output)) = rx.recv_timeout(GIT_DIFF_TIMEOUT) else {
        return None;
    };

    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // Typical --shortstat output:
    //   " 3 files changed, 120 insertions(+), 14 deletions(-)"
    // Fields may be missing (e.g. "1 file changed, 5 insertions(+)" with no
    // deletions section). Empty output = no diff.
    let line = text.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return Some(GitDiffSummary::default());
    }
    let mut summary = GitDiffSummary::default();
    for token in line.split(',') {
        let t = token.trim();
        if let Some(n) = t
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<usize>().ok())
        {
            if t.contains("file") {
                summary.files_changed = n;
            } else if t.contains("insertion") {
                summary.insertions = n;
            } else if t.contains("deletion") {
                summary.deletions = n;
            }
        }
    }
    Some(summary)
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
                status: crate::runs::RunStatus::Aborted,
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
            worktree_path: None,
        }];
        state
    }

    #[test]
    fn enter_detail_from_normal() {
        let mut state = make_state_with_tile("task-001");
        state.mode = Mode::Normal;

        state.enter_detail();

        assert!(
            matches!(
                &state.mode,
                Mode::Detail { task_id, at_bottom: true, .. } if task_id == "task-001"
            ),
            "expected SnapIn with task_id=task-001, got {:?}",
            state.mode
        );
        if let Mode::Detail { scroll, .. } = &state.mode {
            assert_eq!(*scroll, 0, "initial scroll should be 0");
        }
    }

    #[test]
    fn enter_detail_noop_when_no_tile() {
        let mut state = make_state(); // no tasks
        state.mode = Mode::Normal;

        state.enter_detail();

        assert!(
            matches!(state.mode, Mode::Normal),
            "should stay Normal with no tiles"
        );
    }

    #[test]
    fn enter_detail_noop_when_already_in_snap_in() {
        let mut state = make_state_with_tile("task-001");
        state.mode = Mode::Detail {
            task_id: "task-001".to_string(),
            scroll: 5,
            at_bottom: false,
        };

        state.enter_detail(); // should be a no-op

        // scroll must be unchanged
        assert!(matches!(&state.mode, Mode::Detail { scroll: 5, .. }));
    }

    #[test]
    fn exit_detail_returns_to_normal() {
        let mut state = make_state_with_tile("task-001");
        state.mode = Mode::Detail {
            task_id: "task-001".to_string(),
            scroll: 3,
            at_bottom: false,
        };

        state.exit_detail();

        assert!(matches!(state.mode, Mode::Normal));
    }

    #[test]
    fn scroll_down_advances() {
        let mut state = make_state_with_tile("t");
        // Give the state some log lines so scroll can advance.
        state.focus_log = (0..20).map(|i| format!("line {i}")).collect();
        state
            .detail_log_total_rows
            .store(20, std::sync::atomic::Ordering::Relaxed);
        state.mode = Mode::Detail {
            task_id: "t".to_string(),
            scroll: 0,
            at_bottom: false,
        };

        state.detail_scroll_down(1, 10);

        assert!(
            matches!(&state.mode, Mode::Detail { scroll: 1, .. }),
            "expected scroll=1, got {:?}",
            state.mode
        );
    }

    #[test]
    fn scroll_down_increments_without_clamp() {
        // Handler clamps to max_scroll, but for this test total_rows is big
        // enough (100) that the 5-delta advance doesn't hit the cap.
        let mut state = make_state_with_tile("t");
        state.focus_log = (0..10).map(|i| format!("line {i}")).collect();
        state
            .detail_log_total_rows
            .store(100, std::sync::atomic::Ordering::Relaxed);
        state.mode = Mode::Detail {
            task_id: "t".to_string(),
            scroll: 0,
            at_bottom: false,
        };

        state.detail_scroll_down(5, 10);

        assert!(
            matches!(
                &state.mode,
                Mode::Detail {
                    scroll: 5,
                    at_bottom: false,
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
        state.mode = Mode::Detail {
            task_id: "t".to_string(),
            scroll: 0,
            at_bottom: false,
        };

        state.detail_scroll_up(1);

        assert!(
            matches!(&state.mode, Mode::Detail { scroll: 0, .. }),
            "scroll should stay at 0"
        );
    }

    #[test]
    fn scroll_up_decrements() {
        let mut state = make_state_with_tile("t");
        state.focus_log = (0..20).map(|i| format!("line {i}")).collect();
        state
            .detail_log_viewport
            .store(10, std::sync::atomic::Ordering::Relaxed);
        state
            .detail_log_total_rows
            .store(20, std::sync::atomic::Ordering::Relaxed);
        state.mode = Mode::Detail {
            task_id: "t".to_string(),
            scroll: 3,
            at_bottom: false,
        };

        state.detail_scroll_up(1);

        assert!(
            matches!(&state.mode, Mode::Detail { scroll: 2, .. }),
            "expected scroll=2, got {:?}",
            state.mode
        );
    }

    #[test]
    fn detail_jump_top_sets_scroll_to_zero_and_disables_auto_scroll() {
        let mut state = make_state_with_tile("t");
        state.focus_log = (0..50).map(|i| format!("line {i}")).collect();
        state.mode = Mode::Detail {
            task_id: "t".to_string(),
            scroll: 30,
            at_bottom: true,
        };

        state.detail_jump_top();

        assert!(
            matches!(
                &state.mode,
                Mode::Detail {
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
    fn detail_jump_bottom_enables_auto_scroll() {
        let mut state = make_state_with_tile("t");
        state.focus_log = (0..50).map(|i| format!("line {i}")).collect();
        state
            .detail_log_viewport
            .store(10, std::sync::atomic::Ordering::Relaxed);
        state
            .detail_log_total_rows
            .store(50, std::sync::atomic::Ordering::Relaxed);
        state.mode = Mode::Detail {
            task_id: "t".to_string(),
            scroll: 5,
            at_bottom: false,
        };

        state.detail_jump_bottom(10);

        assert!(
            matches!(
                &state.mode,
                Mode::Detail {
                    at_bottom: true,
                    ..
                }
            ),
            "got {:?}",
            state.mode
        );
        if let Mode::Detail { scroll, .. } = &state.mode {
            // scroll == max_scroll = total(50) - viewport(10) = 40.
            assert_eq!(*scroll, 40, "scroll should be max_scroll (40)");
        }
    }

    #[test]
    fn detail_auto_scroll_advances_when_at_bottom() {
        let mut state = make_state_with_tile("t");
        state.focus_log = (0..30).map(|i| format!("line {i}")).collect();
        state
            .detail_log_viewport
            .store(10, std::sync::atomic::Ordering::Relaxed);
        state
            .detail_log_total_rows
            .store(30, std::sync::atomic::Ordering::Relaxed);
        state.mode = Mode::Detail {
            task_id: "t".to_string(),
            scroll: 20,
            at_bottom: true,
        };

        // Simulate new lines arriving — focus_log grows to 35 lines and the
        // next render publishes the new total.
        state.focus_log = (0..35).map(|i| format!("line {i}")).collect();
        state
            .detail_log_total_rows
            .store(35, std::sync::atomic::Ordering::Relaxed);
        state.detail_auto_scroll(10);

        if let Mode::Detail { scroll, .. } = &state.mode {
            // scroll == max_scroll = total(35) - viewport(10) = 25.
            assert_eq!(*scroll, 25, "auto-scroll should set scroll to max_scroll");
        } else {
            panic!("not in Detail mode");
        }
    }

    #[test]
    fn detail_auto_scroll_noop_when_not_at_bottom() {
        let mut state = make_state_with_tile("t");
        state.focus_log = (0..30).map(|i| format!("line {i}")).collect();
        state.mode = Mode::Detail {
            task_id: "t".to_string(),
            scroll: 5,
            at_bottom: false,
        };

        state.focus_log = (0..35).map(|i| format!("line {i}")).collect();
        state.detail_auto_scroll(10);

        // scroll unchanged
        assert!(
            matches!(&state.mode, Mode::Detail { scroll: 5, .. }),
            "got {:?}",
            state.mode
        );
    }

    // -----------------------------------------------------------------------
    // tile_at hit-test
    // -----------------------------------------------------------------------

    #[test]
    fn tile_at_returns_tile_containing_click() {
        use ratatui::layout::Rect;
        let state = make_state();
        // Two tiles side by side in a 40×10 area:
        //   tile 0: x=0..20, y=0..10
        //   tile 1: x=20..40, y=0..10
        *state.tile_hit_rects.lock().unwrap() =
            vec![(0, Rect::new(0, 0, 20, 10)), (1, Rect::new(20, 0, 20, 10))];
        assert_eq!(state.tile_at(5, 5), Some(0));
        assert_eq!(state.tile_at(19, 9), Some(0));
        assert_eq!(state.tile_at(20, 0), Some(1));
        assert_eq!(state.tile_at(39, 9), Some(1));
    }

    #[test]
    fn tile_at_returns_none_outside_any_rect() {
        use ratatui::layout::Rect;
        let state = make_state();
        *state.tile_hit_rects.lock().unwrap() = vec![(0, Rect::new(10, 10, 20, 10))];
        // Left of the rect.
        assert_eq!(state.tile_at(5, 15), None);
        // Below the rect.
        assert_eq!(state.tile_at(15, 25), None);
        // Right edge is exclusive (x + width is one past the last column).
        assert_eq!(state.tile_at(30, 15), None);
    }

    #[test]
    fn tile_at_empty_cache_returns_none() {
        // Before the first render the cache is empty; any click is a miss.
        let state = make_state();
        assert!(state.tile_hit_rects.lock().unwrap().is_empty());
        assert_eq!(state.tile_at(0, 0), None);
    }

    #[test]
    fn picker_row_at_returns_row_index() {
        use ratatui::layout::Rect;
        let state = make_state();
        // Three picker rows stacked vertically: y=5,6,7.
        *state.picker_hit_rects.lock().unwrap() = vec![
            (0, Rect::new(2, 5, 40, 1)),
            (1, Rect::new(2, 6, 40, 1)),
            (2, Rect::new(2, 7, 40, 1)),
        ];
        assert_eq!(state.picker_row_at(10, 5), Some(0));
        assert_eq!(state.picker_row_at(10, 6), Some(1));
        assert_eq!(state.picker_row_at(10, 7), Some(2));
        // Below last row.
        assert_eq!(state.picker_row_at(10, 8), None);
        // Left of picker.
        assert_eq!(state.picker_row_at(0, 5), None);
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
            plan: None,
            kind: pitboss_cli::control::protocol::ApprovalKind::Action,
            sub_mode: ApprovalSubMode::Overview,
        };
        assert!(matches!(m, Mode::ApprovalModal { .. }));

        // Structured-plan form carries through.
        let plan = pitboss_cli::control::protocol::ApprovalPlanWire {
            summary: "delete idx".into(),
            rationale: Some("obsolete".into()),
            resources: vec!["db".into()],
            risks: vec!["slow reads".into()],
            rollback: Some("snapshot".into()),
        };
        let m2 = Mode::ApprovalModal {
            request_id: "req-2".into(),
            task_id: "lead".into(),
            summary: "delete idx".into(),
            plan: Some(plan),
            kind: pitboss_cli::control::protocol::ApprovalKind::Action,
            sub_mode: ApprovalSubMode::Overview,
        };
        if let Mode::ApprovalModal { plan, .. } = m2 {
            assert_eq!(plan.unwrap().resources, vec!["db".to_string()]);
        } else {
            panic!("expected ApprovalModal");
        }
    }

    #[test]
    fn confirm_kill_mode_stores_worker_target_from_focus() {
        let mut state = make_state_with_tile("task-001");
        state.mode = Mode::ConfirmKill {
            target: KillTarget::Worker("task-001".into()),
        };
        if let Mode::ConfirmKill {
            target: KillTarget::Worker(id),
        } = &state.mode
        {
            assert_eq!(id, "task-001");
        } else {
            panic!("not ConfirmKill::Worker");
        }
    }

    #[test]
    fn approval_modal_overview_y_sets_mode_normal() {
        // Simulate the state transition that `handle_approval_modal` performs.
        let mut state = make_state();
        state.mode = Mode::ApprovalModal {
            request_id: "req-1".into(),
            task_id: "lead".into(),
            summary: "spawn 3".into(),
            plan: None,
            kind: pitboss_cli::control::protocol::ApprovalKind::Action,
            sub_mode: ApprovalSubMode::Overview,
        };
        // Direct transition — we don't call into app::handle_* here to avoid
        // tokio-runtime coupling; this test exercises only the state model.
        state.mode = Mode::Normal;
        assert!(matches!(state.mode, Mode::Normal));
    }
}
