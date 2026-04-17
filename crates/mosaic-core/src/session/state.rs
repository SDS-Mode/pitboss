use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Terminal or in-flight state of one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Initializing,
    Running { since: DateTime<Utc> },
    Completed,
    Failed { message: String },
    TimedOut,
    Cancelled,
    SpawnFailed { message: String },
}

impl SessionState {
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed { .. }
                | Self::TimedOut
                | Self::Cancelled
                | Self::SpawnFailed { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializing_is_not_terminal() {
        assert!(!SessionState::Initializing.is_terminal());
    }

    #[test]
    fn completed_is_terminal() {
        assert!(SessionState::Completed.is_terminal());
    }

    #[test]
    fn cancelled_is_terminal() {
        assert!(SessionState::Cancelled.is_terminal());
    }

    #[test]
    fn failed_is_terminal() {
        assert!(SessionState::Failed {
            message: "x".into()
        }
        .is_terminal());
    }
}
