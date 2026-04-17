use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::SessionState;
use crate::parser::TokenUsage;

/// Result of running a single session to completion or cancellation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionOutcome {
    pub final_state: SessionState,
    pub exit_code: Option<i32>,
    pub token_usage: TokenUsage,
    pub claude_session_id: Option<String>,
    pub final_message_preview: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
}

impl SessionOutcome {
    #[must_use]
    pub fn duration_ms(&self) -> i64 {
        (self.ended_at - self.started_at).num_milliseconds()
    }
}
