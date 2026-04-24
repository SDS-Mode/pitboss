//! Operator-declared approval policy. Deterministic rule matcher
//! over fixed PendingApproval fields. NOT executed by an LLM.
//!
//! Rules are TOML-declared in the manifest under [[approval_policy]].
//! The matcher applies rules in declaration order; the first match
//! wins. If no rule matches, the fallback (manifest-level or
//! per-approval) applies.

use serde::{Deserialize, Serialize};

use crate::dispatch::state::PendingApproval;
use crate::mcp::approval::ApprovalCategory;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRule {
    #[serde(default)]
    pub r#match: ApprovalMatch,
    pub action: ApprovalAction,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApprovalMatch {
    /// Match exact actor path (e.g., "root→S1")
    #[serde(default)]
    pub actor: Option<String>,
    /// Match category exactly
    #[serde(default)]
    pub category: Option<ApprovalCategory>,
    /// Match exact tool name (relevant for ToolUse category)
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Match if approval's cost > this value (relevant for Cost category)
    #[serde(default)]
    pub cost_over: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalAction {
    AutoApprove,
    AutoReject,
    /// Surface to operator (overrides default if default would be auto).
    Block,
}

pub struct PolicyMatcher {
    rules: Vec<ApprovalRule>,
}

impl PolicyMatcher {
    pub fn new(rules: Vec<ApprovalRule>) -> Self {
        Self { rules }
    }

    pub fn rules(&self) -> &[ApprovalRule] {
        &self.rules
    }

    /// Returns the first matching action, or None if no rule matches.
    pub fn evaluate(
        &self,
        approval: &PendingApproval,
        tool_name: Option<&str>,
        cost: Option<f64>,
    ) -> Option<ApprovalAction> {
        for rule in &self.rules {
            if rule_matches(&rule.r#match, approval, tool_name, cost) {
                return Some(rule.action);
            }
        }
        None
    }
}

fn rule_matches(
    m: &ApprovalMatch,
    approval: &PendingApproval,
    tool_name: Option<&str>,
    cost: Option<f64>,
) -> bool {
    if let Some(want) = &m.actor {
        if approval.actor_path.to_string() != *want {
            return false;
        }
    }
    if let Some(want) = m.category {
        if approval.category != want {
            return false;
        }
    }
    if let Some(want) = &m.tool_name {
        match tool_name {
            Some(actual) if actual == want => {}
            _ => return false,
        }
    }
    // TODO(Phase 4.x): emit ApprovalCategory::Cost approvals so cost_over
    // rules can fire in practice. Until then, cost_over rules only match when
    // the caller passes an explicit cost_estimate hint via RequestApprovalArgs.
    if let Some(threshold) = m.cost_over {
        match cost {
            Some(actual) if actual > threshold => {}
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::actor::ActorPath;

    fn mk_approval(actor: ActorPath, category: ApprovalCategory) -> PendingApproval {
        PendingApproval {
            id: uuid::Uuid::now_v7(),
            requesting_actor_id: actor.leaf().cloned().unwrap_or_default(),
            actor_path: actor,
            category,
            summary: "test".into(),
            plan: None,
            blocks: vec![],
            created_at: chrono::Utc::now(),
            ttl_secs: 60,
            fallback: crate::mcp::approval::ApprovalFallback::AutoReject,
        }
    }

    #[test]
    fn empty_rule_set_returns_none() {
        let m = PolicyMatcher::new(vec![]);
        let a = mk_approval(ActorPath::new(["root"]), ApprovalCategory::ToolUse);
        assert!(m.evaluate(&a, None, None).is_none());
    }

    #[test]
    fn actor_match_returns_action() {
        let m = PolicyMatcher::new(vec![ApprovalRule {
            r#match: ApprovalMatch {
                actor: Some("root→S1".into()),
                ..Default::default()
            },
            action: ApprovalAction::AutoApprove,
        }]);
        let a = mk_approval(ActorPath::new(["root", "S1"]), ApprovalCategory::ToolUse);
        assert_eq!(
            m.evaluate(&a, None, None),
            Some(ApprovalAction::AutoApprove)
        );
    }

    #[test]
    fn first_match_wins() {
        let m = PolicyMatcher::new(vec![
            ApprovalRule {
                r#match: ApprovalMatch {
                    category: Some(ApprovalCategory::ToolUse),
                    ..Default::default()
                },
                action: ApprovalAction::AutoApprove,
            },
            ApprovalRule {
                r#match: ApprovalMatch {
                    category: Some(ApprovalCategory::ToolUse),
                    ..Default::default()
                },
                action: ApprovalAction::AutoReject,
            },
        ]);
        let a = mk_approval(ActorPath::new(["root"]), ApprovalCategory::ToolUse);
        assert_eq!(
            m.evaluate(&a, None, None),
            Some(ApprovalAction::AutoApprove)
        );
    }

    #[test]
    fn cost_over_threshold_matches() {
        let m = PolicyMatcher::new(vec![ApprovalRule {
            r#match: ApprovalMatch {
                cost_over: Some(0.50),
                ..Default::default()
            },
            action: ApprovalAction::Block,
        }]);
        let a = mk_approval(ActorPath::new(["root"]), ApprovalCategory::Cost);
        assert_eq!(
            m.evaluate(&a, None, Some(1.00)),
            Some(ApprovalAction::Block)
        );
        assert_eq!(m.evaluate(&a, None, Some(0.10)), None);
    }

    #[test]
    fn tool_name_match_requires_exact_string() {
        let m = PolicyMatcher::new(vec![ApprovalRule {
            r#match: ApprovalMatch {
                tool_name: Some("Bash".into()),
                ..Default::default()
            },
            action: ApprovalAction::AutoReject,
        }]);
        let a = mk_approval(ActorPath::new(["root"]), ApprovalCategory::ToolUse);
        assert_eq!(
            m.evaluate(&a, Some("Bash"), None),
            Some(ApprovalAction::AutoReject)
        );
        assert_eq!(m.evaluate(&a, Some("Edit"), None), None);
        assert_eq!(m.evaluate(&a, None, None), None);
    }
}
