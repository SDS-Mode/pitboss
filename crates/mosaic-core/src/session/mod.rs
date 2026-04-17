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

/// Grace window between SIGTERM and SIGKILL during terminate-phase cancellation.
pub const TERMINATE_GRACE: Duration = Duration::from_secs(10);

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
