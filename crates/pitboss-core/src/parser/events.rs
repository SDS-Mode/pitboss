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
        /// Populated when the system event carries a `session_id` field —
        /// notably the `subtype:"init"` event Claude Code emits at the start
        /// of every session, well before any `Result` event lands. Lets the
        /// dispatcher publish the resumable session id immediately on init
        /// rather than blocking the full run duration waiting for the
        /// terminal `Event::Result` to fire its `session_id_tx`. (#149 M5)
        session_id: Option<String>,
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
    /// Reasoning tokens reported by providers that expose them. `None` for
    /// Anthropic/Claude-shaped streams and older persisted records.
    #[serde(default)]
    pub reasoning: Option<u64>,
}

impl TokenUsage {
    /// Saturating add — clamps at `u64::MAX` instead of overflowing.
    /// `u64` token totals are practically unreachable in production
    /// (10^19 tokens is millions of years at typical claude rates),
    /// but the dispatcher accumulates `total_token_usage` across
    /// kill+resume iterations of long-running leads with no per-
    /// iteration cap; the saturation guard makes overflow impossible
    /// rather than merely improbable. Emits a `tracing::warn!` if any
    /// field saturates so the rare event is visible. (#185 low)
    pub fn add(&mut self, other: &TokenUsage) {
        let prev = *self;
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_creation = self.cache_creation.saturating_add(other.cache_creation);
        self.reasoning = match (self.reasoning, other.reasoning) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
        };
        let saturated = (self.input == u64::MAX && prev.input != u64::MAX)
            || (self.output == u64::MAX && prev.output != u64::MAX)
            || (self.cache_read == u64::MAX && prev.cache_read != u64::MAX)
            || (self.cache_creation == u64::MAX && prev.cache_creation != u64::MAX)
            || (matches!(self.reasoning, Some(u64::MAX))
                && !matches!(prev.reasoning, Some(u64::MAX)));
        if saturated {
            tracing::warn!(
                "TokenUsage::add saturated at u64::MAX — token counter overflow \
                 should be impossible in practice; check for repeated \
                 accumulation in a tight loop"
            );
        }
    }
}
