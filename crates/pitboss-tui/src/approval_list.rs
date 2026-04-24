//! Approval list pane — non-modal queue view shown alongside the
//! tile grid. Selecting an item opens a detail modal for the
//! approve/reject decision.

use std::collections::VecDeque;

use crate::state::ApprovalListItem;

#[derive(Debug, Clone, Default)]
pub struct ApprovalListState {
    pub items: VecDeque<ApprovalListItem>,
    pub selected_idx: usize,
}

impl ApprovalListState {
    pub fn line_for(&self, item: &ApprovalListItem) -> String {
        let age = (chrono::Utc::now() - item.created_at).num_seconds();
        format!(
            "[{}] {} ({}s) — {}",
            item.actor_path,
            item.category_str(),
            age.max(0),
            item.summary,
        )
    }

    pub fn move_selection_down(&mut self) {
        if !self.items.is_empty() {
            self.selected_idx = (self.selected_idx + 1) % self.items.len();
        }
    }

    pub fn move_selection_up(&mut self) {
        if !self.items.is_empty() {
            self.selected_idx = if self.selected_idx == 0 {
                self.items.len() - 1
            } else {
                self.selected_idx - 1
            };
        }
    }

    pub fn current(&self) -> Option<&ApprovalListItem> {
        self.items.get(self.selected_idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ApprovalListItem;

    fn mk_item(path: &str, summary: &str) -> ApprovalListItem {
        ApprovalListItem {
            id: format!("req-{}", uuid::Uuid::now_v7()),
            actor_path: path.into(),
            category: "action".into(),
            summary: summary.into(),
            plan: None,
            kind: pitboss_cli::control::protocol::ApprovalKind::Action,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn selection_moves_modulo_items() {
        let mut s = ApprovalListState::default();
        s.items.push_back(mk_item("root", "a"));
        s.items.push_back(mk_item("root→S1", "b"));
        s.items.push_back(mk_item("root→S2", "c"));
        assert_eq!(s.selected_idx, 0);
        s.move_selection_down();
        assert_eq!(s.selected_idx, 1);
        s.move_selection_down();
        s.move_selection_down();
        assert_eq!(s.selected_idx, 0);
        s.move_selection_up();
        assert_eq!(s.selected_idx, 2);
    }

    #[test]
    fn line_format_includes_path_age_summary() {
        let s = ApprovalListState::default();
        let line = s.line_for(&mk_item("root→S1", "destructive op"));
        assert!(line.contains("root→S1"));
        assert!(line.contains("destructive op"));
        assert!(line.contains("action"));
    }
}
