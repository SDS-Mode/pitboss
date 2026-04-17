use mosaic_core::store::{TaskRecord, TaskStatus};

pub struct ProgressTable {
    is_tty: bool,
    rows: Vec<Row>,
    rendered_lines: usize,
}

struct Row {
    task_id: String,
    status: Status,
    duration_ms: i64,
    tokens_in: u64,
    tokens_out: u64,
    tokens_cache: u64,
    exit_code: Option<i32>,
}

enum Status {
    Pending,
    Running,
    Done(TaskStatus),
}

impl ProgressTable {
    pub fn new(is_tty: bool) -> Self {
        Self {
            is_tty,
            rows: Vec::new(),
            rendered_lines: 0,
        }
    }

    pub fn register(&mut self, task_id: &str) {
        self.rows.push(Row {
            task_id: task_id.into(),
            status: Status::Pending,
            duration_ms: 0,
            tokens_in: 0,
            tokens_out: 0,
            tokens_cache: 0,
            exit_code: None,
        });
        if self.is_tty {
            self.render_tty();
        }
        // Non-TTY: silent on register. See `render_non_tty_done`.
    }

    pub fn mark_running(&mut self, task_id: &str) {
        if let Some(r) = self.find_mut(task_id) {
            r.status = Status::Running;
        }
        if self.is_tty {
            self.render_tty();
        }
        // Non-TTY: silent on running transition. Noise in CI logs otherwise.
    }

    pub fn mark_done(&mut self, rec: &TaskRecord) {
        if let Some(r) = self.find_mut(&rec.task_id) {
            r.status = Status::Done(rec.status.clone());
            r.duration_ms = rec.duration_ms;
            r.tokens_in = rec.token_usage.input;
            r.tokens_out = rec.token_usage.output;
            r.tokens_cache = rec.token_usage.cache_read;
            r.exit_code = rec.exit_code;
        }
        if self.is_tty {
            self.render_tty();
        } else {
            // Non-TTY: emit exactly one line for the completed task.
            if let Some(line) = self.done_row_line(&rec.task_id) {
                println!("{line}");
            }
        }
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut Row> {
        self.rows.iter_mut().find(|r| r.task_id == id)
    }

    fn find(&self, id: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.task_id == id)
    }

    /// Returns the formatted line for a specific task (used in non-TTY mode
    /// and in unit tests). `None` if no such task is registered.
    pub(crate) fn done_row_line(&self, task_id: &str) -> Option<String> {
        self.find(task_id).map(|r| self.format_row(r))
    }

    fn render_tty(&mut self) {
        // Move cursor up by rendered_lines, clear, rewrite the full table.
        if self.rendered_lines > 0 {
            print!("\x1b[{}A\x1b[J", self.rendered_lines);
        }
        let header = self.format_header();
        println!("{header}");
        for r in &self.rows {
            println!("{}", self.format_row(r));
        }
        self.rendered_lines = self.rows.len() + 1;
    }

    #[allow(clippy::unused_self)]
    fn format_header(&self) -> String {
        format!(
            "{:<20} {:<12} {:>8} {:<22} {:>4}",
            "TASK", "STATUS", "TIME", "TOKENS (in/out/cache)", "EXIT"
        )
    }

    #[allow(clippy::unused_self)]
    fn format_row(&self, r: &Row) -> String {
        let status = match &r.status {
            Status::Pending => "… Pending".to_string(),
            Status::Running => "● Running".to_string(),
            Status::Done(TaskStatus::Success) => "✓ Success".to_string(),
            Status::Done(TaskStatus::Failed) => "✗ Failed".to_string(),
            Status::Done(TaskStatus::TimedOut) => "⏱ TimedOut".to_string(),
            Status::Done(TaskStatus::Cancelled) => "⊘ Cancelled".to_string(),
            Status::Done(TaskStatus::SpawnFailed) => "! SpawnFail".to_string(),
        };
        let time = if r.duration_ms == 0 {
            "—".to_string()
        } else {
            let secs = r.duration_ms / 1000;
            format!("{}m{:02}s", secs / 60, secs % 60)
        };
        let tokens = if r.tokens_in == 0 && r.tokens_out == 0 {
            "—".to_string()
        } else {
            format!("{} / {} / {}", r.tokens_in, r.tokens_out, r.tokens_cache)
        };
        let exit = r
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "—".to_string());
        format!(
            "{:<20} {:<12} {:>8} {:<22} {:>4}",
            r.task_id, status, time, tokens, exit
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosaic_core::parser::TokenUsage;

    fn rec(id: &str, status: TaskStatus, dur_ms: i64) -> TaskRecord {
        use chrono::Utc;
        use std::path::PathBuf;
        TaskRecord {
            task_id: id.into(),
            status,
            exit_code: Some(0),
            started_at: Utc::now(),
            ended_at: Utc::now(),
            duration_ms: dur_ms,
            worktree_path: None,
            log_path: PathBuf::new(),
            token_usage: TokenUsage {
                input: 1,
                output: 2,
                cache_read: 3,
                cache_creation: 4,
            },
            claude_session_id: None,
            final_message_preview: None,
        }
    }

    #[test]
    fn non_tty_done_row_shows_the_completed_task_not_the_last() {
        // Regression: non-TTY mode used to print `rows.last()` regardless of
        // which task completed, so stdout showed the last-registered task's
        // row repeatedly instead of the real completing task's row.
        let mut table = ProgressTable::new(false);
        table.register("alpha");
        table.register("beta");
        table.register("gamma");

        let alpha_line = table.done_row_line("alpha").unwrap();
        let beta_line = table.done_row_line("beta").unwrap();
        let gamma_line = table.done_row_line("gamma").unwrap();
        assert!(alpha_line.starts_with("alpha"), "alpha row: {alpha_line}");
        assert!(beta_line.starts_with("beta"), "beta row: {beta_line}");
        assert!(gamma_line.starts_with("gamma"), "gamma row: {gamma_line}");
        assert_ne!(alpha_line, gamma_line);
    }

    #[test]
    fn mark_done_updates_correct_row_by_id() {
        let mut table = ProgressTable::new(false);
        table.register("alpha");
        table.register("beta");

        table.mark_done(&rec("beta", TaskStatus::Success, 5000));

        let beta_after = table.done_row_line("beta").unwrap();
        assert!(
            beta_after.contains("Success"),
            "beta marked done: {beta_after}"
        );
        let alpha_after = table.done_row_line("alpha").unwrap();
        assert!(
            alpha_after.contains("Pending"),
            "alpha untouched: {alpha_after}"
        );
    }

    #[test]
    fn done_row_line_none_for_unknown_task() {
        let table = ProgressTable::new(false);
        assert!(table.done_row_line("nope").is_none());
    }
}
