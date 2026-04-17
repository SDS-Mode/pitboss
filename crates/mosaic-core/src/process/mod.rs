//! Process spawn abstraction. Real impl uses tokio; tests inject fakes.

pub mod spawner;
pub mod tokio_impl;

#[cfg(feature = "test-support")]
pub mod fake;

pub use spawner::{ChildProcess, ProcessSpawner, SpawnCmd};
pub use tokio_impl::TokioSpawner;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn spawn_cmd_is_constructible() {
        let cmd = SpawnCmd {
            program: PathBuf::from("claude"),
            args: vec!["--help".into()],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
        };
        assert_eq!(cmd.program, PathBuf::from("claude"));
    }
}

#[cfg(all(test, feature = "test-support"))]
mod fake_tests {
    use super::*;
    use crate::process::fake::{FakeScript, FakeSpawner};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn fake_spawner_emits_scripted_stdout() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init"}"#)
            .stdout_line(
                r#"{"type":"result","session_id":"s1","usage":{"input_tokens":1,"output_tokens":2}}"#,
            )
            .exit_code(0);
        let spawner = FakeSpawner::new(script);
        let cmd = SpawnCmd {
            program: PathBuf::from("claude"),
            args: vec![],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
        };
        let mut child = spawner.spawn(cmd).await.expect("spawn ok");
        let mut stdout = child.take_stdout().unwrap();
        let mut buf = String::new();
        tokio::time::timeout(Duration::from_secs(2), stdout.read_to_string(&mut buf))
            .await
            .expect("read completes")
            .unwrap();
        let status = child.wait().await.unwrap();
        assert!(buf.contains("system"));
        assert!(buf.contains("result"));
        assert!(status.success());
    }

    #[tokio::test]
    async fn fake_spawner_reports_nonzero_exit() {
        let script = FakeScript::new().stdout_line("oops").exit_code(42);
        let mut child = FakeSpawner::new(script)
            .spawn(SpawnCmd {
                program: PathBuf::from("x"),
                args: vec![],
                cwd: PathBuf::from("/tmp"),
                env: HashMap::new(),
            })
            .await
            .expect("spawn ok");
        let _ = child.take_stdout();
        let status = child.wait().await.unwrap();
        assert_eq!(status.code(), Some(42));
    }
}

#[cfg(test)]
mod real_tests {
    use super::*;
    use crate::process::tokio_impl::TokioSpawner;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn tokio_spawner_runs_echo_and_captures_stdout() {
        let spawner = TokioSpawner::new();
        let cmd = SpawnCmd {
            program: PathBuf::from("sh"),
            args: vec!["-c".into(), "printf 'hello\\n'".into()],
            cwd: std::env::temp_dir(),
            env: HashMap::new(),
        };
        let mut child = spawner.spawn(cmd).await.expect("spawn ok");
        let mut stdout = child.take_stdout().expect("stdout present");
        let mut buf = String::new();
        stdout.read_to_string(&mut buf).await.expect("read ok");
        let status = child.wait().await.expect("wait ok");
        assert_eq!(buf.trim(), "hello");
        assert!(status.success());
    }

    #[tokio::test]
    async fn tokio_spawner_reports_binary_not_found() {
        let spawner = TokioSpawner::new();
        let cmd = SpawnCmd {
            program: PathBuf::from("/definitely/not/a/binary/xyz"),
            args: vec![],
            cwd: std::env::temp_dir(),
            env: HashMap::new(),
        };
        let err = spawner.spawn(cmd).await.err().expect("spawn should fail");
        match err {
            crate::error::SpawnError::BinaryNotFound { .. } | crate::error::SpawnError::Io(_) => {}
            crate::error::SpawnError::Rejected { .. } => {
                panic!("unexpected Rejected variant")
            }
        }
    }
}
