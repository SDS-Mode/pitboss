#![allow(dead_code)]

use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Result};

use super::resolve::ResolvedManifest;

/// Run all v0.1 validations. Call after [`crate::manifest::resolve::resolve`].
pub fn validate(resolved: &ResolvedManifest) -> Result<()> {
    validate_ids(resolved)?;
    validate_directories(resolved)?;
    validate_branch_conflicts(resolved)?;
    validate_ranges(resolved)?;
    Ok(())
}

fn validate_ids(r: &ResolvedManifest) -> Result<()> {
    let mut seen = HashSet::new();
    for t in &r.tasks {
        if !seen.insert(&t.id) {
            bail!("duplicate task id: {}", t.id);
        }
        if t.id.is_empty() {
            bail!("empty task id");
        }
        if !t
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            bail!(
                "task id '{}' contains invalid characters (allowed: a-zA-Z0-9_-)",
                t.id
            );
        }
    }
    Ok(())
}

fn validate_directories(r: &ResolvedManifest) -> Result<()> {
    for t in &r.tasks {
        if !t.directory.is_dir() {
            bail!(
                "task '{}' directory does not exist or is not a directory: {}",
                t.id,
                t.directory.display()
            );
        }
        if t.use_worktree && !is_in_git_repo(&t.directory) {
            bail!(
                "task '{}' has use_worktree=true but directory is not a git work-tree: {}",
                t.id,
                t.directory.display()
            );
        }
    }
    Ok(())
}

fn validate_branch_conflicts(r: &ResolvedManifest) -> Result<()> {
    let mut seen: HashSet<(std::path::PathBuf, String)> = HashSet::new();
    for t in &r.tasks {
        if !t.use_worktree {
            continue;
        }
        if let Some(b) = &t.branch {
            let canon = std::fs::canonicalize(&t.directory).unwrap_or_else(|_| t.directory.clone());
            if !seen.insert((canon, b.clone())) {
                bail!("two tasks target the same directory + branch '{}'", b);
            }
        }
    }
    Ok(())
}

fn validate_ranges(r: &ResolvedManifest) -> Result<()> {
    if r.max_parallel == 0 {
        bail!("max_parallel must be > 0");
    }
    for t in &r.tasks {
        if t.timeout_secs == 0 {
            bail!("task '{}': timeout_secs must be > 0", t.id);
        }
    }
    Ok(())
}

fn is_in_git_repo(path: &Path) -> bool {
    git2::Repository::discover(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::super::resolve::ResolvedTask;
    use super::super::schema::{Effort, WorktreeCleanup};
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    fn with_tmp_repo(use_git: bool) -> TempDir {
        let d = TempDir::new().unwrap();
        if use_git {
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(d.path())
                .status()
                .unwrap();
            Command::new("git")
                .args(["config", "user.email", "t@t.x"])
                .current_dir(d.path())
                .status()
                .unwrap();
            Command::new("git")
                .args(["config", "user.name", "t"])
                .current_dir(d.path())
                .status()
                .unwrap();
            std::fs::write(d.path().join("r"), "").unwrap();
            Command::new("git")
                .args(["add", "."])
                .current_dir(d.path())
                .status()
                .unwrap();
            Command::new("git")
                .args(["commit", "-q", "-m", "i"])
                .current_dir(d.path())
                .status()
                .unwrap();
        }
        d
    }

    fn rt(id: &str, dir: PathBuf, use_worktree: bool, branch: Option<&str>) -> ResolvedTask {
        ResolvedTask {
            id: id.into(),
            directory: dir,
            prompt: "p".into(),
            branch: branch.map(str::to_string),
            model: "m".into(),
            effort: Effort::High,
            tools: vec![],
            timeout_secs: 60,
            use_worktree,
            env: Default::default(),
        }
    }

    fn rm(tasks: Vec<ResolvedTask>) -> ResolvedManifest {
        ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks,
        }
    }

    #[test]
    fn rejects_duplicate_ids() {
        let d = with_tmp_repo(true);
        let r = rm(vec![
            rt("a", d.path().to_path_buf(), false, None),
            rt("a", d.path().to_path_buf(), false, None),
        ]);
        assert!(validate(&r).unwrap_err().to_string().contains("duplicate"));
    }

    #[test]
    fn rejects_missing_directory() {
        let r = rm(vec![rt("a", PathBuf::from("/no/such/path"), false, None)]);
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_non_git_directory_with_worktree_true() {
        let d = with_tmp_repo(false);
        let r = rm(vec![rt("a", d.path().to_path_buf(), true, Some("b"))]);
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("not a git"));
    }

    #[test]
    fn accepts_non_git_directory_with_worktree_false() {
        let d = with_tmp_repo(false);
        let r = rm(vec![rt("a", d.path().to_path_buf(), false, None)]);
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn rejects_branch_dir_duplicates() {
        let d = with_tmp_repo(true);
        let r = rm(vec![
            rt("a", d.path().to_path_buf(), true, Some("shared")),
            rt("b", d.path().to_path_buf(), true, Some("shared")),
        ]);
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_zero_max_parallel() {
        let d = with_tmp_repo(true);
        let mut r = rm(vec![rt("a", d.path().to_path_buf(), false, None)]);
        r.max_parallel = 0;
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_invalid_id_chars() {
        let d = with_tmp_repo(true);
        let r = rm(vec![rt("has spaces", d.path().to_path_buf(), false, None)]);
        assert!(validate(&r).is_err());
    }
}
