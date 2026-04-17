//! Application state types for the Mosaic TUI.

use std::path::PathBuf;

use mosaic_core::store::TaskStatus;

/// Overall display mode of the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    ViewingLog,
    Help,
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
    #[allow(dead_code)]
    pub exit_code: Option<i32>,
    pub log_path: PathBuf,
}

/// Full application state updated each poll cycle.
#[derive(Debug)]
pub struct AppState {
    #[allow(dead_code)]
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

    /// Update tasks from a snapshot while preserving focus by task id.
    pub fn apply_snapshot(&mut self, snapshot: AppSnapshot) {
        let focused_id = self.tasks.get(self.focus).map(|t| t.id.clone());

        self.tasks = snapshot.tasks;
        self.focus_log = snapshot.focus_log;
        self.failed_count = snapshot.failed_count;

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
}
