#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::schema::{
    Defaults, Effort, Lead, LeadSpec, Manifest, RunConfig, SingleLeadManifest, SubleadDefaultsSpec,
    Task, Template, WorktreeCleanup,
};

/// Fully resolved task ready for dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedTask {
    pub id: String,
    pub directory: PathBuf,
    pub prompt: String,
    pub branch: Option<String>,
    pub model: String,
    pub effort: Effort,
    pub tools: Vec<String>,
    pub timeout_secs: u64,
    pub use_worktree: bool,
    pub env: HashMap<String, String>,
    /// When set, pass `--resume <id>` to claude so it continues a prior session.
    #[serde(default)]
    pub resume_session_id: Option<String>,
}

/// Fully resolved lead ready for dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedLead {
    pub id: String,
    pub directory: PathBuf,
    pub prompt: String,
    pub branch: Option<String>,
    pub model: String,
    pub effort: Effort,
    pub tools: Vec<String>,
    pub timeout_secs: u64,
    pub use_worktree: bool,
    pub env: HashMap<String, String>,
    /// When set, pass `--resume <id>` to claude so the lead continues a prior
    /// session. Populated by `build_resume_hierarchical`; `None` for fresh runs.
    #[serde(default)]
    pub resume_session_id: Option<String>,

    // ── v0.6 depth-2 cap fields ──────────────────────────────────────────────
    /// When true, `spawn_sublead` is included in the root lead's MCP toolset.
    /// Default false preserves v0.5 behavior.
    #[serde(default)]
    pub allow_subleads: bool,

    /// Hard cap on total live sub-leads under this root.
    #[serde(default)]
    pub max_subleads: Option<u32>,

    /// Hard cap on per-sub-lead budget envelope (USD).
    #[serde(default)]
    pub max_sublead_budget_usd: Option<f64>,

    /// Hard cap on total live workers across the entire tree
    /// (root-level workers + all sub-tree workers).
    #[serde(default)]
    pub max_workers_across_tree: Option<u32>,

    /// Optional defaults applied when `spawn_sublead` omits a param.
    #[serde(default)]
    pub sublead_defaults: Option<SubleadDefaults>,
}

/// v0.6: resolved defaults for sub-lead spawn requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubleadDefaults {
    pub budget_usd: Option<f64>,
    pub max_workers: Option<u32>,
    pub lead_timeout_secs: Option<u64>,
    pub read_down: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedManifest {
    pub max_parallel: u32,
    pub halt_on_failure: bool,
    pub run_dir: PathBuf,
    pub worktree_cleanup: WorktreeCleanup,
    pub emit_event_stream: bool,
    pub tasks: Vec<ResolvedTask>,
    // NEW in v0.3:
    pub lead: Option<ResolvedLead>,
    pub max_workers: Option<u32>,
    pub budget_usd: Option<f64>,
    pub lead_timeout_secs: Option<u64>,
    // NEW in v0.4:
    pub approval_policy: Option<crate::dispatch::state::ApprovalPolicy>,
    // NEW in v0.4.1:
    #[serde(default)]
    pub notifications: Vec<crate::notify::config::NotificationConfig>,
    // NEW in v0.4.2:
    #[serde(default)]
    pub dump_shared_store: bool,
    // NEW in v0.4.5 — when true, gate `spawn_worker` on an approved
    // plan submitted via the `propose_plan` MCP tool.
    #[serde(default)]
    pub require_plan_approval: bool,
    // NEW in v0.6 — declarative approval policy rules resolved from
    // [[approval_policy]] manifest blocks. Empty vec means no policy.
    #[serde(default)]
    pub approval_rules: Vec<crate::mcp::policy::ApprovalRule>,
}

const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_EFFORT: Effort = Effort::High;
const DEFAULT_TIMEOUT_SECS: u64 = 3600;
const DEFAULT_MAX_PARALLEL: u32 = 4;
fn default_tools() -> Vec<String> {
    ["Read", "Write", "Edit", "Bash", "Glob", "Grep"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub fn resolve(manifest: Manifest, env_max_parallel: Option<u32>) -> Result<ResolvedManifest> {
    let templates: HashMap<String, &Template> = manifest
        .templates
        .iter()
        .map(|t| (t.id.clone(), t))
        .collect();

    let mut resolved = Vec::with_capacity(manifest.tasks.len());
    for task in &manifest.tasks {
        resolved.push(resolve_task(task, &manifest.defaults, &templates)?);
    }

    let lead = if let Some(l) = manifest.leads.first() {
        Some(resolve_lead(l, &manifest.defaults, &manifest.run)?)
    } else {
        None
    };

    let max_parallel = manifest
        .run
        .max_parallel
        .or(env_max_parallel)
        .unwrap_or(DEFAULT_MAX_PARALLEL);

    let run_dir = manifest.run.run_dir.unwrap_or_else(default_run_dir);

    // Apply env-var substitution to notification URLs at resolve time.
    let mut notifications = manifest.notification.clone();
    for cfg in &mut notifications {
        crate::notify::config::apply_env_substitution(cfg)?;
    }

    // Resolve [[approval_policy]] TOML specs into typed ApprovalRule values.
    let approval_rules = manifest
        .approval_policy_rules
        .iter()
        .map(resolve_approval_rule)
        .collect::<Result<Vec<_>>>()?;

    Ok(ResolvedManifest {
        max_parallel,
        halt_on_failure: manifest.run.halt_on_failure,
        run_dir,
        worktree_cleanup: manifest.run.worktree_cleanup,
        emit_event_stream: manifest.run.emit_event_stream,
        tasks: resolved,
        lead,
        max_workers: manifest.run.max_workers,
        budget_usd: manifest.run.budget_usd,
        lead_timeout_secs: manifest.run.lead_timeout_secs,
        approval_policy: manifest.run.approval_policy,
        notifications,
        dump_shared_store: manifest.run.dump_shared_store,
        require_plan_approval: manifest.run.require_plan_approval,
        approval_rules,
    })
}

fn resolve_lead(lead: &Lead, defaults: &Defaults, run: &RunConfig) -> Result<ResolvedLead> {
    let mut env = defaults.env.clone();
    env.extend(lead.env.clone());

    // Lead timeout cascade: per-lead timeout_secs > [run].lead_timeout_secs > defaults.timeout_secs > 3600
    let timeout_secs = lead
        .timeout_secs
        .or(run.lead_timeout_secs)
        .or(defaults.timeout_secs)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    Ok(ResolvedLead {
        id: lead.id.clone(),
        directory: lead.directory.clone(),
        prompt: lead.prompt.clone(),
        branch: lead.branch.clone(),
        model: lead
            .model
            .clone()
            .or_else(|| defaults.model.clone())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        effort: lead.effort.or(defaults.effort).unwrap_or(DEFAULT_EFFORT),
        tools: lead
            .tools
            .clone()
            .or_else(|| defaults.tools.clone())
            .unwrap_or_else(default_tools),
        timeout_secs,
        use_worktree: lead.use_worktree.or(defaults.use_worktree).unwrap_or(true),
        env,
        resume_session_id: None,
        // v0.6 depth-2 fields: not present in [[lead]] array format; default to
        // off/None so existing v0.5 manifests behave identically.
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_workers_across_tree: None,
        sublead_defaults: None,
    })
}

fn resolve_task(
    task: &Task,
    defaults: &Defaults,
    templates: &HashMap<String, &Template>,
) -> Result<ResolvedTask> {
    let prompt = match (&task.prompt, &task.template) {
        (Some(p), None) => p.clone(),
        (None, Some(tid)) => {
            let tmpl = templates.get(tid).ok_or_else(|| {
                anyhow!("task '{}' references unknown template '{}'", task.id, tid)
            })?;
            substitute(&tmpl.prompt, &task.vars)
                .with_context(|| format!("rendering template '{}' for task '{}'", tid, task.id))?
        }
        (Some(_), Some(_)) => bail!("task '{}' sets both prompt and template", task.id),
        (None, None) => bail!("task '{}' has no prompt and no template", task.id),
    };

    let mut env = defaults.env.clone();
    env.extend(task.env.clone());

    Ok(ResolvedTask {
        id: task.id.clone(),
        directory: task.directory.clone(),
        prompt,
        branch: task.branch.clone(),
        model: task
            .model
            .clone()
            .or_else(|| defaults.model.clone())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        effort: task.effort.or(defaults.effort).unwrap_or(DEFAULT_EFFORT),
        tools: task
            .tools
            .clone()
            .or_else(|| defaults.tools.clone())
            .unwrap_or_else(default_tools),
        timeout_secs: task
            .timeout_secs
            .or(defaults.timeout_secs)
            .unwrap_or(DEFAULT_TIMEOUT_SECS),
        use_worktree: task.use_worktree.or(defaults.use_worktree).unwrap_or(true),
        env,
        resume_session_id: None,
    })
}

/// Resolve a v0.6 `SingleLeadManifest` (parsed from `[lead]` single-table TOML)
/// into a `ResolvedManifest`. Used by `load_manifest_from_str`.
///
/// Unlike `resolve`, this path does NOT call `validate` so it can be used
/// in unit tests that don't set up real git work-trees.
pub fn resolve_single_lead(
    manifest: SingleLeadManifest,
    env_max_parallel: Option<u32>,
) -> Result<ResolvedManifest> {
    let lead = if let Some(spec) = manifest.lead {
        Some(resolve_lead_spec(&spec, &manifest.run)?)
    } else {
        None
    };

    let max_parallel = manifest
        .run
        .max_parallel
        .or(env_max_parallel)
        .unwrap_or(DEFAULT_MAX_PARALLEL);

    let run_dir = manifest.run.run_dir.unwrap_or_else(default_run_dir);

    let mut notifications = manifest.notification.clone();
    for cfg in &mut notifications {
        crate::notify::config::apply_env_substitution(cfg)?;
    }

    let approval_rules = manifest
        .approval_policy_rules
        .iter()
        .map(resolve_approval_rule)
        .collect::<Result<Vec<_>>>()?;

    Ok(ResolvedManifest {
        max_parallel,
        halt_on_failure: manifest.run.halt_on_failure,
        run_dir,
        worktree_cleanup: manifest.run.worktree_cleanup,
        emit_event_stream: manifest.run.emit_event_stream,
        tasks: vec![],
        lead,
        // In the [lead] single-table format, budget_usd / max_workers /
        // lead_timeout_secs live ON the [lead] table rather than [run].
        // ResolvedManifest top-level fields are left None here; callers
        // that need the per-lead values read from ResolvedLead directly.
        max_workers: manifest.run.max_workers,
        budget_usd: manifest.run.budget_usd,
        lead_timeout_secs: manifest.run.lead_timeout_secs,
        approval_policy: manifest.run.approval_policy,
        notifications,
        dump_shared_store: manifest.run.dump_shared_store,
        require_plan_approval: manifest.run.require_plan_approval,
        approval_rules,
    })
}

/// Resolve a `LeadSpec` (single-table `[lead]`) into a `ResolvedLead`.
fn resolve_lead_spec(spec: &LeadSpec, run: &RunConfig) -> Result<ResolvedLead> {
    let timeout_secs = spec
        .timeout_secs
        .or(run.lead_timeout_secs)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    let sublead_defaults = spec.sublead_defaults.as_ref().map(resolve_sublead_defaults);

    Ok(ResolvedLead {
        // id and directory are not present in the [lead] single-table format
        // (they come from the runtime context). Default to the process CWD so
        // the lead spawns in the same directory the operator ran pitboss from;
        // callers of load_manifest_from_str that need a different directory
        // can override it after resolution.
        id: String::new(),
        directory: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        prompt: spec.prompt.clone().unwrap_or_default(),
        branch: None,
        model: spec
            .model
            .clone()
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        effort: spec.effort.unwrap_or(DEFAULT_EFFORT),
        tools: spec.tools.clone().unwrap_or_else(default_tools),
        timeout_secs,
        use_worktree: spec.use_worktree.unwrap_or(true),
        env: spec.env.clone(),
        resume_session_id: None,
        // v0.6 depth-2 fields:
        allow_subleads: spec.allow_subleads,
        max_subleads: spec.max_subleads,
        max_sublead_budget_usd: spec.max_sublead_budget_usd,
        max_workers_across_tree: spec.max_workers_across_tree,
        sublead_defaults,
    })
}

/// Convert a `SubleadDefaultsSpec` (TOML deserialized) into a `SubleadDefaults`
/// (runtime resolved).
fn resolve_sublead_defaults(spec: &SubleadDefaultsSpec) -> SubleadDefaults {
    SubleadDefaults {
        budget_usd: spec.budget_usd,
        max_workers: spec.max_workers,
        lead_timeout_secs: spec.lead_timeout_secs,
        read_down: spec.read_down,
    }
}

fn substitute(template: &str, vars: &HashMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut iter = template.chars().peekable();
    while let Some(c) = iter.next() {
        match c {
            '\\' => {
                if matches!(iter.peek(), Some('{') | Some('}')) {
                    out.push(iter.next().unwrap());
                } else {
                    out.push(c);
                }
            }
            '{' => {
                let mut name = String::new();
                for nc in iter.by_ref() {
                    if nc == '}' {
                        let value = vars
                            .get(&name)
                            .ok_or_else(|| anyhow!("undeclared var '{}' in template", name))?;
                        out.push_str(value);
                        break;
                    }
                    name.push(nc);
                }
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

fn default_run_dir() -> PathBuf {
    if let Some(h) = dirs_home() {
        h.join(".local/share/pitboss/runs")
    } else {
        PathBuf::from("./pitboss-runs")
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Convert a TOML `ApprovalRuleSpec` into a typed `ApprovalRule`.
/// Returns an error if `action` or `match.category` is an unrecognised string.
fn resolve_approval_rule(
    spec: &super::schema::ApprovalRuleSpec,
) -> Result<crate::mcp::policy::ApprovalRule> {
    use crate::mcp::approval::ApprovalCategory;
    use crate::mcp::policy::{ApprovalAction, ApprovalMatch, ApprovalRule};

    let action = match spec.action.as_str() {
        "auto_approve" => ApprovalAction::AutoApprove,
        "auto_reject" => ApprovalAction::AutoReject,
        "block" => ApprovalAction::Block,
        other => anyhow::bail!(
            "unknown approval_policy action '{}'; expected auto_approve, auto_reject, or block",
            other
        ),
    };

    let category = match spec.match_clause.category.as_deref() {
        None => None,
        Some("tool_use") => Some(ApprovalCategory::ToolUse),
        Some("plan") => Some(ApprovalCategory::Plan),
        Some("cost") => Some(ApprovalCategory::Cost),
        Some("other") => Some(ApprovalCategory::Other),
        Some(other) => anyhow::bail!(
            "unknown approval_policy match.category '{}'; expected tool_use, plan, cost, or other",
            other
        ),
    };

    Ok(ApprovalRule {
        r#match: ApprovalMatch {
            actor: spec.match_clause.actor.clone(),
            category,
            tool_name: spec.match_clause.tool_name.clone(),
            cost_over: spec.match_clause.cost_over,
        },
        action,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn man(src: &str) -> Manifest {
        toml::from_str(src).unwrap()
    }

    #[test]
    fn resolves_inline_prompt() {
        let m = man(r#"
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "hi"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.tasks[0].prompt, "hi");
        assert_eq!(r.max_parallel, 4);
    }

    #[test]
    fn resolves_template_with_vars() {
        let m = man(r#"
            [[template]]
            id = "t"
            prompt = "hi {name}"
            [[task]]
            id = "a"
            directory = "/tmp"
            template = "t"
            vars = { name = "ada" }
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.tasks[0].prompt, "hi ada");
    }

    #[test]
    fn undeclared_var_errors() {
        let m = man(r#"
            [[template]]
            id = "t"
            prompt = "hi {missing}"
            [[task]]
            id = "a"
            directory = "/tmp"
            template = "t"
        "#);
        assert!(resolve(m, None).is_err());
    }

    #[test]
    fn task_overrides_defaults() {
        let m = man(r#"
            [defaults]
            model  = "default-m"
            tools  = ["Read"]
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
            model  = "override-m"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.tasks[0].model, "override-m");
        assert_eq!(r.tasks[0].tools, vec!["Read"]);
    }

    #[test]
    fn env_var_precedence_applies() {
        let m = man(r#"
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, Some(16)).unwrap();
        assert_eq!(r.max_parallel, 16);
    }

    #[test]
    fn manifest_max_parallel_wins_over_env() {
        let m = man(r#"
            [run]
            max_parallel = 2
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, Some(16)).unwrap();
        assert_eq!(r.max_parallel, 2);
    }

    #[test]
    fn escaped_braces_are_literal() {
        let m = man(r#"
            [[template]]
            id = "t"
            prompt = 'literal \{ and \}'
            [[task]]
            id = "a"
            directory = "/tmp"
            template = "t"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.tasks[0].prompt, "literal { and }");
    }

    #[test]
    fn resume_session_id_defaults_to_none() {
        let m = man(r#"
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, None).unwrap();
        assert!(
            r.tasks[0].resume_session_id.is_none(),
            "resume_session_id should default to None"
        );
    }

    #[test]
    fn resolves_lead_inheriting_defaults() {
        let m = man(r#"
            [defaults]
            model = "claude-haiku-4-5"
            tools = ["Read","Write"]
            timeout_secs = 1800

            [[lead]]
            id = "triage"
            directory = "/tmp"
            prompt = "coordinate"
        "#);
        let r = resolve(m, None).unwrap();
        let lead = r.lead.as_ref().expect("must resolve a lead");
        assert_eq!(lead.id, "triage");
        assert_eq!(lead.model, "claude-haiku-4-5");
        assert_eq!(lead.tools, vec!["Read", "Write"]);
        assert_eq!(lead.timeout_secs, 1800);
        assert!(lead.use_worktree);
        assert!(r.tasks.is_empty());
    }

    #[test]
    fn lead_overrides_defaults() {
        let m = man(r#"
            [defaults]
            model = "claude-haiku-4-5"

            [[lead]]
            id = "triage"
            directory = "/tmp"
            prompt = "coordinate"
            model = "claude-sonnet-4-6"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.lead.unwrap().model, "claude-sonnet-4-6");
    }

    #[test]
    fn lead_timeout_falls_back_to_run_lead_timeout_secs() {
        let m = man(r#"
            [run]
            lead_timeout_secs = 7200

            [[lead]]
            id = "triage"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.lead.unwrap().timeout_secs, 7200);
    }

    #[test]
    fn resolves_approval_policy_from_run() {
        let m = man(r#"
            [run]
            approval_policy = "auto_reject"

            [[lead]]
            id = "triage"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(
            r.approval_policy,
            Some(crate::dispatch::state::ApprovalPolicy::AutoReject)
        );
    }

    #[test]
    fn resolves_missing_approval_policy_as_none() {
        let m = man(r#"
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, None).unwrap();
        assert!(r.approval_policy.is_none());
    }

    #[test]
    fn resolves_notifications_with_env_substitution() {
        std::env::set_var("PITBOSS_TEST_WEBHOOK", "https://h.example/x");
        let m = man(r#"
[[notification]]
kind = "webhook"
url  = "${PITBOSS_TEST_WEBHOOK}"
events = ["run_finished"]

[[task]]
id = "t"
directory = "/tmp"
prompt = "p"
"#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.notifications.len(), 1);
        assert_eq!(
            r.notifications[0].url.as_deref(),
            Some("https://h.example/x")
        );
    }

    /// TOML round-trip for [[approval_policy]] blocks: verifies that the
    /// `#[serde(rename = "match")]` annotation on `match_clause` is correct,
    /// that the action and match fields are parsed, and that the resolved
    /// `ResolvedManifest.approval_rules` has the expected content.
    ///
    /// A typo in the rename attribute or a regression in `resolve_approval_rule`
    /// would cause this test to fail even though the unit tests in policy.rs
    /// and schema.rs might pass.
    #[test]
    fn toml_approval_policy_round_trips_through_resolve() {
        use crate::mcp::approval::ApprovalCategory;
        use crate::mcp::policy::ApprovalAction;

        let m = man(r#"
[run]
max_parallel = 4

[[lead]]
id = "root"
directory = "/tmp"
prompt = "coordinate"
model = "claude-haiku-4-5"

[[approval_policy]]
action = "auto_approve"
[approval_policy.match]
actor = "root→S1"
category = "tool_use"
"#);
        let r = resolve(m, None).unwrap();

        assert_eq!(
            r.approval_rules.len(),
            1,
            "expected one approval rule, got: {:?}",
            r.approval_rules
        );

        let rule = &r.approval_rules[0];
        assert_eq!(
            rule.action,
            ApprovalAction::AutoApprove,
            "action should be AutoApprove"
        );
        assert_eq!(
            rule.r#match.actor.as_deref(),
            Some("root→S1"),
            "match.actor should be 'root→S1'"
        );
        assert_eq!(
            rule.r#match.category,
            Some(ApprovalCategory::ToolUse),
            "match.category should be ToolUse"
        );
        assert!(
            rule.r#match.tool_name.is_none(),
            "match.tool_name should be absent"
        );
        assert!(
            rule.r#match.cost_over.is_none(),
            "match.cost_over should be absent"
        );
    }
}
