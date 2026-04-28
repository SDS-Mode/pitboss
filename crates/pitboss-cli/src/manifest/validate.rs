#![allow(dead_code)]

use std::collections::HashSet;
use std::path::Path;

use anyhow::{anyhow, bail, Error, Result};

use super::resolve::ResolvedManifest;

/// Run all manifest validations. Call after [`crate::manifest::resolve::resolve`].
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
    validate_lifecycle(resolved)?;
    if resolved.lead.is_some() {
        validate_lead(resolved, skip_dir_check)?;
        validate_hierarchical_ranges(resolved)?;
        validate_sublead_defaults_adequate(resolved)?;
    } else {
        validate_ids(resolved)?;
        if !skip_dir_check {
            validate_directories(resolved)?;
        }
        validate_branch_conflicts(resolved)?;
        validate_ranges(resolved)?;
    }
    Ok(())
}

/// Enforce the `[lifecycle]` coupling rule (issue #133): a manifest that
/// declares `survive_parent = true` must declare at least one notification
/// target so the orchestrator can observe the run's outcome after losing
/// process-level control of it. Either an inline `[lifecycle].notify` or a
/// top-level `[[notification]]` section satisfies the rule.
///
/// Validates the inline notify spec (when present) using the same SSRF /
/// scheme rules as `[[notification]]`. The error explicitly points at the
/// fix so an operator using only `PITBOSS_PARENT_NOTIFY_URL` knows to add a
/// no-cost `kind = "log"` notification block.
fn validate_lifecycle(r: &ResolvedManifest) -> Result<()> {
    let Some(lifecycle) = &r.lifecycle else {
        return Ok(());
    };
    if let Some(notify) = &lifecycle.notify {
        crate::notify::config::validate(notify)?;
    }
    if lifecycle.survive_parent {
        let has_inline_notify = lifecycle.notify.is_some();
        let has_top_level_notify = !r.notifications.is_empty();
        if !has_inline_notify && !has_top_level_notify {
            bail!(
                "[lifecycle] survive_parent = true requires a notification target so the \
                 orchestrator can observe run completion. Add either:\n  \
                 - a [lifecycle.notify] inline sink (same shape as [[notification]]), or\n  \
                 - at least one top-level [[notification]] section.\n\n\
                 If you intend to deliver lifecycle events via the \
                 PITBOSS_PARENT_NOTIFY_URL env var, add a no-cost log block to satisfy \
                 this validate-time gate (the env-var sink is configured at dispatch-time \
                 and validate cannot see it):\n\n    \
                 [[notification]]\n    \
                 kind = \"log\"\n"
            );
        }
    }
    Ok(())
}

fn validate_mode(r: &ResolvedManifest) -> Result<()> {
    if !r.tasks.is_empty() && r.lead.is_some() {
        bail!("cannot combine [[task]] and [lead] in the same manifest");
    }
    if r.tasks.is_empty() && r.lead.is_none() {
        bail!("empty manifest: define either [[task]] entries or exactly one [lead]");
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

    // A lead with an empty prompt starts, receives no `-p` argument, and exits
    // with code 1 in ~700ms. Common cause: the `prompt =` key appears after a
    // subtable declaration in the TOML source, which moves it out of the
    // `[lead]` scope — TOML silently assigns it to the subtable or drops it.
    if lead.prompt.trim().is_empty() {
        bail!(
            "lead '{}': prompt is required but is empty. Ensure `prompt =` \
             appears before any subtable declaration (e.g. `[lead.env]`) in \
             the TOML source.",
            lead.id
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
    // skip_dir_check also gates the git-repo probe — container-dispatch
    // manifests use container-side paths (`/workspace/foo`) that don't
    // exist on the host, so `git2::Repository::discover` would always
    // fail and reject every hierarchical container manifest with the
    // default `use_worktree = true`. Mirrors the existing flat-mode
    // gating in `validate_directories`.
    if !skip_dir_check && lead.use_worktree && !is_in_git_repo(&lead.directory) {
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

/// If `[lead] allow_subleads = true`, the manifest must also either supply
/// both `sublead_defaults.budget_usd` + `sublead_defaults.max_workers` (for
/// Owned-envelope default), or set `sublead_defaults.read_down = true` (for
/// SharedPool default). Otherwise a runtime `spawn_sublead` call that omits
/// those fields will fail with "budget_usd required when read_down=false"
/// mid-dispatch.
///
/// Note: this is a *required-defaults* check, distinct from the depth-2
/// invariant proper (which lives in [`crate::dispatch::depth`]). It runs
/// only when sub-lead spawning is *enabled* by the manifest; the depth-2
/// caller/capability checks fire on every `spawn_sublead` invocation.
pub fn validate_sublead_defaults_adequate(r: &ResolvedManifest) -> Result<()> {
    let Some(lead) = r.lead.as_ref() else {
        return Ok(());
    };
    if !lead.allow_subleads {
        return Ok(());
    }
    let owned_default_ok = lead
        .sublead_defaults
        .as_ref()
        .is_some_and(|d| d.budget_usd.is_some() && d.max_workers.is_some());
    let shared_pool_default_ok = lead.sublead_defaults.as_ref().is_some_and(|d| d.read_down);
    if owned_default_ok || shared_pool_default_ok {
        return Ok(());
    }

    bail!(
        "`[lead] allow_subleads = true` requires `[sublead_defaults]` to \
         specify either (a) both `budget_usd` and `max_workers` for an \
         Owned-envelope default, or (b) `read_down = true` for a SharedPool \
         default. Without this, any `spawn_sublead` call from the lead that \
         omits those fields will fail at dispatch time with \
         \"budget_usd required when read_down=false\". \
         Pick the one that matches your intent:\n  \
         [sublead_defaults]\n  \
         read_down = true                  # share root's budget + worker pool\n\n\
         or\n\n  \
         [sublead_defaults]\n  \
         budget_usd = 1.00                 # per-sub-lead cap\n  \
         max_workers = 4"
    );
}

fn validate_hierarchical_ranges(r: &ResolvedManifest) -> Result<()> {
    if let Some(mw) = r.max_workers {
        if mw == 0 || mw > 16 {
            bail!("[lead].max_workers must be between 1 and 16 inclusive");
        }
    }
    if let Some(b) = r.budget_usd {
        if b <= 0.0 {
            bail!("[lead].budget_usd must be > 0");
        }
    }
    if let Some(t) = r.lead_timeout_secs {
        if t == 0 {
            bail!("[lead].lead_timeout_secs must be > 0");
        }
    }
    // No `max_parallel_tasks == 0` check here: that's a flat-mode
    // ([[task]]) concurrency cap. In hierarchical mode the lead's own
    // `[lead].max_workers` governs concurrency, and the resolved
    // ResolvedManifest.max_parallel_tasks always has the resolver
    // default (DEFAULT_MAX_PARALLEL_TASKS) applied, so an `== 0`
    // assertion here would never fire. The legitimate range check
    // lives in validate_ranges (flat-mode path) instead.
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
    if r.max_parallel_tasks == 0 {
        bail!("[run].max_parallel_tasks must be > 0");
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

/// Inspect a TOML parse error against the manifest source text and, if it
/// matches a known v0.8→v0.9 migration footprint, return a tailored error
/// with explicit migration guidance. Returns `None` if no legacy pattern is
/// detected (the original parse error stands).
///
/// This is a pure-text scan over the source — it doesn't try to fully parse
/// the file (which already failed). It looks for the table/field markers that
/// changed in v0.9 and emits one error per matched legacy pattern.
pub fn translate_legacy_parse_error(parse_err: &toml::de::Error, src: &str) -> Option<Error> {
    let mut migrations: Vec<String> = Vec::new();

    // [[lead]] array form removed in v0.9.
    if has_top_level_array_table(src, "lead") {
        migrations.push(
            "  • `[[lead]]` (array form) was removed in v0.9. Use `[lead]` \
             (single-table) instead. Pitboss only ever supported one root \
             lead, so the array form was removable without losing capability."
                .to_string(),
        );
    }

    // [run] no longer carries the lead caps.
    let run_field_migrations = [
        ("max_workers", "[lead].max_workers"),
        ("budget_usd", "[lead].budget_usd"),
        ("lead_timeout_secs", "[lead].lead_timeout_secs"),
    ];
    for (old, new) in run_field_migrations {
        if has_field_in_section(src, "run", old) {
            migrations.push(format!(
                "  • `[run].{old}` moved to `{new}` in v0.9 — these are \
                 lead-level caps, not run-wide settings."
            ));
        }
    }

    // [run].max_parallel renamed to [run].max_parallel_tasks.
    if has_field_in_section(src, "run", "max_parallel")
        && !has_field_in_section(src, "run", "max_parallel_tasks")
    {
        migrations.push(
            "  • `[run].max_parallel` renamed to `[run].max_parallel_tasks` \
             in v0.9 (clarifies it's the flat-mode task concurrency cap)."
                .to_string(),
        );
    }

    // [run].approval_policy renamed to [run].default_approval_policy.
    if has_field_in_section(src, "run", "approval_policy")
        && !has_field_in_section(src, "run", "default_approval_policy")
    {
        migrations.push(
            "  • `[run].approval_policy` renamed to \
             `[run].default_approval_policy` in v0.9 (disambiguates from the \
             `[[approval_policy]]` rule array)."
                .to_string(),
        );
    }

    // [lead].max_workers_across_tree renamed to [lead].max_total_workers.
    if has_field_in_section(src, "lead", "max_workers_across_tree") {
        migrations.push(
            "  • `[lead].max_workers_across_tree` renamed to \
             `[lead].max_total_workers` in v0.9 (shorter and clearer)."
                .to_string(),
        );
    }

    // [lead.sublead_defaults] promoted to top-level [sublead_defaults].
    if has_subtable(src, "lead", "sublead_defaults") {
        migrations.push(
            "  • `[lead.sublead_defaults]` promoted to top-level \
             `[sublead_defaults]` in v0.9 (form-friendly; eliminates the \
             prompt-ordering footgun where keys after the subtable header \
             silently moved scope)."
                .to_string(),
        );
    }

    if migrations.is_empty() {
        return None;
    }

    Some(anyhow!(
        "manifest uses pre-v0.9 schema. Migrate the following:\n{}\n\nOriginal parse error: {}",
        migrations.join("\n"),
        parse_err
    ))
}

/// Returns true iff the source contains a `[[<name>]]` array-of-tables marker
/// (with optional whitespace) at the start of a line. Skips comment lines so
/// `# [[lead]] (legacy)` in a docstring doesn't false-positive.
fn has_top_level_array_table(src: &str, name: &str) -> bool {
    let needle = format!("[[{name}]]");
    src.lines().any(|line| {
        let trimmed = line.trim_start();
        !trimmed.starts_with('#') && trimmed.starts_with(&needle)
    })
}

/// Returns true iff the source contains a top-level `[<section>]` table
/// containing a `<field> =` assignment before the next `[` line. Comment
/// lines (`# …`) are skipped entirely so a commented-out legacy field
/// (`# approval_policy = "block"`) doesn't trigger a false migration hint
/// and `# [run]` doesn't mistakenly enter or leave the target section.
fn has_field_in_section(src: &str, section: &str, field: &str) -> bool {
    let header = format!("[{section}]");
    let mut in_section = false;
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') {
            // Entering a new table — true if we hit the target header,
            // false on any other table (including subtables of the section).
            in_section =
                trimmed.starts_with(&header) && trimmed.chars().nth(header.len()) != Some('.');
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix(field) {
            if rest.starts_with(' ') || rest.starts_with('=') || rest.starts_with('\t') {
                return true;
            }
        }
    }
    false
}

/// Returns true iff the source contains a `[<parent>.<child>]` subtable
/// header. Skips comment lines (see [`has_top_level_array_table`]).
fn has_subtable(src: &str, parent: &str, child: &str) -> bool {
    let needle = format!("[{parent}.{child}]");
    src.lines().any(|line| {
        let trimmed = line.trim_start();
        !trimmed.starts_with('#') && trimmed.starts_with(&needle)
    })
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
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks,
            lead: None,
            max_workers: None,
            budget_usd: None,
            lead_timeout_secs: None,
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
        }
    }

    fn rm_with<F>(f: F) -> ResolvedManifest
    where
        F: FnOnce(&mut ResolvedManifest),
    {
        let d = with_tmp_repo(true);
        let mut m = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(rl("lead", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
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
    fn rejects_zero_max_parallel_tasks() {
        let d = with_tmp_repo(true);
        let mut r = rm(vec![rt("a", d.path().to_path_buf(), false, None)]);
        r.max_parallel_tasks = 0;
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
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![rt("t1", d.path().to_path_buf(), false, None)],
            lead: Some(rl("lead", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
        };
        let err = validate(&r).unwrap_err().to_string();
        assert!(
            err.contains("cannot combine [[task]] and [lead]"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_max_workers_out_of_range() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(rl("l", d.path().to_path_buf())),
            max_workers: Some(17),
            budget_usd: Some(1.0),
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
        };
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_non_positive_budget() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(rl("l", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(0.0),
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
        };
        assert!(validate(&r).is_err());
    }

    fn hierarchical_with_subleads(
        allow_subleads: bool,
        sublead_defaults: Option<super::super::resolve::ResolvedSubleadDefaults>,
    ) -> (TempDir, ResolvedManifest) {
        let d = with_tmp_repo(true);
        let mut lead = rl("l", d.path().to_path_buf());
        lead.allow_subleads = allow_subleads;
        lead.sublead_defaults = sublead_defaults;
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
        };
        (d, r)
    }

    #[test]
    fn accepts_allow_subleads_false_without_sublead_defaults() {
        let (_d, r) = hierarchical_with_subleads(false, None);
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn rejects_allow_subleads_true_without_sublead_defaults() {
        let (_d, r) = hierarchical_with_subleads(true, None);
        let err = validate(&r).unwrap_err();
        assert!(
            err.to_string()
                .contains("allow_subleads = true` requires `[sublead_defaults]`"),
            "expected explanatory error, got: {err}"
        );
    }

    #[test]
    fn rejects_allow_subleads_true_with_empty_sublead_defaults() {
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::ResolvedSubleadDefaults {
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
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::ResolvedSubleadDefaults {
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
            Some(super::super::resolve::ResolvedSubleadDefaults {
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
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::ResolvedSubleadDefaults {
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
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::ResolvedSubleadDefaults {
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
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: None,
            budget_usd: None,
            lead_timeout_secs: None,
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
        };
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("empty manifest"), "got: {err}");
    }

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
            max_total_workers: None,
            sublead_defaults: None,
        }
    }

    #[test]
    fn rejects_lead_with_empty_prompt() {
        let d = with_tmp_repo(true);
        let mut lead = rl("l", d.path().to_path_buf());
        lead.prompt = String::new();
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
        };
        let err = validate(&r).unwrap_err().to_string();
        assert!(
            err.contains("prompt is required but is empty"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_lead_with_whitespace_only_prompt() {
        let d = with_tmp_repo(true);
        let mut lead = rl("l", d.path().to_path_buf());
        lead.prompt = "   \t\n  ".to_string();
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
        };
        assert!(validate(&r).is_err());
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
                    request_timeout_secs: None,
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
                    request_timeout_secs: None,
                });
        });
        let err = validate(&m).unwrap_err();
        assert!(err.to_string().contains("unknown event"));
    }

    // ── Migration translator tests ───────────────────────────────────────

    #[test]
    fn translator_flags_legacy_array_lead() {
        let src = "[[lead]]\nid = \"x\"\ndirectory = \"/tmp\"\nprompt = \"p\"\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        let err = translate_legacy_parse_error(&parse_err.unwrap_err(), src).unwrap();
        let s = err.to_string();
        assert!(s.contains("[[lead]]"), "got: {s}");
        assert!(s.contains("v0.9"), "got: {s}");
    }

    #[test]
    fn translator_flags_run_max_workers() {
        let src = "[run]\nmax_workers = 4\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        let err = translate_legacy_parse_error(&parse_err.unwrap_err(), src).unwrap();
        assert!(err.to_string().contains("[run].max_workers"));
        assert!(err.to_string().contains("[lead].max_workers"));
    }

    #[test]
    fn translator_flags_max_parallel_rename() {
        let src = "[run]\nmax_parallel = 4\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        let err = translate_legacy_parse_error(&parse_err.unwrap_err(), src).unwrap();
        assert!(err.to_string().contains("max_parallel_tasks"));
    }

    #[test]
    fn translator_flags_nested_sublead_defaults() {
        let src = "[lead.sublead_defaults]\nread_down = true\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        let err = translate_legacy_parse_error(&parse_err.unwrap_err(), src).unwrap();
        assert!(err.to_string().contains("[sublead_defaults]"));
    }

    #[test]
    fn translator_returns_none_for_unrelated_errors() {
        // A v0.9-shaped manifest with an unrelated error (unknown top-level
        // key) should not be hijacked by the migration translator.
        let src = "wibble = \"x\"\n[lead]\nid = \"a\"\ndirectory = \"/tmp\"\nprompt = \"p\"\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        let translated = translate_legacy_parse_error(&parse_err.unwrap_err(), src);
        assert!(translated.is_none());
    }

    fn log_notification() -> crate::notify::config::NotificationConfig {
        crate::notify::config::NotificationConfig {
            kind: crate::notify::config::SinkKind::Log,
            url: None,
            events: None,
            severity_min: crate::notify::Severity::Info,
            request_timeout_secs: None,
        }
    }

    #[test]
    fn lifecycle_survive_parent_with_top_level_notification_is_ok() {
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.notifications.push(log_notification());
        m.lifecycle = Some(crate::manifest::schema::Lifecycle {
            survive_parent: true,
            notify: None,
        });
        validate(&m).expect("top-level notification satisfies the coupling");
    }

    #[test]
    fn lifecycle_survive_parent_with_inline_notify_is_ok() {
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.lifecycle = Some(crate::manifest::schema::Lifecycle {
            survive_parent: true,
            notify: Some(log_notification()),
        });
        validate(&m).expect("inline lifecycle.notify satisfies the coupling");
    }

    #[test]
    fn lifecycle_survive_parent_without_any_notification_is_rejected() {
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.lifecycle = Some(crate::manifest::schema::Lifecycle {
            survive_parent: true,
            notify: None,
        });
        let err = validate(&m).expect_err("naked survive_parent must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("survive_parent = true"), "msg: {msg}");
        assert!(msg.contains("notification target"), "msg: {msg}");
        assert!(msg.contains("PITBOSS_PARENT_NOTIFY_URL"), "msg: {msg}");
    }

    #[test]
    fn lifecycle_survive_parent_false_does_not_require_notify() {
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.lifecycle = Some(crate::manifest::schema::Lifecycle {
            survive_parent: false,
            notify: None,
        });
        validate(&m).expect("survive_parent=false has no coupling requirement");
    }

    #[test]
    fn lifecycle_inline_notify_runs_through_ssrf_guard() {
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.lifecycle = Some(crate::manifest::schema::Lifecycle {
            survive_parent: true,
            notify: Some(crate::notify::config::NotificationConfig {
                kind: crate::notify::config::SinkKind::Webhook,
                url: Some("http://localhost/hook".to_string()),
                events: None,
                severity_min: crate::notify::Severity::Info,
                request_timeout_secs: None,
            }),
        });
        // The inline notify gets validated by the same logic as
        // [[notification]] — http:// + loopback should be refused. Operators
        // who need loopback should use PITBOSS_PARENT_NOTIFY_URL.
        let err = validate(&m).expect_err("inline notify must obey SSRF rules");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("https://") || msg.contains("loopback"),
            "msg: {msg}"
        );
    }

    #[test]
    fn lifecycle_omitted_section_is_unchanged_behavior() {
        let d = with_tmp_repo(true);
        let m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        validate(&m).expect("manifests without [lifecycle] continue to validate");
    }

    #[test]
    fn skip_dir_check_skips_git_repo_probe_for_lead() {
        // Regression for #142: container-dispatch hierarchical manifests
        // carry container-side directories (`/workspace/foo`) that don't
        // exist on the host, so the lead's git-repo probe must be gated
        // on `skip_dir_check` just like the directory-existence check.
        // Pre-fix this validate path would fail with
        // "directory is not a git work-tree" for every container manifest.
        let mut lead = rl("lead", PathBuf::from("/workspace/does-not-exist-on-host"));
        lead.use_worktree = true;
        let mut m = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
        };
        // Sanity: with skip_dir_check=false the validator rejects (the
        // host-side path is bogus).
        assert!(validate(&m).is_err());
        // The container-dispatch entry point must accept this manifest:
        // the probe is gated on skip_dir_check so a host-side absence
        // doesn't leak into validation.
        validate_skip_dir_check(&m)
            .expect("skip_dir_check must skip the git-repo probe for container-dispatch manifests");
        // Same when use_worktree is false — the gate covers both paths.
        m.lead.as_mut().unwrap().use_worktree = false;
        validate_skip_dir_check(&m).expect("skip_dir_check skips dir checks unconditionally");
    }

    #[test]
    fn translator_ignores_legacy_field_in_comment_lines() {
        // #155: a commented-out v0.8 field name in [run] must not trigger
        // a false-positive migration hint. Pre-fix the line scanner did
        // a prefix match without skipping `#` lines.
        let src = "# previously: max_workers = 4\n[run]\n# approval_policy = \"block\"\nmax_parallel_tasks = 2\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        // The [[run]] body is otherwise valid v0.9, so toml may or may
        // not error — only run the scanner if it did.
        if let Err(e) = parse_err {
            let translated = translate_legacy_parse_error(&e, src);
            assert!(
                translated.is_none(),
                "comment-only mentions of legacy fields must not be translated as migration hints: {translated:?}"
            );
        }
    }

    #[test]
    fn translator_scanner_skips_commented_section_brackets() {
        // `# [[lead]]` in a leading comment used to satisfy
        // has_top_level_array_table; now it must not.
        let src =
            "# legacy syntax: [[lead]]\n[lead]\nid = \"x\"\ndirectory = \"/tmp\"\nprompt = \"p\"\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        if let Err(e) = parse_err {
            let translated = translate_legacy_parse_error(&e, src);
            assert!(
                translated.is_none(),
                "commented-out [[lead]] must not be flagged as legacy: {translated:?}"
            );
        }
    }

    #[test]
    fn approval_rule_rejects_unknown_fields() {
        // #155: ApprovalRuleSpec lacked deny_unknown_fields, so a typo'd
        // sub-field would silently pass through.
        let src = r#"actoin = "auto_approve""#;
        let err: Result<super::super::schema::ApprovalRuleSpec, _> = toml::from_str(src);
        assert!(err.is_err(), "unknown field 'actoin' must be rejected");
    }

    #[test]
    fn approval_match_rejects_unknown_fields() {
        // The classic typo from the audit: `catagory` instead of `category`.
        // Without deny_unknown_fields, the rule parses with no match filter
        // (every field None) and silently matches every event.
        let src = r#"catagory = "tool_use""#;
        let err: Result<super::super::schema::ApprovalMatchSpec, _> = toml::from_str(src);
        assert!(err.is_err(), "unknown field 'catagory' must be rejected");
    }
}
