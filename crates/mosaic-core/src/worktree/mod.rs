//! Git worktree lifecycle for task isolation.

pub mod manager;

pub use manager::{CleanupPolicy, Worktree, WorktreeManager};

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo(root: &std::path::Path) {
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@t.x"])
            .current_dir(root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(root)
            .status()
            .unwrap();
        std::fs::write(root.join("README.md"), "x").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(root)
            .status()
            .unwrap();
    }

    #[test]
    fn prepare_detached_worktree_without_branch() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());

        let mgr = WorktreeManager::new();
        let wt = mgr
            .prepare(repo_dir.path(), "shire-task-test-1", None)
            .unwrap();
        assert!(wt.path.exists(), "worktree path exists");
        assert!(wt.path.join("README.md").exists(), "checkout present");
        assert_eq!(wt.branch, None);
    }
}
