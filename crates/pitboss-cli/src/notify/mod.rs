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
}
