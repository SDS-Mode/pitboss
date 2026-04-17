use serde::{Deserialize, Serialize};

/// A parsed stream-json event from the Claude Code subprocess.
///
/// Each enum variant corresponds to a top-level `"type"` in the wire format.
/// Unknown types are captured verbatim in [`Event::Unknown`] to tolerate
/// additions to the Claude Code wire format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    System {
        subtype: Option<String>,
    },
    AssistantText {
        text: String,
    },
    AssistantToolUse {
        tool_name: String,
        input_summary: String,
    },
    ToolResult {
        content_summary: String,
    },
    Result {
        subtype: Option<String>,
        session_id: String,
        text: Option<String>,
        usage: TokenUsage,
    },
    /// Rate-limit notice emitted by Claude Code. Surfaced as a first-class event
    /// so operators can see throttling without parsing Unknown payloads.
    RateLimit {
        status: String,
        rate_limit_type: Option<String>,
        resets_at: Option<u64>,
    },
    Unknown {
        raw: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl TokenUsage {
    pub fn add(&mut self, other: &TokenUsage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_creation += other.cache_creation;
    }
}
