use std::path::{Path, PathBuf};

use git2::{Repository, WorktreeAddOptions};

use crate::error::WorktreeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupPolicy {
    Always,
    OnSuccess,
    Never,
}

#[derive(Debug)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    name: String,
    repo_root: PathBuf,
}

impl Worktree {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }
}

pub struct WorktreeManager;

impl Default for WorktreeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WorktreeManager {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    pub fn prepare(
        &self,
        repo_root: &Path,
        name: &str,
        branch: Option<&str>,
    ) -> Result<Worktree, WorktreeError> {
        let repo = Repository::open(repo_root).map_err(|_| WorktreeError::NotInRepo {
            path: repo_root.display().to_string(),
        })?;

        let wt_path = sibling_path(repo_root, name);
        let branch_name = branch.map(str::to_string);

        if let Some(bname) = &branch_name {
            if repo.find_branch(bname, git2::BranchType::Local).is_err() {
                let head_commit = repo.head()?.peel_to_commit()?;
                repo.branch(bname, &head_commit, false)?;
            }
        }

        if let Some(bname) = &branch_name {
            // #188 M2: avoid `Repository::open(wt.path())` per worktree —
            // each open does a libgit2 discover() pass and dominates
            // `prepare` time on operators with many active worktrees.
            // Each worktree's HEAD is a plain text file at
            // `<repo>/.git/worktrees/<name>/HEAD` containing
            // `ref: refs/heads/<branch>`; read it directly. Falls back to
            // the Repository::open path only on read/parse failure so
            // detached-HEAD or symbolic-ref forms still classify.
            for wt_name in repo.worktrees()?.iter().flatten() {
                let wt = repo.find_worktree(wt_name)?;
                let checked_out = head_branch_for_worktree(repo_root, wt_name)
                    .or_else(|| head_branch_via_open(wt.path()));
                if checked_out.as_deref() == Some(bname.as_str()) {
                    return Err(WorktreeError::BranchConflict {
                        branch: bname.clone(),
                    });
                }
            }
        }

        let mut opts = WorktreeAddOptions::new();
        let reference_holder;
        if let Some(bname) = &branch_name {
            let rname = format!("refs/heads/{bname}");
            reference_holder = repo.find_reference(&rname)?;
            opts.reference(Some(&reference_holder));
        }

        let _wt = repo.worktree(name, &wt_path, Some(&opts))?;

        Ok(Worktree {
            path: wt_path,
            branch: branch_name,
            name: name.to_string(),
            repo_root: repo_root.to_path_buf(),
        })
    }

    #[allow(clippy::needless_pass_by_value)] // Worktree is consumed intentionally: taking ownership signals the caller has relinquished it.
    pub fn cleanup(
        &self,
        wt: Worktree,
        policy: CleanupPolicy,
        succeeded: bool,
    ) -> Result<(), WorktreeError> {
        let should_remove = match policy {
            CleanupPolicy::Always => true,
            CleanupPolicy::OnSuccess => succeeded,
            CleanupPolicy::Never => false,
        };
        if !should_remove {
            return Ok(());
        }

        let repo = Repository::open(&wt.repo_root)?;
        if let Ok(handle) = repo.find_worktree(&wt.name) {
            let admin_path = handle.path().to_path_buf();
            let mut opts = git2::WorktreePruneOptions::new();
            opts.valid(true).locked(true).working_tree(true);
            if let Err(err) = handle.prune(Some(&mut opts)) {
                // Prune failed mid-way: the `.git/worktrees/<name>/`
                // administrative entry may still exist. Surface the path
                // so operators can run `git worktree prune` manually —
                // leaving the entry behind causes libgit2 to report
                // false `BranchConflict` on subsequent reuse.
                tracing::warn!(
                    worktree = %wt.name,
                    admin_path = %admin_path.display(),
                    "git worktree prune failed: {err}; administrative entry \
                     may be stale — run `git worktree prune` to clean up",
                );
            }
        }
        if wt.path.exists() {
            std::fs::remove_dir_all(&wt.path)?;
        }
        Ok(())
    }
}

fn sibling_path(repo_root: &Path, name: &str) -> PathBuf {
    let parent = repo_root.parent().unwrap_or_else(|| Path::new("."));
    let base = repo_root
        .file_name()
        .map_or_else(|| "repo".to_string(), |s| s.to_string_lossy().to_string());
    parent.join(format!("{base}-{name}"))
}

/// Cheap path-cache lookup for a worktree's currently checked-out branch.
///
/// Reads the plain-text HEAD file at
/// `<repo_root>/.git/worktrees/<wt_name>/HEAD` directly instead of opening
/// a fresh libgit2 `Repository` per worktree (which dominates
/// `prepare` time on operators with many active worktrees — #188 M2).
///
/// Returns the branch shorthand on `ref: refs/heads/<branch>` form;
/// `None` for detached HEAD, missing files, or any other shape — the
/// caller's existing `Repository::open` path picks those up.
fn head_branch_for_worktree(repo_root: &Path, wt_name: &str) -> Option<String> {
    let head_file = repo_root
        .join(".git")
        .join("worktrees")
        .join(wt_name)
        .join("HEAD");
    let contents = std::fs::read_to_string(&head_file).ok()?;
    let line = contents.lines().next()?;
    let suffix = line.strip_prefix("ref: refs/heads/")?;
    Some(suffix.trim().to_string())
}

/// Fallback for the cache miss path: the older `Repository::open`-based
/// branch-lookup. Kept so the cheap-cache path can degrade gracefully
/// when the HEAD file is in an unexpected form (symref, alternate
/// repository layout, etc.) without changing `prepare`'s behavior.
fn head_branch_via_open(wt_path: &Path) -> Option<String> {
    let wt_repo = Repository::open(wt_path).ok()?;
    wt_repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(str::to_string))
}

#[cfg(test)]
mod head_cache_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// #188 M2 regression: the cheap-cache reader extracts the branch
    /// shorthand from a plain `ref: refs/heads/<branch>` HEAD file
    /// without going through libgit2.
    #[test]
    fn head_branch_for_worktree_reads_plain_text_head() {
        let tmp = TempDir::new().unwrap();
        let admin = tmp.path().join(".git").join("worktrees").join("wt-x");
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("HEAD"), "ref: refs/heads/feat-cool\n").unwrap();
        let got = head_branch_for_worktree(tmp.path(), "wt-x");
        assert_eq!(got.as_deref(), Some("feat-cool"));
    }

    /// Detached HEAD form should return None so the caller's fallback
    /// path picks it up via libgit2.
    #[test]
    fn head_branch_for_worktree_returns_none_for_detached_head() {
        let tmp = TempDir::new().unwrap();
        let admin = tmp.path().join(".git").join("worktrees").join("wt-y");
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("HEAD"), "deadbeefdeadbeefdeadbeefdeadbeef\n").unwrap();
        let got = head_branch_for_worktree(tmp.path(), "wt-y");
        assert!(got.is_none(), "detached HEAD should not match the prefix");
    }

    /// Missing HEAD file (worktree pruned mid-walk) returns None,
    /// not a panic.
    #[test]
    fn head_branch_for_worktree_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        let got = head_branch_for_worktree(tmp.path(), "no-such-worktree");
        assert!(got.is_none());
    }
}
