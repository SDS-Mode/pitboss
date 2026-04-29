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

#[must_use]
pub fn excerpt(blob: &str) -> String {
    let trimmed = blob.trim();
    if trimmed.chars().count() <= EXCERPT_MAX_CHARS {
        return trimmed.to_string();
    }
    // Take the LAST EXCERPT_MAX_CHARS chars — errors sit at the tail.
    let start = trimmed.chars().count().saturating_sub(EXCERPT_MAX_CHARS);
    trimmed.chars().skip(start).collect()
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
