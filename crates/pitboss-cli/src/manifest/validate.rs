#![allow(dead_code)]

use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Result};

use super::resolve::ResolvedManifest;

/// Run all v0.1 validations. Call after [`crate::manifest::resolve::resolve`].
pub fn validate(resolved: &ResolvedManifest) -> Result<()> {
    validate_inner(resolved, false)
}

/// Like `validate` but skips the directory-existence check.
/// Used by `container-dispatch` where task `directory` fields are
/// container-side paths that don't exist on the host.
pub fn validate_skip_dir_check(resolved: &ResolvedManifest) -> Result<()> {
    validate_inner(resolved, true)
}

fn validate_inner(resolved: &ResolvedManifest, skip_dir_check: bool) -> Result<()> {
    validate_mode(resolved)?;
    for cfg in &resolved.notifications {
        crate::notify::config::validate(cfg)?;
    }
    if resolved.lead.is_some() {
        validate_lead(resolved, skip_dir_check)?;
        validate_hierarchical_ranges(resolved)?;
        validate_sublead_defaults_adequate(resolved)?;
    } else {
        // Flat-mode validations unchanged.
        validate_ids(resolved)?;
        if !skip_dir_check {
            validate_directories(resolved)?;
        }
        validate_branch_conflicts(resolved)?;
        validate_ranges(resolved)?;
    }
    Ok(())
}

fn validate_mode(r: &ResolvedManifest) -> Result<()> {
    if !r.tasks.is_empty() && r.lead.is_some() {
        bail!("cannot combine [[task]] and [[lead]] in the same manifest");
    }
    if r.tasks.is_empty() && r.lead.is_none() {
        bail!("empty manifest: define either [[task]] entries or exactly one [[lead]]");
    }
    if r.lead.is_none() {
        if r.max_workers.is_some() {
            bail!("[run].max_workers is only valid with a [[lead]] section");
        }
        if r.budget_usd.is_some() {
            bail!("[run].budget_usd is only valid with a [[lead]] section");
        }
        if r.lead_timeout_secs.is_some() {
            bail!("[run].lead_timeout_secs is only valid with a [[lead]] section");
        }
    }
    Ok(())
}

fn validate_lead(r: &ResolvedManifest, skip_dir_check: bool) -> Result<()> {
    let lead = r.lead.as_ref().unwrap();

    // Path B permission routing is not yet stable. The required
    // --permission-prompt-tool CLI flag, correct wire protocol, and env-bypass
    // guard are being implemented in a follow-on branch. Reject it at validate
    // time so operators get a clear error rather than a silent stall.
    if matches!(
        lead.permission_routing,
        crate::manifest::schema::PermissionRouting::PathB
    ) {
        bail!(
            "`permission_routing = \"path_b\"` is not yet stable and will silently \
             stall on any permission-gated tool use. Use the default \
             `\"path_a\"` until the follow-on implementation lands. \
             Track progress at: https://github.com/SDS-Mode/pitboss/issues"
        );
    }

    if lead.id.is_empty() {
        bail!("lead id is required");
    }
    if !lead
        .id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("lead id '{}' contains invalid characters", lead.id);
    }
    if !skip_dir_check && !lead.directory.is_dir() {
        bail!(
            "lead directory does not exist: {}",
            lead.directory.display()
        );
    }
    if lead.use_worktree && !is_in_git_repo(&lead.directory) {
        bail!(
            "lead has use_worktree=true but directory is not a git work-tree: {}",
            lead.directory.display()
        );
    }
    if lead.timeout_secs == 0 {
        bail!("lead timeout_secs must be > 0");
    }
    Ok(())
}

/// If `[lead] allow_subleads = true`, the manifest must also either
/// supply both `sublead_defaults.budget_usd` + `sublead_defaults.max_workers`
/// (for Owned-envelope default), or set `sublead_defaults.read_down = true`
/// (for SharedPool default). Otherwise a runtime `spawn_sublead` call that
/// omits those fields will fail with "budget_usd required when read_down=false"
/// mid-dispatch — a failure class that should have been caught at validate
/// time since it's a pure function of manifest shape.
///
/// Rationale: the lead's Claude session can always override these per-spawn
/// by passing explicit `budget_usd` + `max_workers` arguments, but there's
/// no way at validate time to prove it will. Forcing a well-defined manifest
/// default means the worst case (lead calls spawn_sublead without explicit
/// resources) still resolves cleanly rather than blowing up at dispatch.
///
/// Common fixes:
/// - "share root's pool by default" — `[lead.sublead_defaults] read_down = true`
/// - "give each sub-lead a budget" — `[lead.sublead_defaults]` with both
///   `budget_usd` and `max_workers` set
pub fn validate_sublead_defaults_adequate(r: &ResolvedManifest) -> Result<()> {
    let Some(lead) = r.lead.as_ref() else {
        return Ok(());
    };
    if !lead.allow_subleads {
        return Ok(());
    }
    // Owned default: both budget_usd AND max_workers set on sublead_defaults.
    // SharedPool default: read_down = true (budget/workers irrelevant).
    // Either path resolves cleanly at spawn time even if the lead omits the
    // per-spawn args.
    let owned_default_ok = lead
        .sublead_defaults
        .as_ref()
        .is_some_and(|d| d.budget_usd.is_some() && d.max_workers.is_some());
    let shared_pool_default_ok = lead.sublead_defaults.as_ref().is_some_and(|d| d.read_down);
    if owned_default_ok || shared_pool_default_ok {
        return Ok(());
    }

    bail!(
        "`[lead] allow_subleads = true` requires `[lead.sublead_defaults]` to \
         specify either (a) both `budget_usd` and `max_workers` for an \
         Owned-envelope default, or (b) `read_down = true` for a SharedPool \
         default. Without this, any `spawn_sublead` call from the lead that \
         omits those fields will fail at dispatch time with \
         \"budget_usd required when read_down=false\". \
         Pick the one that matches your intent:\n  \
         [lead.sublead_defaults]\n  \
         read_down = true                  # share root's budget + worker pool\n\n\
         or\n\n  \
         [lead.sublead_defaults]\n  \
         budget_usd = 1.00                 # per-sub-lead cap\n  \
         max_workers = 4"
    );
}

fn validate_hierarchical_ranges(r: &ResolvedManifest) -> Result<()> {
    if let Some(mw) = r.max_workers {
        if mw == 0 || mw > 16 {
            bail!("max_workers must be between 1 and 16 inclusive");
        }
    }
    if let Some(b) = r.budget_usd {
        if b <= 0.0 {
            bail!("budget_usd must be > 0");
        }
    }
    if let Some(t) = r.lead_timeout_secs {
        if t == 0 {
            bail!("lead_timeout_secs must be > 0");
        }
    }
    if r.max_parallel == 0 {
        bail!("max_parallel must be > 0");
    }
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
            resume_session_id: None,
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
            lead: None,
            max_workers: None,
            budget_usd: None,
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        }
    }

    fn rm_with<F>(f: F) -> ResolvedManifest
    where
        F: FnOnce(&mut ResolvedManifest),
    {
        let d = with_tmp_repo(true);
        let mut m = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(rl("lead", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_timeout_secs: Some(600),
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };
        f(&mut m);
        m
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

    #[test]
    fn rejects_mixing_tasks_and_lead() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![rt("t1", d.path().to_path_buf(), false, None)],
            lead: Some(rl("lead", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: Some(600),
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };
        let err = validate(&r).unwrap_err().to_string();
        assert!(
            err.contains("cannot combine [[task]] and [[lead]]"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_max_workers_on_flat_manifest() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![rt("t1", d.path().to_path_buf(), false, None)],
            lead: None,
            max_workers: Some(4), // set without a lead → error
            budget_usd: None,
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("max_workers"), "got: {err}");
    }

    #[test]
    fn rejects_max_workers_out_of_range() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(rl("l", d.path().to_path_buf())),
            max_workers: Some(17),
            budget_usd: Some(1.0),
            lead_timeout_secs: Some(600),
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_non_positive_budget() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(rl("l", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(0.0),
            lead_timeout_secs: Some(600),
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };
        assert!(validate(&r).is_err());
    }

    /// Build a minimal hierarchical manifest with a lead in a real git repo,
    /// parameterized by the two knobs under test.
    fn hierarchical_with_subleads(
        allow_subleads: bool,
        sublead_defaults: Option<super::super::resolve::SubleadDefaults>,
    ) -> (TempDir, ResolvedManifest) {
        let d = with_tmp_repo(true);
        let mut lead = rl("l", d.path().to_path_buf());
        lead.allow_subleads = allow_subleads;
        lead.sublead_defaults = sublead_defaults;
        let r = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_timeout_secs: Some(600),
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };
        (d, r)
    }

    #[test]
    fn accepts_allow_subleads_false_without_sublead_defaults() {
        // No sub-leads → sublead_defaults is irrelevant.
        let (_d, r) = hierarchical_with_subleads(false, None);
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn rejects_allow_subleads_true_without_sublead_defaults() {
        // The regression this validation was added for: allow_subleads=true
        // with no defaults → any lead-initiated spawn_sublead that omits the
        // resource args fails at dispatch. Must be caught at validate time.
        let (_d, r) = hierarchical_with_subleads(true, None);
        let err = validate(&r).unwrap_err();
        assert!(
            err.to_string()
                .contains("allow_subleads = true` requires `[lead.sublead_defaults]`"),
            "expected explanatory error, got: {err}"
        );
    }

    #[test]
    fn rejects_allow_subleads_true_with_empty_sublead_defaults() {
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::SubleadDefaults {
                budget_usd: None,
                max_workers: None,
                lead_timeout_secs: None,
                read_down: false,
            }),
        );
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_allow_subleads_true_with_partial_owned_defaults() {
        // budget_usd but no max_workers — ambiguous; must reject.
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::SubleadDefaults {
                budget_usd: Some(1.0),
                max_workers: None,
                lead_timeout_secs: None,
                read_down: false,
            }),
        );
        assert!(validate(&r).is_err());
    }

    #[test]
    fn accepts_allow_subleads_true_with_complete_owned_defaults() {
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::SubleadDefaults {
                budget_usd: Some(2.0),
                max_workers: Some(4),
                lead_timeout_secs: Some(1800),
                read_down: false,
            }),
        );
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn accepts_allow_subleads_true_with_read_down() {
        // SharedPool fallback — budget/workers omitted intentionally.
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::SubleadDefaults {
                budget_usd: None,
                max_workers: None,
                lead_timeout_secs: None,
                read_down: true,
            }),
        );
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn accepts_allow_subleads_true_with_read_down_and_explicit_defaults() {
        // Redundant but benign: operator set both read_down and explicit
        // defaults. Either alone would satisfy the check; both together is
        // fine.
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::SubleadDefaults {
                budget_usd: Some(2.0),
                max_workers: Some(4),
                lead_timeout_secs: None,
                read_down: true,
            }),
        );
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn rejects_manifest_with_no_tasks_and_no_lead() {
        let r = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: None,
            budget_usd: None,
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("empty manifest"), "got: {err}");
    }

    // Helper for these tests.
    fn rl(id: &str, dir: PathBuf) -> super::super::resolve::ResolvedLead {
        super::super::resolve::ResolvedLead {
            id: id.into(),
            directory: dir,
            prompt: "p".into(),
            branch: None,
            model: "m".into(),
            effort: super::super::schema::Effort::High,
            tools: vec![],
            timeout_secs: 600,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
            permission_routing: Default::default(),
            allow_subleads: false,
            max_subleads: None,
            max_sublead_budget_usd: None,
            max_workers_across_tree: None,
            sublead_defaults: None,
        }
    }

    #[test]
    fn rejects_webhook_without_url() {
        let m = rm_with(|m: &mut ResolvedManifest| {
            m.notifications
                .push(crate::notify::config::NotificationConfig {
                    kind: crate::notify::config::SinkKind::Webhook,
                    url: None,
                    events: None,
                    severity_min: crate::notify::Severity::Info,
                });
        });
        let err = validate(&m).unwrap_err();
        assert!(err.to_string().contains("requires a non-empty 'url'"));
    }

    #[test]
    fn rejects_unknown_event() {
        let m = rm_with(|m: &mut ResolvedManifest| {
            m.notifications
                .push(crate::notify::config::NotificationConfig {
                    kind: crate::notify::config::SinkKind::Log,
                    url: None,
                    events: Some(vec!["hallucinate".into()]),
                    severity_min: crate::notify::Severity::Info,
                });
        });
        let err = validate(&m).unwrap_err();
        assert!(err.to_string().contains("unknown event"));
    }
}
