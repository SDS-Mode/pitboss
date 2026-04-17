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
        self.render();
    }

    pub fn mark_running(&mut self, task_id: &str) {
        if let Some(r) = self.find_mut(task_id) {
            r.status = Status::Running;
        }
        self.render();
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
        self.render();
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut Row> {
        self.rows.iter_mut().find(|r| r.task_id == id)
    }

    fn render(&mut self) {
        if self.is_tty {
            // Move cursor up by rendered_lines, clear, rewrite.
            if self.rendered_lines > 0 {
                print!("\x1b[{}A\x1b[J", self.rendered_lines);
            }
            let header = self.format_header();
            println!("{header}");
            for r in &self.rows {
                println!("{}", self.format_row(r));
            }
            self.rendered_lines = self.rows.len() + 1;
        } else {
            // Append-only: only render on state change of the last row.
            if let Some(last) = self.rows.last() {
                println!("{}", self.format_row(last));
            }
        }
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
