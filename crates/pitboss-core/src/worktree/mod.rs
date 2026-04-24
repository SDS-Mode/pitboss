//! Git worktree lifecycle for task isolation.

pub mod manager;

pub use manager::{CleanupPolicy, Worktree, WorktreeManager};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::WorktreeError;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo(root: &std::path::Path) {
        // Pin the initial branch name to `master` so these tests don't depend
        // on the runner's `init.defaultBranch` git config. libgit2's worktree
        // creation resolves HEAD → `refs/heads/master` when the manager is
        // invoked without an explicit branch; runners that default HEAD to
        // `main` (GitHub Actions post-Oct 2020) previously failed all 8
        // tests here with "reference 'refs/heads/master' not found".
        Command::new("git")
            .args(["init", "-q", "--initial-branch=master"])
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
        // Disable commit signing so the init commit succeeds regardless of
        // the developer's global git config (CI runners don't sign; some
        // local setups do and fail without a signing key available).
        Command::new("git")
            .args(["config", "commit.gpgsign", "false"])
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
            .prepare(repo_dir.path(), "pitboss-task-test-1", None)
            .unwrap();
        assert!(wt.path.exists(), "worktree path exists");
        assert!(wt.path.join("README.md").exists(), "checkout present");
        assert_eq!(wt.branch, None);
    }

    #[test]
    fn prepare_creates_new_branch_when_absent() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());

        let mgr = WorktreeManager::new();
        let wt = mgr
            .prepare(repo_dir.path(), "pitboss-task-new-branch", Some("feat/new"))
            .unwrap();
        assert_eq!(wt.branch.as_deref(), Some("feat/new"));

        let out = std::process::Command::new("git")
            .args(["branch", "--list", "feat/new"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("feat/new"));
    }

    #[test]
    fn prepare_checks_out_existing_branch() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());
        std::process::Command::new("git")
            .args(["branch", "existing"])
            .current_dir(repo_dir.path())
            .status()
            .unwrap();

        let mgr = WorktreeManager::new();
        let wt = mgr
            .prepare(repo_dir.path(), "pitboss-task-exist", Some("existing"))
            .unwrap();
        assert_eq!(wt.branch.as_deref(), Some("existing"));
    }

    #[test]
    fn prepare_rejects_branch_already_checked_out_in_another_worktree() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());

        let mgr = WorktreeManager::new();
        let _first = mgr
            .prepare(repo_dir.path(), "wt-a", Some("shared"))
            .unwrap();
        let err = mgr
            .prepare(repo_dir.path(), "wt-b", Some("shared"))
            .unwrap_err();
        assert!(matches!(err, WorktreeError::BranchConflict { .. }));
    }

    #[test]
    fn cleanup_always_removes_worktree_on_success() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());
        let mgr = WorktreeManager::new();
        let wt = mgr.prepare(repo_dir.path(), "wt-ca", None).unwrap();
        let path = wt.path.clone();
        mgr.cleanup(wt, CleanupPolicy::Always, true).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn cleanup_always_removes_worktree_on_failure() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());
        let mgr = WorktreeManager::new();
        let wt = mgr.prepare(repo_dir.path(), "wt-cf", None).unwrap();
        let path = wt.path.clone();
        mgr.cleanup(wt, CleanupPolicy::Always, false).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn cleanup_on_success_keeps_failed_worktree() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());
        let mgr = WorktreeManager::new();
        let wt = mgr.prepare(repo_dir.path(), "wt-os", None).unwrap();
        let path = wt.path.clone();
        mgr.cleanup(wt, CleanupPolicy::OnSuccess, false).unwrap();
        assert!(path.exists(), "failed worktree preserved for forensics");
    }

    #[test]
    fn cleanup_never_always_keeps() {
        let repo_dir = TempDir::new().unwrap();
        init_repo(repo_dir.path());
        let mgr = WorktreeManager::new();
        let wt = mgr.prepare(repo_dir.path(), "wt-nev", None).unwrap();
        let path = wt.path.clone();
        mgr.cleanup(wt, CleanupPolicy::Never, true).unwrap();
        assert!(path.exists());
    }
}
