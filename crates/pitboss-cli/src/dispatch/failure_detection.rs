//! Post-exit classification of claude-subprocess failures.
//!
//! When a claude worker/lead exits non-zero, we don't just want to know *that*
//! it failed — callers (parent leads, the TUI, the spawn gater) need to know
//! *why* so they can react appropriately: back off on rate-limit, retry on
//! transient network, fail-fast on auth. Exit code alone is 1 for all of
//! these; the distinguishing signal lives in the last few KB of stdout/stderr.
//!
//! This module reads the tail of those logs and maps known markers to
//! [`FailureReason`] variants. The strategy is *conservative*:
//!
//! * Exit code 0 never produces a reason — a successful response that happens
//!   to mention "rate limit" in prose is not a failure. Callers must gate on
//!   a non-zero exit before invoking [`detect_failure_reason`].
//! * Markers come from observed claude CLI output, not guesses. An unknown
//!   non-zero exit becomes [`FailureReason::Unknown`] carrying a short log
//!   excerpt rather than being misclassified.
//! * Read only the tail (default 8 KiB) — rate-limit and error markers are
//!   always at the end of a streamed session. Scanning full logs would hurt
//!   at scale without changing the classification.

use std::path::Path;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use pitboss_core::store::FailureReason;
use tokio::sync::RwLock;

use crate::control::protocol::{ControlEvent, EventEnvelope};
use crate::dispatch::actor::ActorPath;
use crate::dispatch::layer::LayerState;

/// Minimum back-off after a `RateLimit` failure when the CLI didn't emit a
/// parseable `resets_at` timestamp. 5 minutes is long enough to cover most
/// transient burst-limit windows; callers fall through to the timestamp when
/// one was parsed.
const RATE_LIMIT_DEFAULT_BACKOFF_SECS: i64 = 300;

/// How long an `AuthFailure` is treated as fatal. Auth errors are almost
/// never transient — a bad API key stays bad — so we set this high enough
/// that the operator has time to notice and either kill the run or rotate
/// credentials. 10 minutes.
const AUTH_FAILURE_BACKOFF_SECS: i64 = 600;

/// Rolling per-run view of the Anthropic API's recent behavior, derived from
/// classified worker failures. Used by `handle_spawn_worker` /
/// `handle_spawn_sublead` to reject new spawns while a known-bad condition
/// persists (rate-limited, auth-broken) rather than burning budget on
/// subprocesses that will immediately fail with the same error.
///
/// Only `RateLimit` and `AuthFailure` populate state here. `NetworkError` is
/// intentionally *not* tracked — networks recover on their own and the
/// spawn retry is cheap; flagging network blips as a gate would cause
/// spurious refusals. `ContextExceeded`/`InvalidArgument`/`Unknown` are
/// per-task payload problems, not API health.
#[derive(Debug, Default)]
pub struct ApiHealth {
    rate_limit: RwLock<Option<RateLimitState>>,
    auth_failure: RwLock<Option<DateTime<Utc>>>,
}

#[derive(Debug, Clone, Copy)]
struct RateLimitState {
    hit_at: DateTime<Utc>,
    /// `None` when the CLI marker had no parseable timestamp — we fall back
    /// to `RATE_LIMIT_DEFAULT_BACKOFF_SECS` from `hit_at`.
    resets_at: Option<DateTime<Utc>>,
}

/// Why `ApiHealth::check_can_spawn` refused. Carries enough information for
/// the spawn handler to return a helpful error to the lead (so its Claude
/// session can plan around the outage rather than retrying immediately).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnGateReason {
    /// API is rate-limited. `retry_after` is best-effort: the parsed
    /// `resets_at` from the CLI when available, else a default-backoff
    /// projection from `hit_at`.
    RateLimited { retry_after: DateTime<Utc> },
    /// API auth failed recently. `clears_at` is a conservative 10-minute
    /// projection so repeated spawns don't hammer the API while the
    /// operator rotates credentials.
    AuthFailed { clears_at: DateTime<Utc> },
}

impl ApiHealth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update health state from a classified failure. No-op for reasons that
    /// don't affect spawn gating (`NetworkError`, `ContextExceeded`,
    /// `InvalidArgument`, `Unknown`). Safe to call from any path that
    /// received a `Some(FailureReason)`.
    pub async fn record(&self, reason: &FailureReason) {
        match reason {
            FailureReason::RateLimit { resets_at } => {
                *self.rate_limit.write().await = Some(RateLimitState {
                    hit_at: Utc::now(),
                    resets_at: *resets_at,
                });
            }
            FailureReason::AuthFailure => {
                *self.auth_failure.write().await = Some(Utc::now());
            }
            FailureReason::NetworkError { .. }
            | FailureReason::ContextExceeded
            | FailureReason::InvalidArgument { .. }
            | FailureReason::Unknown { .. } => {}
        }
    }

    /// Return `Err(SpawnGateReason)` when a new spawn should be refused,
    /// `Ok(())` otherwise. Checks the most severe gate first (auth, then
    /// rate-limit) so the returned reason is the most actionable.
    pub async fn check_can_spawn(&self) -> Result<(), SpawnGateReason> {
        if let Some(hit_at) = *self.auth_failure.read().await {
            let clears_at = hit_at + chrono::Duration::seconds(AUTH_FAILURE_BACKOFF_SECS);
            if Utc::now() < clears_at {
                return Err(SpawnGateReason::AuthFailed { clears_at });
            }
        }
        if let Some(state) = *self.rate_limit.read().await {
            let retry_after = state.resets_at.unwrap_or_else(|| {
                state.hit_at + chrono::Duration::seconds(RATE_LIMIT_DEFAULT_BACKOFF_SECS)
            });
            if Utc::now() < retry_after {
                return Err(SpawnGateReason::RateLimited { retry_after });
            }
        }
        Ok(())
    }
}

/// How many bytes from the end of stdout+stderr to scan. Markers land at the
/// tail of a session, and claude's final error block is typically <1 KiB —
/// 8 KiB is generous without being wasteful.
const TAIL_BYTES: u64 = 8 * 1024;

/// Length cap on `message` excerpts embedded in `NetworkError`/`Unknown`
/// variants. Keeps `TaskRecord`s compact in storage; full context is still
/// in the log file for anyone who needs it.
const EXCERPT_MAX_CHARS: usize = 240;

/// Inspect a completed subprocess and classify the failure. Returns `None`
/// only when the caller passed `exit_code == 0` — for any non-zero exit we
/// return at least [`FailureReason::Unknown`] so downstream code can always
/// distinguish "no failure" from "unclassified failure".
///
/// `stdout_path` and `stderr_path` may point at files that don't exist
/// (e.g., the process died before flushing); missing files are treated as
/// empty and do not cause an error.
pub fn detect_failure_reason(
    exit_code: Option<i32>,
    stdout_path: Option<&Path>,
    stderr_path: Option<&Path>,
) -> Option<FailureReason> {
    if exit_code == Some(0) {
        return None;
    }
    let mut buf = String::new();
    if let Some(p) = stdout_path {
        buf.push_str(&read_tail(p, TAIL_BYTES));
    }
    if let Some(p) = stderr_path {
        buf.push('\n');
        buf.push_str(&read_tail(p, TAIL_BYTES));
    }
    Some(classify(&buf))
}

/// Build a `WorkerFailed` control event envelope and broadcast it via the
/// root layer's control writer. No-op if no TUI is connected. Call this
/// alongside the `TaskRecord` persist in every worker/lead/sublead
/// completion path so downstream consumers (TUI, parent lead) see
/// classified failures without rescanning logs.
///
/// `actor_path_segments` builds the tree lineage — pass `[lead_id, task_id]`
/// for a lead-owned worker, `[root, sublead_id, task_id]` for a sublead-
/// owned worker, `[root]` alone for a root-lead failure. Empty paths are
/// elided on the wire for v0.5 client compat.
pub async fn broadcast_worker_failed(
    root_layer: &LayerState,
    task_id: String,
    parent_task_id: Option<String>,
    reason: FailureReason,
    actor_path_segments: &[&str],
) {
    let envelope = EventEnvelope {
        actor_path: ActorPath::new(actor_path_segments.iter().copied()),
        event: ControlEvent::WorkerFailed {
            task_id,
            parent_task_id,
            reason,
        },
    };
    root_layer.broadcast_control_event(envelope).await;
}

/// Public for unit tests — classify a pre-read log blob without touching the
/// filesystem.
pub fn classify(blob: &str) -> FailureReason {
    if let Some(reason) = match_rate_limit(blob) {
        return reason;
    }
    if let Some(reason) = match_auth(blob) {
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
    Some(FailureReason::RateLimit {
        resets_at: parse_reset_timestamp(blob),
    })
}

fn match_auth(blob: &str) -> Option<FailureReason> {
    if blob.contains("invalid_api_key")
        || blob.contains("authentication_error")
        || blob.contains("401")
            && (blob.contains("Unauthorized") || blob.contains("Authentication"))
    {
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

/// Read the last `max_bytes` of `path` as a lossy UTF-8 string. Missing files
/// return empty. Errors (permission, I/O) are swallowed — the log tail is
/// diagnostic-best-effort; we'd rather classify as Unknown than fail the whole
/// record write.
fn read_tail(path: &Path, max_bytes: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::with_capacity(max_bytes as usize);
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn excerpt(blob: &str) -> String {
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
/// alone is still enough to classify, and Phase 3 can apply a default
/// back-off when `resets_at` is missing.
fn parse_reset_timestamp(blob: &str) -> Option<DateTime<Utc>> {
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
    Utc.from_utc_datetime(&naive).into()
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
        (12, false) => 0,        // 12am = 00:00
        (12, true) => 12,        // 12pm = 12:00
        (h, false) => h,
        (h, true) => h + 12,
    };
    NaiveTime::from_hms_opt(hour24, minute, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn exit_zero_returns_none() {
        assert!(detect_failure_reason(Some(0), None, None).is_none());
    }

    #[test]
    fn non_zero_with_no_logs_is_unknown() {
        let r = detect_failure_reason(Some(1), None, None).unwrap();
        assert!(matches!(r, FailureReason::Unknown { .. }));
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
            FailureReason::RateLimit { resets_at: Some(ts) } => {
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
            FailureReason::RateLimit { resets_at: Some(ts) } => {
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
        // Even when we can't parse the reset time, the marker alone should
        // produce a `RateLimit` — the kind is what callers gate on.
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
        let r = classify(blob);
        match r {
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
    fn invalid_request_without_context_marker_is_invalid_argument() {
        let blob = r#"{"error":{"type":"invalid_request_error","message":"unknown tool"}}"#;
        assert!(matches!(
            classify(blob),
            FailureReason::InvalidArgument { .. }
        ));
    }

    #[test]
    fn unclassified_non_zero_is_unknown_with_excerpt() {
        let blob = "some unrelated stderr about disk IO";
        match classify(blob) {
            FailureReason::Unknown { message } => assert!(message.contains("disk IO")),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn excerpt_caps_length() {
        let long = "x".repeat(1000);
        match classify(&long) {
            FailureReason::Unknown { message } => {
                assert!(message.chars().count() <= EXCERPT_MAX_CHARS);
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn read_tail_returns_last_bytes() {
        let mut f = NamedTempFile::new().unwrap();
        let payload = "A".repeat(10_000) + "MARKER";
        f.write_all(payload.as_bytes()).unwrap();
        let tail = read_tail(f.path(), 100);
        assert!(tail.ends_with("MARKER"));
        assert!(tail.len() <= 100);
    }

    #[tokio::test]
    async fn api_health_fresh_allows_spawn() {
        let h = ApiHealth::new();
        assert!(h.check_can_spawn().await.is_ok());
    }

    #[tokio::test]
    async fn api_health_records_rate_limit_and_refuses_spawn() {
        let h = ApiHealth::new();
        let future = Utc::now() + chrono::Duration::minutes(10);
        h.record(&FailureReason::RateLimit {
            resets_at: Some(future),
        })
        .await;
        match h.check_can_spawn().await {
            Err(SpawnGateReason::RateLimited { retry_after }) => {
                assert_eq!(retry_after, future);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn api_health_rate_limit_without_timestamp_uses_default_backoff() {
        let h = ApiHealth::new();
        h.record(&FailureReason::RateLimit { resets_at: None }).await;
        match h.check_can_spawn().await {
            Err(SpawnGateReason::RateLimited { retry_after }) => {
                let remaining = (retry_after - Utc::now()).num_seconds();
                assert!(remaining > 0, "retry_after should be in the future");
                assert!(
                    remaining <= RATE_LIMIT_DEFAULT_BACKOFF_SECS,
                    "retry_after should be within the default backoff window"
                );
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn api_health_past_rate_limit_clears() {
        let h = ApiHealth::new();
        let past = Utc::now() - chrono::Duration::minutes(10);
        h.record(&FailureReason::RateLimit {
            resets_at: Some(past),
        })
        .await;
        assert!(h.check_can_spawn().await.is_ok());
    }

    #[tokio::test]
    async fn api_health_auth_failure_refuses_spawn() {
        let h = ApiHealth::new();
        h.record(&FailureReason::AuthFailure).await;
        assert!(matches!(
            h.check_can_spawn().await,
            Err(SpawnGateReason::AuthFailed { .. })
        ));
    }

    #[tokio::test]
    async fn api_health_ignores_non_gate_variants() {
        let h = ApiHealth::new();
        h.record(&FailureReason::NetworkError {
            message: "ETIMEDOUT".into(),
        })
        .await;
        h.record(&FailureReason::ContextExceeded).await;
        h.record(&FailureReason::Unknown {
            message: "boom".into(),
        })
        .await;
        h.record(&FailureReason::InvalidArgument {
            message: "bad".into(),
        })
        .await;
        assert!(h.check_can_spawn().await.is_ok());
    }

    #[tokio::test]
    async fn api_health_auth_takes_precedence_over_rate_limit() {
        // Both gates active — auth is more actionable for the operator,
        // so its error should be reported first.
        let h = ApiHealth::new();
        h.record(&FailureReason::RateLimit { resets_at: None }).await;
        h.record(&FailureReason::AuthFailure).await;
        assert!(matches!(
            h.check_can_spawn().await,
            Err(SpawnGateReason::AuthFailed { .. })
        ));
    }

    #[tokio::test]
    async fn broadcast_worker_failed_delivers_event() {
        use crate::manifest::resolve::ResolvedManifest;
        use crate::manifest::schema::WorktreeCleanup;
        use crate::shared_store::SharedStore;
        use pitboss_core::process::fake::{FakeScript, FakeSpawner};
        use pitboss_core::session::CancelToken;
        use pitboss_core::store::JsonFileStore;
        use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use std::sync::Arc;

        let dir = tempfile::TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
        };
        let store: Arc<dyn pitboss_core::store::SessionStore> =
            Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let spawner: Arc<dyn pitboss_core::process::ProcessSpawner> =
            Arc::new(FakeSpawner::new(FakeScript::new()));
        let layer = LayerState::new(
            uuid::Uuid::now_v7(),
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("claude"),
            Arc::new(WorktreeManager::new()),
            CleanupPolicy::Never,
            dir.path().to_path_buf(),
            crate::dispatch::state::ApprovalPolicy::Block,
            None,
            Arc::new(SharedStore::new()),
            None,
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ControlEvent>();
        *layer.control_writer.lock().await = Some(tx);

        broadcast_worker_failed(
            &layer,
            "w-1".into(),
            Some("lead".into()),
            FailureReason::RateLimit { resets_at: None },
            &["root", "lead", "w-1"],
        )
        .await;

        let ev = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("event should arrive before timeout")
            .expect("channel should deliver one event");
        match ev {
            ControlEvent::WorkerFailed {
                task_id,
                parent_task_id,
                reason,
            } => {
                assert_eq!(task_id, "w-1");
                assert_eq!(parent_task_id.as_deref(), Some("lead"));
                assert!(matches!(reason, FailureReason::RateLimit { .. }));
            }
            other => panic!("expected WorkerFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn broadcast_without_control_writer_is_noop() {
        // With no TUI attached, broadcast must not panic or block.
        use crate::manifest::resolve::ResolvedManifest;
        use crate::manifest::schema::WorktreeCleanup;
        use crate::shared_store::SharedStore;
        use pitboss_core::process::fake::{FakeScript, FakeSpawner};
        use pitboss_core::session::CancelToken;
        use pitboss_core::store::JsonFileStore;
        use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use std::sync::Arc;

        let dir = tempfile::TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
        };
        let store: Arc<dyn pitboss_core::store::SessionStore> =
            Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let spawner: Arc<dyn pitboss_core::process::ProcessSpawner> =
            Arc::new(FakeSpawner::new(FakeScript::new()));
        let layer = LayerState::new(
            uuid::Uuid::now_v7(),
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("claude"),
            Arc::new(WorktreeManager::new()),
            CleanupPolicy::Never,
            dir.path().to_path_buf(),
            crate::dispatch::state::ApprovalPolicy::Block,
            None,
            Arc::new(SharedStore::new()),
            None,
        );
        // No control_writer installed.
        broadcast_worker_failed(
            &layer,
            "w-1".into(),
            None,
            FailureReason::AuthFailure,
            &["root", "w-1"],
        )
        .await;
    }

    #[test]
    fn detect_reads_stdout_and_stderr() {
        let mut out = NamedTempFile::new().unwrap();
        out.write_all(b"normal output").unwrap();
        let mut err = NamedTempFile::new().unwrap();
        err.write_all(b"Error: ETIMEDOUT connecting\n").unwrap();
        let r = detect_failure_reason(Some(1), Some(out.path()), Some(err.path())).unwrap();
        assert!(matches!(r, FailureReason::NetworkError { .. }));
    }
}
