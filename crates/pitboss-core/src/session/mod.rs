//! Session handle and cancellation machinery.

use std::time::Duration;

pub mod cancel;
pub mod handle;
pub mod outcome;
pub mod state;

pub use cancel::CancelToken;
pub use handle::SessionHandle;
pub use outcome::SessionOutcome;
pub use state::SessionState;

/// Default grace window between SIGTERM and SIGKILL during terminate-phase
/// cancellation. Builders can override per-`SessionHandle` via
/// `SessionHandle::with_terminate_grace`. The constant is the operational
/// default; tests and tight-loop scenarios typically pass a smaller value.
pub const TERMINATE_GRACE: Duration = Duration::from_secs(10);

/// Default ceiling on the post-exit stdout/stderr drain in
/// `run_to_completion`. Builders can override per-`SessionHandle` via
/// `SessionHandle::with_stream_drain_timeout`. After the child exits, the
/// stream-reader task drains buffered tail bytes; without this ceiling a
/// hung log writer could wedge the dispatcher indefinitely. 30 seconds is
/// generous for kernel-buffered stdout, tight for genuine wedges.
pub const DEFAULT_STREAM_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(all(test, feature = "test-support"))]
mod happy_path_tests {
    use super::*;
    use crate::process::fake::{FakeScript, FakeSpawner};
    use crate::process::{ProcessSpawner, SpawnCmd};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn cmd() -> SpawnCmd {
        SpawnCmd {
            program: PathBuf::from("claude"),
            args: vec![],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn completed_session_records_usage_and_session_id() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init"}"#)
            .stdout_line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}"#)
            .stdout_line(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"all done"}]}}"#)
            .stdout_line(r#"{"type":"result","subtype":"success","session_id":"sess_final","result":"complete","usage":{"input_tokens":10,"output_tokens":25,"cache_read_input_tokens":100,"cache_creation_input_tokens":3}}"#)
            .exit_code(0);

        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let handle = SessionHandle::new("t1", spawner, cmd());
        let outcome = handle
            .run_to_completion(CancelToken::new(), Duration::from_secs(30))
            .await;

        assert!(matches!(outcome.final_state, SessionState::Completed));
        assert_eq!(outcome.exit_code, Some(0));
        assert_eq!(outcome.claude_session_id.as_deref(), Some("sess_final"));
        assert_eq!(outcome.token_usage.input, 10);
        assert_eq!(outcome.token_usage.output, 25);
        assert_eq!(outcome.token_usage.cache_read, 100);
        assert_eq!(outcome.final_message_preview.as_deref(), Some("all done"));
        assert_eq!(outcome.final_message.as_deref(), Some("all done"));
    }

    #[tokio::test]
    async fn long_assistant_message_lands_untruncated_in_final_message() {
        // Regression for issue #124: the 200-char preview cap silently
        // truncated the only field downstream consumers had access to. The
        // session outcome must now expose both the truncated preview and the
        // untruncated text so consumers can pick the right one.
        let body = "abc ".repeat(200); // 800 chars — well past the preview cap
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{body}"}}]}}}}"#
        );
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init"}"#)
            .stdout_line(&line)
            .stdout_line(r#"{"type":"result","subtype":"success","session_id":"s","result":"ok","usage":{"input_tokens":0,"output_tokens":0}}"#)
            .exit_code(0);
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let outcome = SessionHandle::new("t-long", spawner, cmd())
            .run_to_completion(CancelToken::new(), Duration::from_secs(30))
            .await;

        let preview = outcome.final_message_preview.as_deref().unwrap();
        let full = outcome.final_message.as_deref().unwrap();
        assert!(preview.ends_with('…'), "preview should be truncated");
        assert_eq!(preview.chars().count(), 201, "200 chars + ellipsis");
        assert_eq!(full.chars().count(), body.chars().count());
        assert!(!full.contains('…'), "full message must not be truncated");
    }

    #[tokio::test]
    async fn goose_chunked_text_lands_as_one_final_message() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolRequest","toolCall":{"status":"success","value":{"name":"pitboss__list_workers","arguments":{}}}}]}}"#)
            .stdout_line(r#"{"type":"message","message":{"role":"user","content":[{"type":"toolResponse","toolResult":{"status":"success","value":{"content":[{"type":"text","text":"{\"workers\":[]}"}]}}}]}}"#)
            .stdout_line(r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"PITBOSS_GO"}]}}"#)
            .stdout_line(r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"OSE_MCP_SMOKE_OK"}]}}"#)
            .stdout_line(r#"{"type":"complete","total_tokens":42}"#)
            .exit_code(0);
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let outcome = SessionHandle::new("t-goose", spawner, cmd())
            .with_parse_dialect(crate::parser::ParseDialect::Goose)
            .run_to_completion(CancelToken::new(), Duration::from_secs(30))
            .await;

        assert!(matches!(outcome.final_state, SessionState::Completed));
        assert_eq!(outcome.token_usage.output, 42);
        assert_eq!(
            outcome.final_message.as_deref(),
            Some("PITBOSS_GOOSE_MCP_SMOKE_OK")
        );
        assert_eq!(
            outcome.final_message_preview.as_deref(),
            Some("PITBOSS_GOOSE_MCP_SMOKE_OK")
        );
    }

    #[tokio::test]
    async fn nonzero_exit_becomes_failed_state() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"result","session_id":"s","usage":{"input_tokens":0,"output_tokens":0}}"#)
            .exit_code(1);
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let outcome = SessionHandle::new("t2", spawner, cmd())
            .run_to_completion(CancelToken::new(), Duration::from_secs(5))
            .await;
        assert_eq!(outcome.exit_code, Some(1));
        assert!(matches!(outcome.final_state, SessionState::Failed { .. }));
    }
}

#[cfg(all(test, feature = "test-support"))]
mod cancel_tests {
    use super::*;
    use crate::process::fake::{FakeScript, FakeSpawner};
    use crate::process::{ProcessSpawner, SpawnCmd};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    fn cmd() -> SpawnCmd {
        SpawnCmd {
            program: PathBuf::from("claude"),
            args: vec![],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn terminate_produces_cancelled_state() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init"}"#)
            .hold_until_signal();

        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let handle_fut = tokio::spawn(async move {
            SessionHandle::new("t", spawner, cmd())
                .run_to_completion(c2, Duration::from_secs(60))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.terminate();
        let outcome = tokio::time::timeout(
            Duration::from_secs(TERMINATE_GRACE.as_secs() + 15),
            handle_fut,
        )
        .await
        .expect("finishes within grace")
        .unwrap();
        assert!(matches!(outcome.final_state, SessionState::Cancelled));
    }

    #[tokio::test]
    async fn timeout_produces_timed_out_state() {
        let script = FakeScript::new().hold_until_signal();
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let outcome = SessionHandle::new("t", spawner, cmd())
            .run_to_completion(CancelToken::new(), Duration::from_millis(100))
            .await;
        assert!(matches!(outcome.final_state, SessionState::TimedOut));
    }

    /// `with_terminate_grace` overrides the SIGTERM→SIGKILL window.
    /// With a 50 ms grace, a hold-until-signal child that ignores
    /// SIGTERM (`FakeSpawner` doesn't, but the assertion is on wall-clock
    /// bound: the whole flow must finish within 1 s, which is well
    /// under the 10 s default).
    #[tokio::test]
    async fn with_terminate_grace_overrides_default() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init"}"#)
            .hold_until_signal();
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let started = std::time::Instant::now();
        let handle_fut = tokio::spawn(async move {
            SessionHandle::new("t", spawner, cmd())
                .with_terminate_grace(Duration::from_millis(50))
                .run_to_completion(c2, Duration::from_secs(60))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.terminate();
        let outcome = tokio::time::timeout(Duration::from_secs(2), handle_fut)
            .await
            .expect("finishes well under default 10s grace + 1s kill")
            .unwrap();
        assert!(matches!(outcome.final_state, SessionState::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "with_terminate_grace(50ms) should bound the whole flow under 2s; \
             elapsed = {:?}",
            started.elapsed()
        );
    }

    /// `with_stream_drain_timeout` is wired through. Verified by setting
    /// a tiny drain ceiling and confirming the run still completes
    /// (no hangs) when stdout has nothing more to deliver. This is a
    /// smoke check — the real protection is against a hung log writer,
    /// which is hard to simulate without a hostile `FakeSpawner`.
    #[tokio::test]
    async fn with_stream_drain_timeout_is_threaded_through() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init"}"#)
            .stdout_line(
                r#"{"type":"result","duration_ms":1,"duration_api_ms":1,"is_error":false,"num_turns":1,"result":"done","session_id":"s","total_cost_usd":0.0,"usage":{"input_tokens":0,"output_tokens":0}}"#,
            )
            .exit_code(0);
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let outcome = SessionHandle::new("t", spawner, cmd())
            .with_stream_drain_timeout(Duration::from_millis(100))
            .run_to_completion(CancelToken::new(), Duration::from_secs(5))
            .await;
        assert!(matches!(outcome.final_state, SessionState::Completed));
    }
}

#[cfg(all(test, feature = "test-support"))]
mod spawn_fail_tests {
    use super::*;
    use crate::process::fake::{FakeScript, FakeSpawner};
    use crate::process::{ProcessSpawner, SpawnCmd};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn spawn_rejection_yields_spawnfailed() {
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(
            FakeScript::new().fail_spawn("no binary here"),
        ));
        let cmd = SpawnCmd {
            program: PathBuf::from("x"),
            args: vec![],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
        };
        let outcome = SessionHandle::new("t", spawner, cmd)
            .run_to_completion(CancelToken::new(), Duration::from_secs(5))
            .await;
        match outcome.final_state {
            SessionState::SpawnFailed { message } => {
                assert!(message.contains("no binary here"));
            }
            other => panic!("expected SpawnFailed, got {other:?}"),
        }
        assert!(outcome.exit_code.is_none());
    }
}
