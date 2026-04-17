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
            for wt_name in repo.worktrees()?.iter().flatten() {
                let wt = repo.find_worktree(wt_name)?;
                let wt_repo = Repository::open(wt.path())?;
                let checked_out = wt_repo
                    .head()
                    .ok()
                    .and_then(|h| h.shorthand().map(str::to_string));
                if checked_out.as_deref() == Some(bname.as_str()) {
                    return Err(WorktreeError::BranchConflict { branch: bname.clone() });
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
            CleanupPolicy::Always    => true,
            CleanupPolicy::OnSuccess => succeeded,
            CleanupPolicy::Never     => false,
        };
        if !should_remove { return Ok(()); }

        let repo = Repository::open(&wt.repo_root)?;
        if let Ok(handle) = repo.find_worktree(&wt.name) {
            let mut opts = git2::WorktreePruneOptions::new();
            opts.valid(true).locked(true).working_tree(true);
            let _ = handle.prune(Some(&mut opts));
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
