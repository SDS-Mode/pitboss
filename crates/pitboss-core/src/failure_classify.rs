//! Failure classification for completed claude subprocesses.
//!
//! Lives in `pitboss-core` rather than `pitboss-cli` so downstream
//! consumers (the HTTP `pitboss-web` console, future external integrations,
//! reporting tooling) can classify a log blob into a [`FailureReason`]
//! without having to depend on the CLI crate. The audit (#188 M1) flagged
//! the prior placement as a layering inversion — `FailureReason` was here
//! but its constructor lived in `pitboss-cli`.
//!
//! What's *not* moved:
//!   * `ApiHealth` / `SpawnGateReason` — operator-facing spawn-gate state,
//!     CLI-only.
//!   * `detect_failure_reason` — does file IO; trivial wrapper around
//!     [`classify`] that lives in CLI alongside the dispatch path.
//!   * `broadcast_worker_failed` — depends on CLI control-protocol types.
//!
//! Everything in this module is pure: blob → reason. The strategy is
//! conservative — markers come from observed claude CLI output, not
//! guesses; an unknown non-zero exit produces `FailureReason::Unknown`
//! carrying a short log excerpt rather than being misclassified.

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};

use crate::store::FailureReason;

/// Length cap on `message` excerpts embedded in `NetworkError`/`Unknown`
/// variants. Keeps `TaskRecord`s compact in storage; full context is still
/// in the log file for anyone who needs it.
pub const EXCERPT_MAX_CHARS: usize = 240;

/// Default back-off in seconds when a rate-limit marker is detected but the
/// CLI didn't emit a parseable `resets_at` timestamp. Mirrored for log-line
/// emission inside [`match_rate_limit`]; the operator-facing spawn gate
/// (in `pitboss-cli`) consults the same constant when applying the
/// fallback wait. (#185 medium)
pub const RATE_LIMIT_DEFAULT_BACKOFF_SECS: i64 = 300;

/// Classify a pre-read log blob into a [`FailureReason`]. Pure function —
/// no IO, no global state. Public for direct use by `pitboss-web`'s
/// log-tail importer and similar non-dispatcher callers.
///
/// **Two-stage matcher** (#185):
///
///   1. **Schema-first.** Walk the blob line-by-line, attempt to parse
///      each line as a stream-JSON event, and look for the upstream
///      Anthropic API error envelope shape (`{"error": {"type": …,
///      "message": …}}`, optionally nested under `result`). The
///      `error.type` strings (`rate_limit_exceeded`, `authentication_error`,
///      `invalid_request_error`) are part of the public Anthropic API
///      contract and are far more stable than scanning prose — when
///      present, they're authoritative.
///   2. **Substring fallback.** When no JSON envelope yields a
///      classification, fall through to the substring matchers below
///      (`match_auth` / `match_rate_limit` / …) which handle CLI banner
///      text (e.g. `"You've hit your limit · resets Apr 23, 3pm"`) and
///      shell-level errors (e.g. `getaddrinfo ENOTFOUND`) that don't
///      arrive as structured events.
///
/// Auth is checked before rate-limit at both stages: when both markers
/// coexist (e.g. an expired key hits burst-limit before the 401 is
/// returned) the run would otherwise cycle indefinitely — rate-limit
/// back-off clears on its own, auth failure does not. Classifying as
/// `AuthFailure` terminates the run promptly via the operator's
/// spawn-gate window, which is the correct response when credentials
/// are broken.
#[must_use]
pub fn classify(blob: &str) -> FailureReason {
    // Stage 1: schema-driven classification from stream-JSON events.
    // Collect every structured reason in declaration order, then pick by
    // priority below — auth wins when it co-occurs with rate-limit.
    let json_reasons: Vec<FailureReason> = blob.lines().filter_map(classify_event_line).collect();

    if json_reasons
        .iter()
        .any(|r| matches!(r, FailureReason::AuthFailure))
    {
        return FailureReason::AuthFailure;
    }
    // Sub-priority within JSON: ContextExceeded > RateLimit > InvalidArgument.
    // ContextExceeded outranks RateLimit because a too-long prompt cannot
    // be retried without operator action, while RateLimit will clear on
    // its own — surfacing ContextExceeded is the actionable signal.
    if let Some(reason) = json_reasons
        .iter()
        .find(|r| matches!(r, FailureReason::ContextExceeded))
    {
        return reason.clone();
    }
    if let Some(reason) = json_reasons
        .iter()
        .find(|r| matches!(r, FailureReason::RateLimit { .. }))
    {
        // Substring auth check guards against the case where the API
        // returned a rate-limit JSON event but the CLI banner also
        // showed an auth marker — keep the "auth wins" rule.
        if match_auth(blob).is_some() {
            return FailureReason::AuthFailure;
        }
        // Re-resolve `resets_at` from the full blob's CLI banner if the
        // JSON event didn't carry one — `match_rate_limit` already does
        // the timestamp parse + warn-on-fail, so prefer its output when
        // the schema path returned `resets_at: None`.
        if matches!(reason, FailureReason::RateLimit { resets_at: None }) {
            if let Some(banner) = match_rate_limit(blob) {
                return banner;
            }
        }
        return reason.clone();
    }
    if let Some(reason) = json_reasons.into_iter().next() {
        if match_auth(blob).is_some() {
            return FailureReason::AuthFailure;
        }
        return reason;
    }

    // Stage 2: substring fallback for non-JSON content.
    if let Some(reason) = match_auth(blob) {
        return reason;
    }
    if let Some(reason) = match_rate_limit(blob) {
        return reason;
    }
    if let Some(reason) = match_context_exceeded(blob) {
        return reason;
    }
    if let Some(reason) = match_invalid_argument(blob) {
        return reason;
    }
    if let Some(reason) = match_network(blob) {
        return reason;
    }
    FailureReason::Unknown {
        message: excerpt(blob),
    }
}

/// Stage-1 schema matcher: parse a single stream-JSON line and map a
/// recognized Anthropic API `error.type` to a [`FailureReason`].
///
/// Accepted envelope shapes:
///   * `{"type":"error","error":{"type":"…","message":"…"}}`
///   * `{"error":{"type":"…","message":"…"}}`
///   * `{"result":{"error":{"type":"…","message":"…"}}}` (claude SDK
///     wraps responses under `result` for some streaming flows)
///
/// Returns `None` on JSON parse failure, when no `error` envelope is
/// present, or when the `error.type` string doesn't map to a known
/// failure variant — the caller falls through to substring matching.
fn classify_event_line(line: &str) -> Option<FailureReason> {
    let trimmed = line.trim();
    // Cheap shape gate: stream-JSON events always start with `{`. Skip
    // everything else without paying for a JSON parse — the bulk of a
    // typical worker log is plain stdout from tool calls.
    if !trimmed.starts_with('{') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let err = v
        .get("error")
        .or_else(|| v.get("result").and_then(|r| r.get("error")))?;
    let err_type = err.get("type")?.as_str()?;
    let err_msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");

    match err_type {
        "rate_limit_exceeded" | "overloaded_error" => Some(FailureReason::RateLimit {
            resets_at: parse_reset_timestamp(err_msg),
        }),
        "authentication_error" => Some(FailureReason::AuthFailure),
        "invalid_request_error" => {
            // Disambiguate context-exceeded from generic invalid-request:
            // the API uses `invalid_request_error` for both, distinguished
            // only by the message body. Mirrors the substring path's
            // ContextExceeded > InvalidArgument priority.
            if err_msg.contains("prompt is too long") || err_msg.contains("context_length_exceeded")
            {
                Some(FailureReason::ContextExceeded)
            } else {
                // Preserve a useful excerpt — prefer the schema-supplied
                // `error.message` over scraping the whole blob since it's
                // the API's own description of what went wrong.
                let msg = if err_msg.is_empty() {
                    excerpt(line)
                } else {
                    excerpt(err_msg)
                };
                Some(FailureReason::InvalidArgument { message: msg })
            }
        }
        _ => None,
    }
}

fn match_rate_limit(blob: &str) -> Option<FailureReason> {
    // Claude CLI prints phrasings like:
    //   "You've hit your limit · resets Apr 23, 3pm"
    //   "rate_limit_exceeded"
    //   "usage limit reached"
    let hit = blob.contains("You've hit your limit")
        || blob.contains("rate_limit_exceeded")
        || blob.contains("rate limit exceeded")
        || blob.contains("usage limit reached");
    if !hit {
        return None;
    }
    let resets_at = parse_reset_timestamp(blob);
    if resets_at.is_none() {
        // Without an observable `resets_at`, the spawn gate falls back to
        // RATE_LIMIT_DEFAULT_BACKOFF_SECS (300s). Operators who saw "rate
        // limited, retrying in 5 minutes" with no further signal had no
        // way to know the actual reset time was much sooner. Surface the
        // parse failure so log-tailing operators (and integration tests)
        // can see why the default kicked in. (#185 medium)
        tracing::warn!(
            raw_excerpt = %reset_context(blob),
            default_backoff_secs = RATE_LIMIT_DEFAULT_BACKOFF_SECS,
            "rate-limit detected but reset_at parse failed; falling back to default backoff"
        );
    }
    Some(FailureReason::RateLimit { resets_at })
}

/// Pull a small, log-friendly excerpt around the `"resets "` marker so the
/// parse-failure warning has actionable context. Falls back to a short
/// tail of the blob when no marker is present (the rate-limit detector
/// matched on a phrasing that doesn't carry a reset hint, e.g. the bare
/// `"rate_limit_exceeded"` API error). Public so the CLI's reset-format
/// regression tests can drive it directly.
#[must_use]
pub fn reset_context(blob: &str) -> String {
    const CTX_CHARS: usize = 80;
    if let Some(idx) = blob.find("resets ") {
        let rest = &blob[idx..];
        let end = rest
            .find(['\n', '·', '|'])
            .unwrap_or(rest.len().min(CTX_CHARS));
        return rest[..end].trim().to_string();
    }
    // No "resets " marker — return a short tail so the operator at
    // least sees what triggered the rate-limit classification.
    let trimmed = blob.trim();
    let start = trimmed.chars().count().saturating_sub(CTX_CHARS);
    let tail: String = trimmed.chars().skip(start).collect();
    format!("(no `resets ` marker; tail: {tail:?})")
}

fn match_auth(blob: &str) -> Option<FailureReason> {
    let has_401 =
        blob.contains("401") && (blob.contains("Unauthorized") || blob.contains("Authentication"));
    let has_invalid_key = blob.contains("invalid_api_key");
    // Require "authentication_error" to co-occur with another auth signal so
    // prose mentions (e.g. "no authentication_error occurred") don't trigger
    // the 600-second backoff gate.
    let has_auth_error = blob.contains("authentication_error") && (has_401 || has_invalid_key);
    if has_invalid_key || has_auth_error || has_401 {
        Some(FailureReason::AuthFailure)
    } else {
        None
    }
}

fn match_context_exceeded(blob: &str) -> Option<FailureReason> {
    if blob.contains("context_length_exceeded") || blob.contains("prompt is too long") {
        Some(FailureReason::ContextExceeded)
    } else {
        None
    }
}

fn match_invalid_argument(blob: &str) -> Option<FailureReason> {
    if blob.contains("invalid_request_error") {
        Some(FailureReason::InvalidArgument {
            message: excerpt(blob),
        })
    } else {
        None
    }
}

fn match_network(blob: &str) -> Option<FailureReason> {
    let markers = [
        "ENOTFOUND",
        "ETIMEDOUT",
        "ECONNRESET",
        "ECONNREFUSED",
        "EAI_AGAIN",
        "getaddrinfo",
        "socket hang up",
        "network error",
    ];
    if markers.iter().any(|m| blob.contains(m)) {
        Some(FailureReason::NetworkError {
            message: excerpt(blob),
        })
    } else {
        None
    }
}

/// Number of leading characters of a `claude_session_id` to surface in
/// resume-hint diagnostic messages. Real Claude session ids are UUID-ish
/// (~36 chars); 8 is enough for an operator to disambiguate which session
/// failed without leaking the full id into log scrapers / chat dumps.
pub const RESUME_HINT_SESSION_PREFIX_CHARS: usize = 8;

/// Augment a [`FailureReason`] with a hint about a `--resume`-driven
/// dispatch when the classification is otherwise unhelpful. Pure
/// function — caller provides the `resume_session_id` from the spawn
/// args; `enrich_with_resume_hint` only modifies `Unknown` reasons,
/// since the specific markers (`RateLimit` / `AuthFailure` / `NetworkError`
/// / `ContextExceeded` / `InvalidArgument`) are authoritative explanations
/// that don't get clearer with a resume note.
///
/// **Why lazy fail-with-hint, not active validation:** validating the
/// session id at dispatch start would mean pitboss itself talks to the
/// Anthropic API (auth plumbing, rate-limit awareness, retry loop), all
/// duplicating what the claude subprocess does. The subprocess is the
/// authoritative source for "this session is invalid" — so we wait for
/// it to fail, then surface a hint that points the operator at the
/// actionable next step.
///
/// Issue #184. Pinned by the `enrich_with_resume_hint_*` tests below.
#[must_use]
pub fn enrich_with_resume_hint(
    reason: crate::store::FailureReason,
    resume_session_id: &str,
) -> crate::store::FailureReason {
    use crate::store::FailureReason;
    match reason {
        FailureReason::Unknown { message } => {
            let prefix: String = resume_session_id
                .chars()
                .take(RESUME_HINT_SESSION_PREFIX_CHARS)
                .collect();
            FailureReason::Unknown {
                message: format!(
                    "subprocess was started with --resume {prefix}…; the \
                     session id may be invalid (expired, revoked, or never \
                     existed). Re-run without --resume to start fresh, or \
                     run `pitboss resume <run-id>` against a more recent \
                     run. Original excerpt: {message}"
                ),
            }
        }
        // Specific classified reasons are authoritative — don't second-
        // guess them. A 401 / rate-limit / network error has the same
        // diagnosis whether or not --resume was used.
        other => other,
    }
}

/// Build a short, operator-readable excerpt from a log blob. Used by the
/// `Unknown` / `NetworkError` / `InvalidArgument` variants and surfaced
/// directly in the failures dashboard, so its output must be human-legible
/// — not a mid-codepoint, mid-line slice of stream-JSON.
///
/// **Strategy** (#222 / #223):
///
///   1. **Last full line.** Stream-JSON emits one event per line; "last
///      240 chars" almost always landed mid-event and produced output
///      like `put_tokens":0,"ephemeral_5m_input_tokens":0}...`. Walk
///      lines bottom-up to find the last non-empty line.
///   2. **JSON-aware extraction.** If that line parses as a stream-JSON
///      event, prefer (in order):
///         * `error.message` / `result.error.message` — the API's own
///           description of what went wrong.
///         * `result.result` (string) — claude SDK puts the agent's
///           final answer / human-readable error message here. When
///           the wrapper exits non-zero but the agent emitted a clean
///           result event (e.g. "There's an issue with the selected
///           model …"), this is the message the operator wants.
///   3. **Char-cap as the floor.** Cap at `EXCERPT_MAX_CHARS` codepoints
///      after extraction — never mid-codepoint (`chars()` is
///      codepoint-aware) and only mid-line if the chosen line itself
///      exceeds the cap.
#[must_use]
pub fn excerpt(blob: &str) -> String {
    let trimmed = blob.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Walk lines bottom-up to find the last non-empty line. A trailing
    // newline is common; skip blank lines to land on the actual final
    // event.
    let last_line = trimmed
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(trimmed)
        .trim();

    // Try JSON-aware extraction on the last line. Cheap shape gate first
    // — most lines are not JSON, and parsing every blob's tail eagerly
    // would burn CPU on the hot dispatch path.
    if last_line.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(last_line) {
            // Prefer error.message at any of the recognized locations.
            let err_msg = v
                .pointer("/error/message")
                .or_else(|| v.pointer("/result/error/message"))
                .and_then(|m| m.as_str());
            if let Some(msg) = err_msg.filter(|s| !s.is_empty()) {
                return cap_chars(msg);
            }
            // claude SDK final-answer / error string at /result.
            // String, not object — when the field is an object it's
            // typically a structured tool-call result and we'd rather
            // fall through to the raw line.
            if let Some(msg) = v.pointer("/result").and_then(|m| m.as_str()) {
                if !msg.is_empty() {
                    return cap_chars(msg);
                }
            }
        }
    }

    // #475 / #552: detect a truncated mid-event tail and surface it
    // explicitly. The 8 KiB tail-read window in `detect_failure_reason`
    // can produce two distinct truncation shapes:
    //
    //   A. `last_line` doesn't start with `{` — the tail-read landed
    //      mid-content of an event larger than 8 KiB (typical: a
    //      `tool_use` whose input contains base64, or a `tool_result`
    //      with multi-thousand-line cat output). The trailing envelope
    //      keys (`parent_tool_use_id`, `session_id`, `uuid`, `timestamp`)
    //      are visible but the leading `{` is offscreen.
    //
    //   B. `last_line` DOES start with `{` but `serde_json::from_str`
    //      above failed — the event is also truncated, just at its own
    //      EOF rather than at the head. The stdout was cut off before
    //      the closing brace landed.
    //
    // Both shapes look identical to the operator (an unparseable JSON
    // tail), so the same `[truncated mid-event]` marker is the right
    // surface. The detector unifies both: a `last_line` that fails to
    // parse end-to-end as JSON but carries the `":` key-value marker
    // is a truncated event. The length floor avoids marking short
    // non-JSON log lines that happen to contain `":` (rare but possible
    // in panic messages).
    if is_truncated_event_tail(last_line) {
        let tail = last_chars(last_line, 80);
        return format!("[truncated mid-event] …{tail}");
    }

    // Fallback: the last full line, capped. Still strictly better than
    // the pre-fix mid-line slice — at least the operator sees one
    // complete event boundary.
    cap_chars(last_line)
}

/// Detect a truncated stream-JSON event tail — either shape from
/// `excerpt`'s call site (mid-content with nested `{` chars before the
/// envelope, OR a starts-with-`{` event cut at its EOF). Both shapes
/// fail to parse end-to-end as JSON. The length floor and `":` key
/// marker together gate against false positives on short non-JSON
/// content. (#552)
fn is_truncated_event_tail(line: &str) -> bool {
    // Floor: real stream-JSON event tails are always at least a few
    // hundred bytes because they're returned from the 8 KiB tail-read
    // window. Anything shorter than 100 chars couldn't meaningfully
    // be a mid-event tail; the floor keeps short panic messages and
    // plain-log lines that happen to embed `":` (e.g. `error: {"a":1}`)
    // out of the marker path. The canonical bug-report shape from #475
    // is ~190 chars, so the floor sits comfortably below that.
    const MIN_TRUNCATED_LEN: usize = 100;
    if line.len() < MIN_TRUNCATED_LEN {
        return false;
    }
    // Clean stream-JSON parses end-to-end; anything that doesn't is
    // either truncated (the case we care about) or non-JSON entirely.
    // The `contains("\":")` check filters out the non-JSON case —
    // long log lines without JSON-key markers fall through to
    // `cap_chars` unchanged.
    if !line.contains("\":") {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(line).is_err()
}

/// Take the last `n` codepoints of `s`. Codepoint-aware so it never
/// produces a partial UTF-8 sequence.
fn last_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    s.chars().skip(count - n).collect()
}

/// Cap a string at `EXCERPT_MAX_CHARS` codepoints, taking the tail when
/// it overflows. Codepoint-aware so it never produces a partial UTF-8
/// sequence.
fn cap_chars(s: &str) -> String {
    let count = s.chars().count();
    if count <= EXCERPT_MAX_CHARS {
        return s.to_string();
    }
    let start = count - EXCERPT_MAX_CHARS;
    s.chars().skip(start).collect()
}

/// Parse a claude-CLI reset timestamp like `"resets Apr 23, 3pm"` into a
/// UTC `DateTime`. The CLI doesn't emit a year or timezone, so we assume
/// the current UTC year and treat the timestamp as UTC — imprecise by up
/// to a few hours but good enough to gate spawn decisions. Returns `None`
/// when no timestamp is found or parsing fails — the `RateLimit` marker
/// alone is still enough to classify, and the operator-facing spawn gate
/// can apply a default back-off when `resets_at` is missing.
#[must_use]
pub fn parse_reset_timestamp(blob: &str) -> Option<DateTime<Utc>> {
    let idx = blob.find("resets ")?;
    let rest = &blob[idx + "resets ".len()..];
    let end = rest.find(['\n', '·', '|']).unwrap_or(rest.len().min(40));
    let candidate = rest[..end].trim().trim_end_matches(['.', ',']);

    // Splits we expect: "Apr 23, 3pm" → ["Apr", "23,", "3pm"].
    let parts: Vec<&str> = candidate.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }
    let month = month_from_abbrev(parts[0])?;
    let day: u32 = parts[1].trim_end_matches(',').parse().ok()?;
    let time = parse_12h_time(parts[2])?;
    let now = Utc::now();
    let year = {
        use chrono::Datelike;
        now.year()
    };
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let naive = NaiveDateTime::new(date, time);
    let dt = Utc.from_utc_datetime(&naive);
    // If the parsed date is in the past the reset wraps into next year
    // (e.g., "resets Jan 1" seen on Dec 31).
    if dt < now {
        Some(
            NaiveDate::from_ymd_opt(year + 1, month, day)
                .map_or(dt, |d| Utc.from_utc_datetime(&NaiveDateTime::new(d, time))),
        )
    } else {
        Some(dt)
    }
}

fn month_from_abbrev(s: &str) -> Option<u32> {
    match s.to_ascii_lowercase().as_str() {
        "jan" => Some(1),
        "feb" => Some(2),
        "mar" => Some(3),
        "apr" => Some(4),
        "may" => Some(5),
        "jun" => Some(6),
        "jul" => Some(7),
        "aug" => Some(8),
        "sep" | "sept" => Some(9),
        "oct" => Some(10),
        "nov" => Some(11),
        "dec" => Some(12),
        _ => None,
    }
}

/// Parse strings like `"3pm"`, `"3PM"`, `"3:45pm"`, `"12:00am"` into
/// a `NaiveTime`. Returns `None` on any other shape.
fn parse_12h_time(s: &str) -> Option<NaiveTime> {
    let lower = s.to_ascii_lowercase();
    let (body, is_pm) = if let Some(b) = lower.strip_suffix("pm") {
        (b, true)
    } else if let Some(b) = lower.strip_suffix("am") {
        (b, false)
    } else {
        return None;
    };
    let (hour, minute) = if let Some((h, m)) = body.split_once(':') {
        (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?)
    } else {
        (body.parse::<u32>().ok()?, 0)
    };
    if !(1..=12).contains(&hour) || minute >= 60 {
        return None;
    }
    let hour24 = match (hour, is_pm) {
        (12, false) => 0, // 12am = 00:00
        (12, true) => 12, // 12pm = 12:00
        (h, false) => h,
        (h, true) => h + 12,
    };
    NaiveTime::from_hms_opt(hour24, minute, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coincident_auth_and_rate_limit_classifies_as_auth() {
        // Expired key hitting burst limits emits both markers. Auth must
        // win so the operator-facing spawn gate applies the longer
        // (terminal-in-practice) gate instead of cycling through rate-limit
        // resets.
        let blob = "You've hit your limit · resets Apr 23, 3pm\n\
                    authentication_error: invalid_api_key";
        assert!(matches!(classify(blob), FailureReason::AuthFailure));
    }

    #[test]
    fn rate_limit_hit_message_classifies_as_rate_limit() {
        let blob = "...streaming output...\nYou've hit your limit · resets Apr 23, 3pm\n";
        let r = classify(blob);
        assert!(matches!(r, FailureReason::RateLimit { .. }));
    }

    #[test]
    fn rate_limit_with_reset_timestamp_parses() {
        let blob = "You've hit your limit · resets Apr 23, 3pm";
        match classify(blob) {
            FailureReason::RateLimit {
                resets_at: Some(ts),
            } => {
                use chrono::{Datelike, Timelike};
                assert_eq!(ts.month(), 4);
                assert_eq!(ts.day(), 23);
                assert_eq!(ts.hour(), 15);
                assert_eq!(ts.minute(), 0);
            }
            other => panic!("expected RateLimit with timestamp, got {other:?}"),
        }
    }

    #[test]
    fn rate_limit_with_hour_minute_reset_parses() {
        let blob = "You've hit your limit · resets May 5, 9:45am";
        match classify(blob) {
            FailureReason::RateLimit {
                resets_at: Some(ts),
            } => {
                use chrono::{Datelike, Timelike};
                assert_eq!(ts.month(), 5);
                assert_eq!(ts.day(), 5);
                assert_eq!(ts.hour(), 9);
                assert_eq!(ts.minute(), 45);
            }
            other => panic!("expected RateLimit with timestamp, got {other:?}"),
        }
    }

    #[test]
    fn rate_limit_without_parseable_timestamp_still_classifies() {
        let blob = "rate_limit_exceeded (no timestamp here)";
        match classify(blob) {
            FailureReason::RateLimit { resets_at: None } => {}
            other => panic!("expected RateLimit with no timestamp, got {other:?}"),
        }
    }

    #[test]
    fn rate_limit_exceeded_api_error_classifies_as_rate_limit() {
        let blob = r#"{"type":"error","error":{"type":"rate_limit_exceeded"}}"#;
        let r = classify(blob);
        assert!(matches!(r, FailureReason::RateLimit { resets_at: None }));
    }

    #[test]
    fn network_marker_classifies_as_network_error() {
        let blob = "Error: getaddrinfo ENOTFOUND api.anthropic.com";
        match classify(blob) {
            FailureReason::NetworkError { message } => {
                assert!(message.contains("ENOTFOUND"));
            }
            other => panic!("expected NetworkError, got {other:?}"),
        }
    }

    #[test]
    fn invalid_api_key_classifies_as_auth() {
        let blob = r#"{"error":{"type":"authentication_error","message":"invalid_api_key"}}"#;
        assert!(matches!(classify(blob), FailureReason::AuthFailure));
    }

    #[test]
    fn context_exceeded_classifies_correctly() {
        let blob = r#"{"error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens"}}"#;
        // context_exceeded takes priority over invalid_argument.
        assert!(matches!(classify(blob), FailureReason::ContextExceeded));
    }

    #[test]
    fn invalid_request_error_without_context_classifies_as_invalid_argument() {
        let blob =
            r#"{"error":{"type":"invalid_request_error","message":"missing required field"}}"#;
        match classify(blob) {
            FailureReason::InvalidArgument { message } => {
                // Schema-first matcher (#185) extracts the API-supplied
                // `error.message` directly — more useful than scraping
                // the whole envelope. Check for the message body.
                assert!(
                    message.contains("missing required field"),
                    "expected schema-extracted error message, got: {message}"
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn no_marker_returns_unknown_with_excerpt() {
        let blob = "subprocess died\nsomething happened that we can't classify";
        match classify(blob) {
            FailureReason::Unknown { message } => {
                assert!(!message.is_empty());
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn excerpt_caps_length() {
        let blob = "a".repeat(EXCERPT_MAX_CHARS + 100);
        let e = excerpt(&blob);
        assert_eq!(e.chars().count(), EXCERPT_MAX_CHARS);
    }

    /// #222 regression: pre-fix the excerpt sliced the last 240 chars
    /// out of the blob, almost always landing mid-stream-JSON-event and
    /// producing operator-illegible output. Now we walk bottom-up to
    /// find the last full line.
    #[test]
    fn excerpt_takes_last_full_line_not_mid_event() {
        let blob = format!(
            "tool stdout line 1\n\
             tool stdout line 2\n\
             {{\"type\":\"result\",\"foo\":\"{}\",\"bar\":42}}",
            "x".repeat(50)
        );
        let e = excerpt(&blob);
        // Must NOT start with the middle of the JSON event.
        assert!(
            e.starts_with('{') || e.starts_with("\"type\""),
            "expected a clean line start, got: {e:?}"
        );
        assert!(!e.contains("tool stdout"));
    }

    /// #222 / #223 regression: when the last line is a stream-JSON
    /// `result` event with a string `result` field (claude SDK final
    /// answer / human-readable error), surface THAT as the message
    /// rather than the whole envelope. This is what made the failures
    /// dashboard legible after #222 — a worker that exited 1 because of
    /// a bad model name now shows "There's an issue with the selected
    /// model …" instead of `put_tokens":0,"ephemeral_5m_input_tokens":0}…`.
    #[test]
    fn excerpt_extracts_result_string_from_sdk_envelope() {
        let blob = r#"{"type":"result","subtype":"x","result":"There's an issue with the selected model (claude-haiku-4-5-bad). It may not exist or you may not have access to it.","stop_reason":"stop_sequence","session_id":"abc"}"#;
        let e = excerpt(blob);
        assert!(
            e.starts_with("There's an issue with the selected model"),
            "expected agent's result string, got: {e:?}"
        );
        assert!(!e.contains("\"stop_reason\""));
    }

    /// When the last line carries an `error.message`, prefer that over
    /// `result.result` — the API's own description is more authoritative.
    #[test]
    fn excerpt_prefers_error_message_over_result_string() {
        let blob = r#"{"error":{"type":"some_error","message":"the actionable description"},"result":"fallback string"}"#;
        let e = excerpt(blob);
        assert_eq!(e, "the actionable description");
    }

    /// `result.error.message` (nested envelope used by some streaming
    /// flows) is also picked up.
    #[test]
    fn excerpt_extracts_nested_result_error_message() {
        let blob = r#"{"result":{"error":{"type":"x","message":"nested actionable description"}}}"#;
        let e = excerpt(blob);
        assert_eq!(e, "nested actionable description");
    }

    /// Non-JSON tails fall through to last-line + cap; never produce
    /// mid-codepoint output.
    #[test]
    fn excerpt_non_json_tail_falls_back_to_last_line() {
        let blob = "stdout line 1\nstdout line 2\nERROR: something broke\n\n";
        let e = excerpt(blob);
        assert_eq!(e, "ERROR: something broke");
    }

    /// JSON parse failures fall through to last-line + cap rather than
    /// silently corrupting the message.
    #[test]
    fn excerpt_malformed_json_falls_back_to_last_line() {
        let blob = "earlier output\n{\"truncated\":";
        let e = excerpt(blob);
        // Falls back to the malformed line itself (capped).
        assert_eq!(e, "{\"truncated\":");
    }

    /// #475 regression: when the last stream-JSON line is itself larger
    /// than the 8 KiB tail-read window (e.g., a `tool_use` with a
    /// base64-encoded payload), the dispatcher's 8 KiB tail starts
    /// mid-line. Pre-fix `excerpt()` returned that mid-line slice
    /// verbatim — operator-illegible output like
    /// `kens":1325},"output_tokens":6,...` that resembles a usage
    /// envelope tail but isn't classified as one. Post-fix the
    /// truncation is surfaced explicitly with a `[truncated mid-event]`
    /// marker so the failures dashboard says "we didn't capture enough
    /// of the tail to classify" rather than rendering garbage.
    #[test]
    fn excerpt_marks_mid_event_truncation_when_tail_starts_mid_line() {
        let payload = format!(
            "earlier line 1\n\
             earlier line 2\n\
             {{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"input\":{{\"command\":\"{}\"}}}}]}},\"usage\":{{\"input_tokens\":1325,\"output_tokens\":6}},\"uuid\":\"abc\"}}",
            "x".repeat(9000)
        );
        // Simulate the 8 KiB tail read in `detect_failure_reason`.
        let mut tail_start = payload.len().saturating_sub(8 * 1024);
        while !payload.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        let tail = &payload[tail_start..];
        let e = excerpt(tail);
        assert!(
            e.starts_with("[truncated mid-event]"),
            "expected truncation marker, got: {e:?}"
        );
        // Anti-pattern from the bug report: the bare mid-line slice
        // resembling a usage envelope tail must NOT be returned.
        assert!(
            !e.contains("input_tokens\":1325},\"output_tokens\":6,") || e.starts_with("[truncated"),
            "mid-line slice must be marked, not returned bare: {e:?}"
        );
    }

    /// Companion check: prose tails without JSON-syntax artifacts are
    /// NOT misclassified as mid-event truncations.
    #[test]
    fn excerpt_does_not_flag_clean_prose_as_truncated() {
        let blob = "ERROR: something broke and we couldn't recover";
        let e = excerpt(blob);
        assert!(!e.starts_with("[truncated"));
        assert_eq!(e, "ERROR: something broke and we couldn't recover");
    }

    /// A mid-line tail starting with a JSON closing-brace and continuing
    /// with key-value pairs is the canonical shape the bug produced.
    #[test]
    fn excerpt_flags_classic_mid_envelope_shape() {
        // Reproduce the exact shape from the bug report.
        let blob = r#"kens":1325},"output_tokens":6,"service_tier":"standard","inference_geo":"not_available"},"context_management":null},"parent_tool_use_id":null,"session_id":"e7b8712b-...","uuid":"4d8ce2f9-..."}"#;
        let e = excerpt(blob);
        assert!(
            e.starts_with("[truncated mid-event]"),
            "expected truncation marker, got: {e:?}"
        );
    }

    /// #552 sub-case A: a mid-line tail that does NOT start with `{`
    /// but contains a nested `{` somewhere in `tool_result` content
    /// (e.g. line-numbered cat output with stringified struct dumps),
    /// followed by the trailing envelope keys (`parent_tool_use_id`,
    /// `session_id`, `uuid`, `timestamp`) at the end. The original
    /// `looks_like_mid_json_event` heuristic missed this because it
    /// only fired when `kv_idx < first_brace_idx` — when a nested `{`
    /// appears in content BEFORE the trailing envelope keys, the
    /// check returned false. With the parse-failure-based detector
    /// the marker fires correctly.
    #[test]
    fn excerpt_marks_nested_brace_in_content_as_truncated() {
        // Shape captured from the failing run 019e2ce1: the tail is
        // mid-content (line numbers + closing braces from a Rust
        // source file) plus the trailing envelope. Nested `{` from
        // the previous event's `init` block is also in the tail at
        // an earlier byte position.
        let blob = format!(
            "\\t            .await\\n1733\\t            .unwrap();\\n{}\
             1742\\t\"}}]}},\"parent_tool_use_id\":\"toolu_01GQfqpf5cp6eNaCqJ1j6e1C\",\
             \"session_id\":\"dab09735-b33c-40e5-83a1-16ea91f52c4a\",\
             \"uuid\":\"94e5a300-e2eb-4957-87f8-7406dd6c284d\",\
             \"timestamp\":\"2026-05-15T18:26:53.128Z\"}}",
            // Inject a nested `{...}` mid-content to confirm the new
            // detector doesn't misfire on it the way the old one did.
            r#"{"command":"cat -n foo.rs","description":"read file"}"#
        );
        let e = excerpt(&blob);
        assert!(
            e.starts_with("[truncated mid-event]"),
            "nested-{{ tail must be marked truncated; got: {e:?}"
        );
        // And the tail-portion of the marker contains the recognisable
        // trailing envelope key — confirms `last_chars(line, 80)`
        // surfaces the most useful 80 chars (the timestamp end, not
        // some opaque middle).
        assert!(
            e.contains("timestamp"),
            "tail should end with the envelope's timestamp field: {e:?}"
        );
    }

    /// #552 sub-case B: a `last_line` that DOES start with `{` but is
    /// truncated at its own EOF (subprocess was signal-killed
    /// mid-stdout-write). `serde_json::from_str` fails on the
    /// unbalanced braces. Pre-fix this fell through both detectors —
    /// `starts_with('{')` early-returned in `looks_like_mid_json_event`
    /// and the JSON parse failure was silent, landing in `cap_chars`
    /// and producing the same operator-illegible tail. With the
    /// parse-failure-based detector this case is caught.
    #[test]
    fn excerpt_marks_starts_with_brace_truncated_event_as_truncated() {
        // Real shape from worker-019e2cfa-c707 in run 019e2cf9: the
        // assistant event begins cleanly but the closing `}` never
        // arrives because the OOM-killer signal-killed the worker
        // mid-write. The line below is ~250 bytes — over the 200-byte
        // floor — and starts with `{"type":"assistant"...`.
        let blob = r#"{"type":"assistant","message":{"id":"msg_01ABC","model":"claude-sonnet-4-6","stop_reason":null,"content":[{"type":"text","text":"Let me read more of this file to understand the dispatcher layout before"#;
        // Sanity check: this line starts with `{` and is over the
        // length floor — the pre-fix path would have hit the cap_chars
        // fallback.
        assert!(blob.starts_with('{'));
        assert!(blob.len() > 200);
        let e = excerpt(blob);
        assert!(
            e.starts_with("[truncated mid-event]"),
            "starts-with-{{ truncated event must be marked; got: {e:?}"
        );
    }

    /// Negative companion: a starts-with-`{` line that IS a complete,
    /// parseable event must NOT get the truncation marker — the
    /// happy-path extraction (or `cap_chars` fallback) should run.
    /// Pin the parser-success branch so a future refactor can't
    /// regress to "always mark as truncated."
    #[test]
    fn excerpt_does_not_mark_clean_complete_event() {
        let blob = r#"{"type":"result","subtype":"success","session_id":"s","result":"the answer is 42","usage":{"input_tokens":10,"output_tokens":3}}"#;
        let e = excerpt(blob);
        assert!(
            !e.starts_with("[truncated mid-event]"),
            "clean complete event must not be marked truncated; got: {e:?}"
        );
        // And the happy path extracted the `result` string.
        assert_eq!(e, "the answer is 42");
    }

    /// Negative: a long non-JSON log line with no `":` key marker is
    /// NOT marked truncated. The `contains("\":")` filter is the gate
    /// that keeps plain log lines (Rust panic messages, banner
    /// errors, etc.) out of the marker path. Pin it so a refactor
    /// that loosens the filter to e.g. just `":"` would fail here.
    #[test]
    fn excerpt_does_not_mark_long_plain_log_line() {
        // 400-char prose with no JSON-key markers anywhere.
        let blob = "thread 'main' panicked at crates/foo/src/lib.rs:42:13: ".to_string()
            + &"unexpected condition in the validation step ".repeat(8);
        assert!(blob.len() > 200);
        assert!(!blob.contains("\":"));
        let e = excerpt(&blob);
        assert!(
            !e.starts_with("[truncated mid-event]"),
            "plain log line without `\":` must fall through to cap_chars; got: {e:?}"
        );
    }

    /// Empty `result.result` strings are skipped — they're not useful
    /// excerpts, fall back to the line.
    #[test]
    fn excerpt_skips_empty_result_string() {
        let blob = r#"{"type":"result","result":"","stop_reason":"end_turn"}"#;
        let e = excerpt(blob);
        // Falls back to the raw line; just assert it didn't return "".
        assert!(!e.is_empty());
        assert!(e.contains("end_turn"));
    }

    /// #185 medium regression: a rate-limit marker with a malformed
    /// `resets …` clause must not silently fall through to the
    /// 5-minute default backoff. The operator needs a log line saying
    /// the parse failed and that the default kicked in.
    #[test]
    #[tracing_test::traced_test]
    fn rate_limit_with_unparseable_resets_emits_warn() {
        let blob = "You've hit your limit · resets WHAT-IS-THIS-NONSENSE";
        match classify(blob) {
            FailureReason::RateLimit { resets_at: None } => {}
            other => panic!("expected RateLimit with no timestamp, got {other:?}"),
        }
        assert!(logs_contain(
            "rate-limit detected but reset_at parse failed"
        ));
        assert!(logs_contain("default_backoff_secs"));
        assert!(logs_contain("WHAT-IS-THIS-NONSENSE"));
    }

    /// #185 medium regression: the `rate_limit_exceeded` API-error
    /// phrasing has no `resets ` marker; the warn must still fire and
    /// include a useful tail excerpt.
    #[test]
    #[tracing_test::traced_test]
    fn rate_limit_without_resets_marker_emits_warn_with_tail_excerpt() {
        let blob = "...some output...\nrate_limit_exceeded (no timestamp here)";
        let _ = classify(blob);
        assert!(logs_contain(
            "rate-limit detected but reset_at parse failed"
        ));
        assert!(logs_contain("no `resets ` marker"));
    }

    #[test]
    fn reset_context_extracts_resets_clause() {
        let blob = "blah blah · resets Apr 23, 3pm · more text\nignore-this";
        let ctx = reset_context(blob);
        assert!(ctx.starts_with("resets "));
        assert!(ctx.contains("Apr 23, 3pm"));
        assert!(!ctx.contains("more text"));
    }

    #[test]
    fn reset_context_falls_back_to_tail_when_marker_absent() {
        let blob = "long log... rate_limit_exceeded";
        let ctx = reset_context(blob);
        assert!(ctx.contains("no `resets ` marker"));
        assert!(ctx.contains("rate_limit_exceeded"));
    }

    // ── enrich_with_resume_hint (#184) ──────────────────────────────────

    #[test]
    fn enrich_with_resume_hint_augments_unknown() {
        let r = FailureReason::Unknown {
            message: "exit 1; <subprocess wrote nothing useful>".into(),
        };
        let r = enrich_with_resume_hint(r, "abc12345-deadbeef-cafe-1234-567890abcdef");
        match r {
            FailureReason::Unknown { message } => {
                assert!(
                    message.contains("--resume abc12345"),
                    "should mention truncated session id; got: {message}"
                );
                assert!(
                    !message.contains("deadbeef"),
                    "should NOT leak full session id; got: {message}"
                );
                assert!(
                    message.contains("Re-run without --resume"),
                    "should give actionable guidance; got: {message}"
                );
                assert!(
                    message.contains("Original excerpt:"),
                    "should preserve the original message for triage; got: {message}"
                );
                assert!(
                    message.contains("subprocess wrote nothing useful"),
                    "original excerpt must survive enrichment; got: {message}"
                );
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn enrich_with_resume_hint_passes_through_auth_failure() {
        // Auth failures have an authoritative explanation; the resume
        // hint would be a distraction. Pass through unchanged.
        let r = FailureReason::AuthFailure;
        assert_eq!(
            enrich_with_resume_hint(r, "sess_123"),
            FailureReason::AuthFailure
        );
    }

    #[test]
    fn enrich_with_resume_hint_passes_through_rate_limit() {
        let r = FailureReason::RateLimit { resets_at: None };
        assert!(matches!(
            enrich_with_resume_hint(r, "sess_123"),
            FailureReason::RateLimit { resets_at: None }
        ));
    }

    #[test]
    fn enrich_with_resume_hint_passes_through_network_error() {
        let r = FailureReason::NetworkError {
            message: "connection refused".into(),
        };
        match enrich_with_resume_hint(r, "sess_123") {
            FailureReason::NetworkError { message } => {
                assert_eq!(message, "connection refused");
            }
            other => panic!("expected unchanged NetworkError, got {other:?}"),
        }
    }

    // ── #185: schema-first stream-JSON event classification ────────────

    /// Schema-first path matches the `result.error.type` envelope used
    /// by claude SDK streaming responses. Pre-#185 the classifier only
    /// looked for `error.type` at the top level and missed this shape.
    #[test]
    fn json_result_envelope_rate_limit_classifies() {
        let blob =
            r#"{"type":"result","result":{"error":{"type":"rate_limit_exceeded","message":""}}}"#;
        assert!(matches!(
            classify(blob),
            FailureReason::RateLimit { resets_at: None }
        ));
    }

    /// Schema-first path picks up `overloaded_error` (Anthropic's API
    /// emits this when 529-throttled at the platform layer rather than
    /// hitting a per-key rate limit). Both should classify as
    /// `RateLimit` so the spawn gate applies the standard backoff.
    #[test]
    fn json_overloaded_error_classifies_as_rate_limit() {
        let blob = r#"{"error":{"type":"overloaded_error","message":"please retry later"}}"#;
        assert!(matches!(
            classify(blob),
            FailureReason::RateLimit { resets_at: None }
        ));
    }

    /// When the JSON event also carries a parseable `resets …` hint in
    /// `error.message`, the schema path extracts it without needing the
    /// substring fallback.
    #[test]
    fn json_rate_limit_with_resets_in_message_parses_timestamp() {
        let blob = r#"{"error":{"type":"rate_limit_exceeded","message":"resets May 5, 9:45am"}}"#;
        match classify(blob) {
            FailureReason::RateLimit {
                resets_at: Some(ts),
            } => {
                use chrono::{Datelike, Timelike};
                assert_eq!(ts.month(), 5);
                assert_eq!(ts.day(), 5);
                assert_eq!(ts.hour(), 9);
                assert_eq!(ts.minute(), 45);
            }
            other => panic!("expected RateLimit with timestamp, got {other:?}"),
        }
    }

    /// Mixed log: streaming-JSON `error` event interleaved with prose
    /// stdout from earlier tool calls. The schema path must find the
    /// JSON envelope without being thrown off by the surrounding noise.
    #[test]
    fn schema_classifies_amid_mixed_stdout_lines() {
        let blob = "tool call output\n\
                    more text\n\
                    {\"error\":{\"type\":\"authentication_error\",\"message\":\"invalid_api_key\"}}\n\
                    trailing line";
        assert!(matches!(classify(blob), FailureReason::AuthFailure));
    }

    /// Auth-vs-rate-limit precedence holds when one comes from the JSON
    /// envelope and the other from the CLI banner: the substring auth
    /// check guards the schema path so a rate-limit JSON event with a
    /// co-occurring auth banner still terminates the run promptly.
    #[test]
    fn schema_rate_limit_with_substring_auth_banner_classifies_as_auth() {
        let blob = "{\"error\":{\"type\":\"rate_limit_exceeded\"}}\n\
             401 Unauthorized: authentication_error";
        assert!(matches!(classify(blob), FailureReason::AuthFailure));
    }

    /// JSON `invalid_request_error` with a `prompt is too long` body
    /// must classify as `ContextExceeded` even though the API uses the
    /// same `error.type` for both. Mirrors the substring fallback's
    /// disambiguation rule.
    #[test]
    fn json_invalid_request_with_prompt_too_long_classifies_as_context_exceeded() {
        let blob = r#"{"error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens"}}"#;
        assert!(matches!(classify(blob), FailureReason::ContextExceeded));
    }

    /// Unknown `error.type` strings fall through to the substring path.
    /// A future Anthropic API change adding a new error variant must
    /// not be silently misclassified — the substring path either
    /// recognizes a marker (network/auth/etc.) or returns Unknown.
    #[test]
    fn json_unknown_error_type_falls_through_to_substring_or_unknown() {
        let blob = r#"{"error":{"type":"some_future_error_type","message":"new failure mode"}}"#;
        match classify(blob) {
            FailureReason::Unknown { message } => {
                // Excerpt should preserve enough context for triage.
                assert!(
                    message.contains("some_future_error_type") || message.contains("new failure"),
                    "got: {message}"
                );
            }
            other => panic!("expected Unknown for unrecognized error.type, got {other:?}"),
        }
    }

    /// Non-JSON blob doesn't trigger the schema path — substring
    /// matchers continue to handle CLI banner output.
    #[test]
    fn cli_banner_text_falls_through_to_substring_matcher() {
        let blob = "You've hit your limit · resets Apr 23, 3pm";
        match classify(blob) {
            FailureReason::RateLimit { resets_at: Some(_) } => {}
            other => panic!("expected RateLimit with timestamp, got {other:?}"),
        }
    }

    #[test]
    fn enrich_with_resume_hint_handles_short_session_id() {
        // Shorter than RESUME_HINT_SESSION_PREFIX_CHARS — take the whole id.
        let r = FailureReason::Unknown {
            message: "x".into(),
        };
        let r = enrich_with_resume_hint(r, "abc");
        match r {
            FailureReason::Unknown { message } => {
                assert!(message.contains("--resume abc…"), "got: {message}");
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }
}
