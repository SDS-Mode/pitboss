//! Post-exit classification of agent subprocess failures.
//!
//! When an agent worker/lead exits non-zero, we don't just want to know *that*
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

use chrono::{DateTime, Utc};
use pitboss_core::failure_classify::classify;
use pitboss_core::provider::Provider;
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

/// Rolling per-run view of each provider API's recent behavior, derived from
/// classified worker failures. Used by `handle_spawn_worker` /
/// `handle_spawn_sublead` to reject new spawns for the affected provider while
/// a known-bad condition persists (rate-limited, auth-broken) rather than
/// burning budget on subprocesses that will immediately fail with the same
/// error.
///
/// Only `RateLimit` and `AuthFailure` populate state here. `NetworkError` is
/// intentionally *not* tracked — networks recover on their own and the
/// spawn retry is cheap; flagging network blips as a gate would cause
/// spurious refusals. `ContextExceeded`/`InvalidArgument`/`Unknown` are
/// per-task payload problems, not API health.
#[derive(Debug, Default)]
pub struct ApiHealth {
    providers: RwLock<std::collections::HashMap<Provider, ProviderHealth>>,
}

#[derive(Debug, Default)]
struct ProviderHealth {
    rate_limit: Option<RateLimitState>,
    auth_failure: Option<DateTime<Utc>>,
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

    /// Update Anthropic health state from a classified failure. Compatibility
    /// wrapper for the current hierarchical Claude path; new Goose provider
    /// paths should call [`Self::record_for_provider`].
    pub async fn record(&self, reason: &FailureReason) {
        self.record_for_provider(&Provider::Anthropic, reason).await;
    }

    /// Update provider health state from a classified failure. No-op for
    /// reasons that don't affect spawn gating (`NetworkError`,
    /// `ContextExceeded`, `InvalidArgument`, `Unknown`). Safe to call from any
    /// path that received a `Some(FailureReason)`.
    pub async fn record_for_provider(&self, provider: &Provider, reason: &FailureReason) {
        match reason {
            FailureReason::RateLimit { resets_at } => {
                let mut providers = self.providers.write().await;
                let bucket = providers.entry(provider.clone()).or_default();
                bucket.rate_limit = Some(RateLimitState {
                    hit_at: Utc::now(),
                    resets_at: *resets_at,
                });
            }
            FailureReason::AuthFailure => {
                let mut providers = self.providers.write().await;
                let bucket = providers.entry(provider.clone()).or_default();
                bucket.auth_failure = Some(Utc::now());
            }
            FailureReason::NetworkError { .. }
            | FailureReason::ContextExceeded
            | FailureReason::InvalidArgument { .. }
            | FailureReason::Unknown { .. } => {}
        }
    }

    /// Check Anthropic health. Compatibility wrapper for the current
    /// hierarchical Claude path; new Goose provider paths should call
    /// [`Self::check_can_spawn_for_provider`].
    pub async fn check_can_spawn(&self) -> Result<(), SpawnGateReason> {
        self.check_can_spawn_for_provider(&Provider::Anthropic)
            .await
    }

    /// Return `Err(SpawnGateReason)` when a new spawn for `provider` should be
    /// refused, `Ok(())` otherwise. Checks the most severe gate first (auth,
    /// then rate-limit) so the returned reason is the most actionable.
    pub async fn check_can_spawn_for_provider(
        &self,
        provider: &Provider,
    ) -> Result<(), SpawnGateReason> {
        let providers = self.providers.read().await;
        let Some(state) = providers.get(provider) else {
            return Ok(());
        };
        if let Some(hit_at) = state.auth_failure {
            let clears_at = hit_at + chrono::Duration::seconds(AUTH_FAILURE_BACKOFF_SECS);
            if Utc::now() < clears_at {
                return Err(SpawnGateReason::AuthFailed { clears_at });
            }
        }
        if let Some(state) = state.rate_limit {
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

    // ── classify() unit tests live in `pitboss_core::failure_classify`.
    // The remaining tests below cover the CLI-side wiring: `detect_
    // failure_reason` (file IO + classify), `read_tail`, `ApiHealth`,
    // and `broadcast_worker_failed`.

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
    async fn api_health_gates_are_provider_scoped() {
        let h = ApiHealth::new();
        h.record_for_provider(
            &Provider::OpenAi,
            &FailureReason::RateLimit { resets_at: None },
        )
        .await;

        assert!(h
            .check_can_spawn_for_provider(&Provider::Anthropic)
            .await
            .is_ok());
        assert!(matches!(
            h.check_can_spawn_for_provider(&Provider::OpenAi).await,
            Err(SpawnGateReason::RateLimited { .. })
        ));
    }

    #[tokio::test]
    async fn api_health_rate_limit_without_timestamp_uses_default_backoff() {
        let h = ApiHealth::new();
        h.record(&FailureReason::RateLimit { resets_at: None })
            .await;
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
        h.record(&FailureReason::RateLimit { resets_at: None })
            .await;
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
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: None,
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
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
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ControlEvent>(
            crate::dispatch::layer::CONTROL_EVENT_QUEUE_CAP,
        );
        *layer.control_writer.lock().await = Some(crate::dispatch::layer::ControlWriterSlot {
            id: uuid::Uuid::now_v7(),
            sender: tx,
        });

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
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: None,
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
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
