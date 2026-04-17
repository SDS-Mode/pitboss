use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)] // Some helpers may be unused in subset test builds
pub fn init_git_repo(dir: &Path) {
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "t@t.x"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(dir)
        .status()
        .unwrap();
    std::fs::write(dir.join("README.md"), "x").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args(["commit", "-q", "-m", "i"])
        .current_dir(dir)
        .status()
        .unwrap();
}

#[allow(dead_code)]
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[allow(dead_code)]
pub fn fake_claude_path() -> PathBuf {
    workspace_root().join("target/debug/fake-claude")
}

#[allow(dead_code)]
pub fn shire_binary() -> PathBuf {
    workspace_root().join("target/debug/shire")
}

#[allow(dead_code)]
pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/scripts")
        .join(name)
}
