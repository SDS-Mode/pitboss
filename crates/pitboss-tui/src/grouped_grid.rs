//! Grouped tile grid for depth-2 runs. Each sub-tree renders as a
//! collapsible container with a header strip (sub-lead id, budget
//! bar, worker count, approval-pending badge) and an inner row of
//! worker tiles. Root-layer workers render in a top row outside any
//! container.

use crate::state::SubtreeView;

#[derive(Debug, Clone)]
pub struct SubtreeContainer<'a> {
    pub sublead_id: &'a str,
    pub view: &'a SubtreeView,
    pub expanded: bool,
}

impl SubtreeContainer<'_> {
    pub fn header_text(&self) -> String {
        let arrow = if self.expanded {
            "\u{25BC}"
        } else {
            "\u{25B6}"
        };
        let approvals = if self.view.pending_approvals > 0 {
            format!(" | \u{26A0} {} approval", self.view.pending_approvals)
        } else {
            String::new()
        };
        format!(
            "\u{2500} {} ({}) ${:.2}/${:.2} | {} workers{} ",
            self.sublead_id,
            arrow,
            self.view.spent_usd,
            self.view.budget_usd.unwrap_or(0.0),
            self.view.workers.len(),
            approvals,
        )
    }

    pub fn height_when_expanded(&self) -> u16 {
        // Header + tile rows (1 row per 4 workers, min 1)
        #[allow(clippy::cast_possible_truncation)]
        let tile_rows = (self.view.workers.len().div_ceil(4).max(1) as u16).max(1);
        1 + tile_rows
    }

    pub fn height_when_collapsed(&self) -> u16 {
        1
    }

    /// Current rendered height given the current `expanded` flag.
    pub fn current_height(&self) -> u16 {
        if self.expanded {
            self.height_when_expanded()
        } else {
            self.height_when_collapsed()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{SubtreeView, TileState, TileStatus};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn mk_tile(id: &str) -> TileState {
        TileState {
            id: id.to_string(),
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
        }
    }

    fn mk_view(workers: usize, spent: f64, budget: f64, approvals: u32) -> SubtreeView {
        SubtreeView {
            workers: (0..workers)
                .map(|i| (format!("W{i}"), mk_tile(&format!("W{i}"))))
                .collect::<HashMap<_, _>>(),
            spent_usd: spent,
            budget_usd: Some(budget),
            pending_approvals: approvals,
            read_down: false,
        }
    }

    #[test]
    fn expanded_header_shows_chevron_down() {
        let view = mk_view(3, 2.30, 5.00, 1);
        let c = SubtreeContainer {
            sublead_id: "S1",
            view: &view,
            expanded: true,
        };
        let h = c.header_text();
        assert!(h.contains('\u{25BC}'), "got: {h}");
        assert!(h.contains("$2.30/$5.00"), "got: {h}");
        assert!(h.contains("3 workers"), "got: {h}");
        assert!(h.contains("1 approval"), "got: {h}");
    }

    #[test]
    fn collapsed_header_shows_chevron_right() {
        let view = mk_view(2, 0.80, 5.00, 0);
        let c = SubtreeContainer {
            sublead_id: "S2",
            view: &view,
            expanded: false,
        };
        let h = c.header_text();
        assert!(h.contains('\u{25B6}'), "got: {h}");
        assert!(
            !h.contains("approval"),
            "no approval badge when count=0: {h}"
        );
    }

    #[test]
    fn collapsed_height_is_one_line() {
        let view = mk_view(8, 0.0, 1.0, 0);
        let c = SubtreeContainer {
            sublead_id: "S3",
            view: &view,
            expanded: false,
        };
        assert_eq!(c.height_when_collapsed(), 1);
    }

    #[test]
    fn expanded_height_grows_with_worker_count() {
        let view4 = mk_view(4, 0.0, 1.0, 0);
        let view5 = mk_view(5, 0.0, 1.0, 0);
        let view8 = mk_view(8, 0.0, 1.0, 0);
        assert_eq!(
            SubtreeContainer {
                sublead_id: "S",
                view: &view4,
                expanded: true
            }
            .height_when_expanded(),
            2
        );
        assert_eq!(
            SubtreeContainer {
                sublead_id: "S",
                view: &view5,
                expanded: true
            }
            .height_when_expanded(),
            3
        );
        assert_eq!(
            SubtreeContainer {
                sublead_id: "S",
                view: &view8,
                expanded: true
            }
            .height_when_expanded(),
            3
        );
    }
}
