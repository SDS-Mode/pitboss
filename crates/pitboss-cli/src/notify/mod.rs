//! Notification sink system. See
//! docs/superpowers/specs/2026-04-17-pitboss-v041-notifications-design.md
//! for the full design.

#![allow(dead_code)] // Wired up gradually across Tasks 2-21.

use serde::{Deserialize, Serialize};

/// Severity levels for `NotificationEnvelope`. Matches syslog heritage +
/// PagerDuty/Opsgenie conventions. Ordered so filters can say
/// `severity_min = "warning"` and include Error + Critical.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

/// Lifecycle events pitboss can emit to notification sinks. Typed
/// enum, not a context dict — sinks `match` exhaustively. Each
/// variant carries its own specific fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PitbossEvent {
    ApprovalRequest {
        request_id: String,
        task_id: String,
        summary: String,
    },
    RunFinished {
        run_id: String,
        tasks_total: usize,
        tasks_failed: usize,
        duration_ms: u64,
        spent_usd: f64,
    },
    BudgetExceeded {
        run_id: String,
        spent_usd: f64,
        budget_usd: f64,
    },
}

impl PitbossEvent {
    /// Short string identifier used for filter lists (`events = [...]`)
    /// and dedup_key construction.
    pub fn kind(&self) -> &'static str {
        match self {
            PitbossEvent::ApprovalRequest { .. } => "approval_request",
            PitbossEvent::RunFinished { .. } => "run_finished",
            PitbossEvent::BudgetExceeded { .. } => "budget_exceeded",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_ord_is_info_warning_error_critical() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        assert!(Severity::Error < Severity::Critical);
    }

    #[test]
    fn severity_serde_roundtrip() {
        let s = serde_json::to_string(&Severity::Warning).unwrap();
        assert_eq!(s, "\"warning\"");
        let back: Severity = serde_json::from_str("\"critical\"").unwrap();
        assert_eq!(back, Severity::Critical);
    }

    #[test]
    fn pitboss_event_approval_request_roundtrip() {
        let ev = PitbossEvent::ApprovalRequest {
            request_id: "req-1".into(),
            task_id: "w-1".into(),
            summary: "spawn 3 workers".into(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"approval_request\""));
        assert!(s.contains("\"request_id\":\"req-1\""));
    }

    #[test]
    fn pitboss_event_run_finished_roundtrip() {
        let ev = PitbossEvent::RunFinished {
            run_id: "019d...".into(),
            tasks_total: 3,
            tasks_failed: 1,
            duration_ms: 12_345,
            spent_usd: 0.42,
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"run_finished\""));
        assert!(s.contains("\"tasks_failed\":1"));
    }

    #[test]
    fn pitboss_event_budget_exceeded_roundtrip() {
        let ev = PitbossEvent::BudgetExceeded {
            run_id: "019d...".into(),
            spent_usd: 1.51,
            budget_usd: 1.50,
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"budget_exceeded\""));
    }
}
