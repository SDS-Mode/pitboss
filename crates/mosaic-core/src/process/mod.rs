//! Process spawn abstraction. Real impl uses tokio; tests inject fakes.

pub mod spawner;

pub use spawner::{ChildProcess, ProcessSpawner, SpawnCmd};

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
