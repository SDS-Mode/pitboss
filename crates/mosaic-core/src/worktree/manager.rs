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

    pub fn cleanup(
        &self,
        _wt: Worktree,
        _policy: CleanupPolicy,
        _succeeded: bool,
    ) -> Result<(), WorktreeError> {
        unimplemented!("cleanup — Task 21")
    }
}

fn sibling_path(repo_root: &Path, name: &str) -> PathBuf {
    let parent = repo_root.parent().unwrap_or_else(|| Path::new("."));
    let base = repo_root
        .file_name()
        .map_or_else(|| "repo".to_string(), |s| s.to_string_lossy().to_string());
    parent.join(format!("{base}-{name}"))
}
