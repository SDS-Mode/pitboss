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
    /// First ~200 chars of the assistant's chosen final message, with an
    /// ellipsis if truncated. Stable for human-facing displays where a long
    /// blob would blow up the layout (TUI tile, status table, log line).
    /// Consumers that need the complete message should read `final_message`.
    pub final_message_preview: Option<String>,
    /// Untruncated text of the assistant's chosen final message. Same source
    /// as `final_message_preview` (longest non-trivial assistant turn) without
    /// the 200-char cap. `None` for sessions that never produced an assistant
    /// turn (spawn-failed, terminated before first response).
    #[serde(default)]
    pub final_message: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
}

impl SessionOutcome {
    #[must_use]
    pub fn duration_ms(&self) -> i64 {
        (self.ended_at - self.started_at).num_milliseconds()
    }
}
