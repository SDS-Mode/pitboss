#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::schema::{
    AgentProfile, CommunicationConfig, ContainerConfig, Defaults, Effort, Lead, Manifest,
    SubleadDefaults, Task, Template, WorktreeCleanup,
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

    /// Path A (default): `CLAUDE_CODE_ENTRYPOINT=sdk-ts` bypasses the gate.
    /// Path B: pitboss registers `permission_prompt` MCP tool; claude routes
    /// each permission check through it into pitboss's approval queue.
    #[serde(default)]
    pub permission_routing: crate::manifest::schema::PermissionRouting,

    /// When true, `spawn_sublead` is included in the root lead's MCP toolset.
    #[serde(default)]
    pub allow_subleads: bool,

    /// Hard cap on total live sub-leads under this root.
    #[serde(default)]
    pub max_subleads: Option<u32>,

    /// Hard cap on per-sub-lead budget envelope (USD).
    #[serde(default)]
    pub max_sublead_budget_usd: Option<f64>,

    /// Hard cap on total live workers across the entire tree
    /// (root-level workers + all sub-tree workers). Renamed from
    /// `max_workers_across_tree` in v0.9 to match the TOML field name.
    /// `alias` keeps pre-v0.9 `resolved.json` snapshots resumable.
    #[serde(default, alias = "max_workers_across_tree")]
    pub max_total_workers: Option<u32>,

    /// Resolved `[sublead_defaults]` (top-level in v0.9, was nested under
    /// `[lead.sublead_defaults]`).
    #[serde(default)]
    pub sublead_defaults: Option<ResolvedSubleadDefaults>,
}

/// Resolved defaults for sub-lead spawn requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedSubleadDefaults {
    #[serde(default)]
    pub budget_usd: Option<f64>,
    #[serde(default)]
    pub lead_budget_usd: Option<f64>,
    #[serde(default)]
    pub max_workers: Option<u32>,
    #[serde(default)]
    pub lead_timeout_secs: Option<u64>,
    pub read_down: bool,
}

/// Schema version baked into every `resolved.json` snapshot written by
/// this build. **Bump on any breaking change** to `ResolvedManifest`,
/// `ResolvedTask`, or `ResolvedLead` that pre-existing snapshots cannot
/// faithfully express via `#[serde(alias)]` — e.g. a removed field, a
/// changed type, or a semantic re-interpretation of an existing value.
/// Renames that ship with an `alias` are NOT breaking and do NOT require
/// a bump (they round-trip via the alias).
///
/// The resume loader rejects any snapshot with a version greater than
/// this constant, surfacing a typed
/// [`ManifestError::IncompatibleVersion`](crate::manifest::error::ManifestError)
/// instead of an opaque serde failure.
pub const CURRENT_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Default for the `manifest_schema_version` field when absent from a
/// snapshot. v0.9 snapshots predate this field; treating missing as `0`
/// (legacy) lets them resume on a best-effort basis. New snapshots
/// always carry [`CURRENT_MANIFEST_SCHEMA_VERSION`].
fn default_legacy_manifest_schema_version() -> u32 {
    0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedManifest {
    /// Discriminator written by [`resolve()`] so [`crate::dispatch::resume`]
    /// can reject snapshots produced by a newer pitboss whose schema this
    /// build cannot deserialize. Defaults to `0` for pre-versioning
    /// snapshots (anything written before v0.9.2).
    #[serde(default = "default_legacy_manifest_schema_version")]
    pub manifest_schema_version: u32,
    /// Human-readable label from `[run].name`. Surfaced into `RunSummary`
    /// so the operational console can group related runs without re-reading
    /// `manifest.snapshot.toml` per digest. `None` when the manifest omits
    /// the field.
    #[serde(default)]
    pub name: Option<String>,
    /// Concurrency cap for `[[task]]` flat-mode runs. `None` in
    /// hierarchical mode (the lead's `max_workers` plays this role; the
    /// flat-mode semaphore at `runner.rs` is never constructed). Renamed
    /// from `max_parallel` in v0.9 to match the TOML field name; `alias`
    /// keeps pre-v0.9 `resolved.json` snapshots resumable.
    ///
    /// #320: pre-fix this was always `Some(DEFAULT_MAX_PARALLEL_TASKS)`
    /// even for hierarchical manifests, which made `resolved.json` lie
    /// about the run's actual concurrency model.
    #[serde(
        default,
        alias = "max_parallel",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_parallel_tasks: Option<u32>,
    pub halt_on_failure: bool,
    pub run_dir: PathBuf,
    pub worktree_cleanup: WorktreeCleanup,
    pub emit_event_stream: bool,
    /// Optional `--setting-sources` override forwarded to every worker
    /// `claude … -p` spawn. `None` means apply pitboss's default (filter
    /// in container, no filter on host). Validated comma-separated
    /// subset of `user`, `project`, `local` — see
    /// `manifest::schema::Manifest::claude_setting_sources` doc-comment.
    #[serde(default)]
    pub claude_setting_sources: Option<String>,
    pub tasks: Vec<ResolvedTask>,
    #[serde(default)]
    pub lead: Option<ResolvedLead>,
    /// Surfaced from `[lead].max_workers` for consumer convenience.
    /// `None` in flat mode.
    #[serde(default)]
    pub max_workers: Option<u32>,
    /// Surfaced from `[lead].budget_usd`. `None` in flat mode.
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// Surfaced from `[lead].lead_budget_usd` — optional separate cap on
    /// lead + sub-lead token spend, independent of `budget_usd`. `None` when
    /// unset or in flat mode. (#253)
    #[serde(default)]
    pub lead_budget_usd: Option<f64>,
    /// Surfaced from `[lead].lead_timeout_secs`. `None` in flat mode.
    #[serde(default)]
    pub lead_timeout_secs: Option<u64>,
    /// Renamed from `approval_policy` in v0.9 to match the TOML field name
    /// and disambiguate from `approval_rules`.
    /// `alias` keeps pre-v0.9 `resolved.json` snapshots resumable.
    #[serde(default, alias = "approval_policy")]
    pub default_approval_policy: Option<crate::dispatch::state::ApprovalPolicy>,
    /// What happens to an actor's terminal status after a denied
    /// `permission_prompt`. `None` resolves to `Adapt` at read time.
    /// (#377)
    #[serde(default)]
    pub denial_termination_policy: Option<crate::dispatch::state::DenialTerminationPolicy>,
    /// Post-substitution notification configs. Skipped from serde
    /// (#346) — `apply_env_substitution` expands `${PITBOSS_NOTIFY_*}`
    /// placeholders into their literal values (Slack tokens, Discord
    /// webhook ids, `?token=...` query params), and serializing those
    /// to `resolved.json` would persist secrets to a long-lived
    /// on-disk artifact. Resume reads `manifest.snapshot.toml` (which
    /// stores the placeholder form) and re-runs substitution to
    /// repopulate this field — see [`crate::dispatch::resume`].
    #[serde(skip)]
    pub notifications: Vec<crate::notify::config::NotificationConfig>,
    #[serde(default)]
    pub dump_shared_store: bool,
    #[serde(default)]
    pub require_plan_approval: bool,
    /// Declarative approval policy rules resolved from `[[approval_policy]]`
    /// manifest blocks. Empty vec means no policy.
    #[serde(default)]
    pub approval_rules: Vec<crate::mcp::policy::ApprovalRule>,
    #[serde(default)]
    pub container: Option<ContainerConfig>,
    #[serde(default)]
    pub mcp_servers: Vec<crate::manifest::schema::McpServerSpec>,
    /// Pitboss-owned mailbox + artifact policy. `Default` (mode = Disabled)
    /// when the manifest omits `[communication]`. Pre-v0.10 `resolved.json`
    /// snapshots without this field also resolve to the disabled default.
    #[serde(default)]
    pub communication: CommunicationConfig,
    /// Resolved `[lifecycle]` section. `None` when the manifest omits it
    /// entirely (the common case — pitboss's default semantics apply: dies
    /// with parent, no out-of-band lifecycle notify expected).
    #[serde(default)]
    pub lifecycle: Option<crate::manifest::schema::Lifecycle>,
    /// Typed worker profiles (`[[worker_type]]`). Looked up by id in
    /// `handle_spawn_worker` to enforce per-class capability caps.
    /// Empty in v0.11 manifests that pre-date this surface.
    /// (#252)
    #[serde(default)]
    pub worker_types: Vec<crate::manifest::schema::WorkerType>,
    /// Typed sub-lead profiles (`[[sublead_type]]`). Looked up by id in
    /// the `spawn_sublead` MCP handler. (#252)
    #[serde(default)]
    pub sublead_types: Vec<crate::manifest::schema::SubleadType>,
    /// When true, every `spawn_worker` / `spawn_sublead` call MUST name
    /// a profile; type-less spawns are rejected. Default `false` for
    /// back-compat. (#252)
    #[serde(default)]
    pub require_actor_type: bool,
    /// Path-B-only policy for un-typed actors reaching
    /// `permission_prompt`. Default `Bridge` preserves pre-#252
    /// behavior; `Block` synthesizes an empty profile so the call
    /// auto-denies via `denied_by_profile`. (#252)
    #[serde(default)]
    pub untyped_actor_policy: crate::manifest::schema::UntypedActorPolicy,
    /// Effective [`AgentProfile`] catalogue: bundled built-ins union
    /// with manifest-declared `[[agent_profile]]` entries. Manifest
    /// entries shadow built-ins by matching id exactly. Looked up at
    /// MCP-spawn time by `worker_type`/`sublead_type` →
    /// `agent_profile` references to compose worker/sublead prompts +
    /// env defaults. Serialised so `resolved.json` round-trips through
    /// resume.
    ///
    /// Note: this is the *merged* catalogue. To distinguish
    /// manifest-declared from built-in entries (used by
    /// `validate_agent_profiles` for namespace-squatting detection),
    /// re-load the built-in id set via
    /// `builtin_profiles::load_builtins()` and diff — the duplicate
    /// load is cheap (3 `include_str!` parses) and avoids carrying a
    /// second `agent_profiles_manifest: Vec<AgentProfile>` field whose
    /// only consumer is validate.
    #[serde(default)]
    pub agent_profiles: HashMap<String, AgentProfile>,
}

const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_EFFORT: Effort = Effort::High;
const DEFAULT_TIMEOUT_SECS: u64 = 3600;
use super::schema::DEFAULT_MAX_PARALLEL_TASKS;
fn default_tools() -> Vec<String> {
    ["Read", "Write", "Edit", "Bash", "Glob", "Grep"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub fn resolve(
    manifest: Manifest,
    env_max_parallel_tasks: Option<u32>,
) -> Result<ResolvedManifest> {
    let templates: HashMap<String, &Template> = manifest
        .templates
        .iter()
        .map(|t| (t.id.clone(), t))
        .collect();

    // Reject duplicate manifest-declared agent_profile ids up-front: a
    // duplicate would silently shadow the earlier entry once we collapse
    // into a HashMap, matching the dedup posture of [[worker_type]] in
    // validate.rs.
    {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for p in &manifest.agent_profiles {
            if !seen.insert(p.id.as_str()) {
                bail!(
                    "[[agent_profile]].id {:?}: duplicate; ids must be \
                     unique within the manifest (catalogue lookup collapses \
                     duplicates and would silently shadow the earlier entry)",
                    p.id
                );
            }
        }
    }
    // Build the effective agent-profile catalogue: bundled built-ins
    // first, manifest entries last so a manifest entry sharing a
    // built-in id shadows it via HashMap::insert's last-wins semantics.
    let agent_profiles: HashMap<String, AgentProfile> =
        crate::manifest::builtin_profiles::load_builtins()
            .into_iter()
            .chain(manifest.agent_profiles.iter().cloned())
            .map(|p| (p.id.clone(), p))
            .collect();

    // Lookup helper: dangling agent_profile references on `[lead]` /
    // `[[task]]` hard-fail at resolve so a typo doesn't silently fall
    // back to "no prelude" — operator intent is lost otherwise.
    let lookup_profile = |surface: &str,
                          maybe_ref: Option<&str>|
     -> Result<Option<&AgentProfile>> {
        match maybe_ref {
            Some(id) => match agent_profiles.get(id) {
                Some(p) => Ok(Some(p)),
                None => {
                    let mut known: Vec<&str> = agent_profiles.keys().map(String::as_str).collect();
                    known.sort();
                    bail!(
                        "{surface}: agent_profile {id:?} not found. \
                         Declared profiles (built-ins + manifest): {}. \
                         See `pitboss schema --format=agent-profiles`.",
                        known.join(", ")
                    );
                }
            },
            None => Ok(None),
        }
    };

    let mut resolved_tasks = Vec::with_capacity(manifest.tasks.len());
    for task in &manifest.tasks {
        let profile = lookup_profile(
            &format!("[[task]] id={:?}", task.id),
            task.agent_profile.as_deref(),
        )?;
        resolved_tasks.push(resolve_task(task, &manifest.defaults, &templates, profile)?);
    }

    let resolved_sublead_defaults = manifest
        .sublead_defaults
        .as_ref()
        .map(resolve_sublead_defaults);

    let resolved_lead = if let Some(l) = &manifest.lead {
        let profile = lookup_profile(&format!("[lead] id={:?}", l.id), l.agent_profile.as_deref())?;
        Some(resolve_lead(
            l,
            &manifest.defaults,
            resolved_sublead_defaults.clone(),
            profile,
        )?)
    } else {
        None
    };

    // #320: max_parallel_tasks is flat-mode-only. Hierarchical mode caps
    // concurrency via `lead.max_workers`; a default applied here would
    // surface in `resolved.json` as a misleading non-zero value the
    // dispatcher never reads.
    let max_parallel_tasks = if resolved_lead.is_some() {
        None
    } else {
        Some(
            manifest
                .run
                .max_parallel_tasks
                .or(env_max_parallel_tasks)
                .unwrap_or(DEFAULT_MAX_PARALLEL_TASKS),
        )
    };

    let run_dir = manifest.run.run_dir.unwrap_or_else(default_run_dir);

    // Apply env-var substitution to notification URLs at resolve time.
    let mut notifications = manifest.notification.clone();
    for cfg in &mut notifications {
        crate::notify::config::apply_env_substitution(cfg)?;
    }

    let approval_rules = manifest
        .approval_policy_rules
        .iter()
        .map(resolve_approval_rule)
        .collect::<Result<Vec<_>>>()?;

    // Surface lead-level caps at the top level of ResolvedManifest for
    // consumer convenience. `None` when no [lead] is declared (flat mode).
    let (max_workers, budget_usd, lead_budget_usd, lead_timeout_secs) = match manifest.lead.as_ref()
    {
        Some(l) => (
            l.max_workers,
            l.budget_usd,
            l.lead_budget_usd,
            l.lead_timeout_secs,
        ),
        None => (None, None, None, None),
    };

    Ok(ResolvedManifest {
        manifest_schema_version: CURRENT_MANIFEST_SCHEMA_VERSION,
        name: manifest.run.name.clone(),
        max_parallel_tasks,
        halt_on_failure: manifest.run.halt_on_failure,
        run_dir,
        worktree_cleanup: manifest.run.worktree_cleanup,
        emit_event_stream: manifest.run.emit_event_stream,
        claude_setting_sources: manifest.run.claude_setting_sources.clone(),
        tasks: resolved_tasks,
        lead: resolved_lead,
        max_workers,
        budget_usd,
        lead_budget_usd,
        lead_timeout_secs,
        default_approval_policy: manifest.run.default_approval_policy,
        denial_termination_policy: manifest.run.denial_termination_policy,
        notifications,
        dump_shared_store: manifest.run.dump_shared_store,
        require_plan_approval: manifest.run.require_plan_approval,
        approval_rules,
        container: manifest.container,
        mcp_servers: manifest.mcp_servers,
        communication: manifest.communication,
        lifecycle: manifest.lifecycle,
        worker_types: manifest.worker_types,
        sublead_types: manifest.sublead_types,
        require_actor_type: manifest.run.require_actor_type,
        untyped_actor_policy: manifest.run.untyped_actor_policy,
        agent_profiles,
    })
}

/// Compose the worker/lead/task prompt by prepending the profile's
/// `system_prompt` (if any) to the operator's prompt with a fixed
/// `\n\n--- TASK ---\n\n` separator. Operator content is never
/// replaced — the profile only contributes the prelude.
pub fn compose_prompt(profile: Option<&AgentProfile>, operator_prompt: &str) -> String {
    match profile
        .map(|p| p.system_prompt.as_str())
        .filter(|s| !s.is_empty())
    {
        Some(prelude) => format!("{prelude}\n\n--- TASK ---\n\n{operator_prompt}"),
        None => operator_prompt.to_string(),
    }
}

fn resolve_lead(
    lead: &Lead,
    defaults: &Defaults,
    sublead_defaults: Option<ResolvedSubleadDefaults>,
    profile: Option<&AgentProfile>,
) -> Result<ResolvedLead> {
    // Env precedence (later wins): defaults.env → profile.env → lead.env.
    // Operator-supplied per-actor env beats the profile's role default,
    // which beats the manifest-wide defaults block.
    let mut env = defaults.env.clone();
    if let Some(p) = profile {
        for (k, v) in &p.env {
            env.insert(k.clone(), v.clone());
        }
    }
    env.extend(lead.env.clone());

    // Lead timeout cascade: per-lead timeout_secs > lead.lead_timeout_secs
    // > defaults.timeout_secs > 3600.
    let timeout_secs = lead
        .timeout_secs
        .or(lead.lead_timeout_secs)
        .or(defaults.timeout_secs)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    let prompt = compose_prompt(profile, &lead.prompt);

    Ok(ResolvedLead {
        id: lead.id.clone(),
        directory: lead.directory.clone(),
        prompt,
        branch: lead.branch.clone(),
        // Model precedence: lead.model > defaults.model > profile.model
        // > DEFAULT_MODEL. The profile is a default-provider, not an
        // override.
        model: lead
            .model
            .clone()
            .or_else(|| defaults.model.clone())
            .or_else(|| profile.and_then(|p| p.model.clone()))
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        effort: lead.effort.or(defaults.effort).unwrap_or(DEFAULT_EFFORT),
        // Tools precedence mirrors model: lead.tools > defaults.tools
        // > profile.tools > default_tools().
        tools: lead
            .tools
            .clone()
            .or_else(|| defaults.tools.clone())
            .or_else(|| profile.and_then(|p| p.tools.clone()))
            .unwrap_or_else(default_tools),
        timeout_secs,
        use_worktree: lead.use_worktree.or(defaults.use_worktree).unwrap_or(true),
        env,
        resume_session_id: None,
        permission_routing: lead.permission_routing,
        allow_subleads: lead.allow_subleads,
        max_subleads: lead.max_subleads,
        max_sublead_budget_usd: lead.max_sublead_budget_usd,
        max_total_workers: lead.max_total_workers,
        sublead_defaults,
    })
}

fn resolve_task(
    task: &Task,
    defaults: &Defaults,
    templates: &HashMap<String, &Template>,
    profile: Option<&AgentProfile>,
) -> Result<ResolvedTask> {
    let raw_prompt = match (&task.prompt, &task.template) {
        (Some(p), None) => p.clone(),
        (None, Some(tid)) => {
            let tmpl = templates.get(tid).ok_or_else(|| {
                anyhow!("task '{}' references unknown template '{}'", task.id, tid)
            })?;
            substitute(&tmpl.prompt, &task.vars)
                .with_context(|| format!("rendering template '{}' for task '{}'", tid, task.id))?
        }
        (Some(_), Some(_)) => bail!("task '{}' sets both prompt and template", task.id),
        (None, None) => bail!(
            "task '{}': prompt is required (set `prompt = \"...\"` or reference a [[template]] via `template = \"id\"`)",
            task.id
        ),
    };
    let prompt = compose_prompt(profile, &raw_prompt);

    // Env precedence (later wins): defaults.env → profile.env → task.env.
    let mut env = defaults.env.clone();
    if let Some(p) = profile {
        for (k, v) in &p.env {
            env.insert(k.clone(), v.clone());
        }
    }
    env.extend(task.env.clone());

    Ok(ResolvedTask {
        id: task.id.clone(),
        directory: task.directory.clone(),
        prompt,
        branch: task.branch.clone(),
        // Model/tools precedence: per-task > defaults > profile > built-in.
        model: task
            .model
            .clone()
            .or_else(|| defaults.model.clone())
            .or_else(|| profile.and_then(|p| p.model.clone()))
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        effort: task.effort.or(defaults.effort).unwrap_or(DEFAULT_EFFORT),
        tools: task
            .tools
            .clone()
            .or_else(|| defaults.tools.clone())
            .or_else(|| profile.and_then(|p| p.tools.clone()))
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

/// Convert a `SubleadDefaults` (TOML deserialized) into a `ResolvedSubleadDefaults`.
fn resolve_sublead_defaults(spec: &SubleadDefaults) -> ResolvedSubleadDefaults {
    ResolvedSubleadDefaults {
        budget_usd: spec.budget_usd,
        lead_budget_usd: spec.lead_budget_usd,
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
                let mut closed = false;
                for nc in iter.by_ref() {
                    if nc == '}' {
                        closed = true;
                        break;
                    }
                    name.push(nc);
                }
                if !closed {
                    bail!(
                        "unclosed '{{' in template string; expected '}}' after '{}'",
                        name
                    );
                }
                let value = vars
                    .get(&name)
                    .ok_or_else(|| anyhow!("undeclared var '{}' in template", name))?;
                out.push_str(value);
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
    use serial_test::serial;

    fn man(src: &str) -> Manifest {
        toml::from_str(src).unwrap()
    }

    #[test]
    fn merges_defaults_env_into_lead() {
        let m = man(r#"
            [defaults]
            model = "claude-sonnet-4-6"

            [defaults.env]
            WORK_DIR = "/tmp/foo"
            ARTIFACTS_DIR = "/tmp/bar"

            [lead]
            id = "root"
            directory = "/tmp"
            prompt = "test"
            budget_usd = 1.0
            max_workers = 2
            allow_subleads = true

            [sublead_defaults]
            read_down = true
            "#);
        let r = resolve(m, None).unwrap();
        let lead = r.lead.as_ref().unwrap();
        assert_eq!(
            lead.env.get("WORK_DIR"),
            Some(&"/tmp/foo".to_string()),
            "defaults.env.WORK_DIR must propagate to the lead"
        );
        assert_eq!(
            lead.env.get("ARTIFACTS_DIR"),
            Some(&"/tmp/bar".to_string()),
            "defaults.env.ARTIFACTS_DIR must propagate to the lead"
        );
    }

    #[test]
    fn merges_defaults_model_when_lead_omits() {
        let m = man(r#"
            [defaults]
            model = "claude-sonnet-4-6"

            [lead]
            id = "root"
            directory = "/tmp"
            prompt = "x"
            "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.lead.as_ref().unwrap().model, "claude-sonnet-4-6");
    }

    #[test]
    fn lead_model_overrides_defaults_model() {
        let m = man(r#"
            [defaults]
            model = "claude-haiku-4-5"

            [lead]
            id = "root"
            directory = "/tmp"
            prompt = "x"
            model = "claude-opus-4-7"
            "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.lead.as_ref().unwrap().model, "claude-opus-4-7");
    }

    #[test]
    fn lead_env_overrides_defaults_env_on_collision() {
        let m = man(r#"
            [defaults.env]
            SHARED = "default-value"

            [lead]
            id = "root"
            directory = "/tmp"
            prompt = "x"

            [lead.env]
            SHARED = "lead-override"
            EXTRA = "lead-only"
            "#);
        let r = resolve(m, None).unwrap();
        let env = &r.lead.as_ref().unwrap().env;
        assert_eq!(env.get("SHARED"), Some(&"lead-override".to_string()));
        assert_eq!(env.get("EXTRA"), Some(&"lead-only".to_string()));
    }

    #[test]
    fn merges_defaults_tools_when_lead_omits() {
        let m = man(r#"
            [defaults]
            tools = ["Read", "Bash"]

            [lead]
            id = "root"
            directory = "/tmp"
            prompt = "x"
            "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.lead.as_ref().unwrap().tools, vec!["Read", "Bash"]);
    }

    #[test]
    fn unknown_top_level_section_rejected() {
        // [default] (singular) is a common typo for [defaults]. With
        // deny_unknown_fields, this fails at parse instead of silently
        // dropping the operator's intent.
        let result: Result<Manifest, _> = toml::from_str(
            r#"
            [default]
            model = "claude-sonnet-4-6"

            [lead]
            id = "x"
            directory = "/tmp"
            prompt = "x"
            "#,
        );
        assert!(result.is_err(), "expected parse error for [default] typo");
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
        assert_eq!(r.max_parallel_tasks, Some(4));
    }

    /// #320: hierarchical manifests do NOT use `max_parallel_tasks`
    /// (lead.max_workers caps concurrency there). Resolving such a
    /// manifest must leave the field as `None` rather than the flat-mode
    /// default — otherwise `resolved.json` carries a misleading value
    /// the dispatcher never reads.
    #[test]
    fn hierarchical_manifest_resolves_max_parallel_tasks_to_none() {
        let m = man(r#"
            [lead]
            id = "lead"
            directory = "/tmp"
            prompt = "p"
            allow_subleads = false
            max_workers = 4
            budget_usd = 5.0
        "#);
        let r = resolve(m, None).unwrap();
        assert!(
            r.lead.is_some(),
            "fixture must produce a hierarchical manifest"
        );
        assert_eq!(
            r.max_parallel_tasks, None,
            "hierarchical mode must leave max_parallel_tasks unset; pre-fix it was Some(4)"
        );
    }

    /// Even when an env-var override is supplied, hierarchical mode
    /// still resolves to `None` — the env var only applies to flat-mode
    /// `[[task]]` runs.
    #[test]
    fn hierarchical_manifest_ignores_env_max_parallel_override() {
        let m = man(r#"
            [lead]
            id = "lead"
            directory = "/tmp"
            prompt = "p"
            allow_subleads = false
            max_workers = 4
            budget_usd = 5.0
        "#);
        let r = resolve(m, Some(64)).unwrap();
        assert_eq!(r.max_parallel_tasks, None);
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
        assert_eq!(r.max_parallel_tasks, Some(16));
    }

    #[test]
    fn manifest_max_parallel_tasks_wins_over_env() {
        let m = man(r#"
            [run]
            max_parallel_tasks = 2
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, Some(16)).unwrap();
        assert_eq!(r.max_parallel_tasks, Some(2));
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

            [lead]
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

            [lead]
            id = "triage"
            directory = "/tmp"
            prompt = "coordinate"
            model = "claude-sonnet-4-6"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.lead.unwrap().model, "claude-sonnet-4-6");
    }

    #[test]
    fn lead_timeout_falls_back_to_lead_lead_timeout_secs() {
        let m = man(r#"
            [lead]
            id = "triage"
            directory = "/tmp"
            prompt = "p"
            lead_timeout_secs = 7200
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.lead.unwrap().timeout_secs, 7200);
    }

    #[test]
    fn resolves_default_approval_policy_from_run() {
        let m = man(r#"
            [run]
            default_approval_policy = "auto_reject"

            [lead]
            id = "triage"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(
            r.default_approval_policy,
            Some(crate::dispatch::state::ApprovalPolicy::AutoReject)
        );
    }

    #[test]
    fn resolves_missing_default_approval_policy_as_none() {
        let m = man(r#"
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, None).unwrap();
        assert!(r.default_approval_policy.is_none());
    }

    #[test]
    #[serial(env)]
    fn resolves_notifications_with_env_substitution() {
        // The substitution prefix is `PITBOSS_NOTIFY_` (#156 M3) — narrower
        // than the previous `PITBOSS_` so a manifest can't sneak
        // `${PITBOSS_RUN_ID}` etc. into a webhook URL.
        std::env::set_var("PITBOSS_NOTIFY_TEST_WEBHOOK", "https://h.example/x");
        let m = man(r#"
[[notification]]
kind = "webhook"
url  = "${PITBOSS_NOTIFY_TEST_WEBHOOK}"
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

    /// TOML round-trip for `[[approval_policy]]` blocks: verifies that the
    /// `#[serde(rename = "match")]` annotation on `match_clause` is correct,
    /// that the action and match fields are parsed, and that the resolved
    /// `ResolvedManifest.approval_rules` has the expected content.
    #[test]
    fn toml_approval_policy_round_trips_through_resolve() {
        use crate::mcp::approval::ApprovalCategory;
        use crate::mcp::policy::ApprovalAction;

        let m = man(r#"
[run]
max_parallel_tasks = 4

[lead]
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

    /// Every fresh `resolve()` must stamp the snapshot with the current
    /// schema version so the resume loader can later reject future
    /// snapshots produced by a newer pitboss with a typed
    /// `IncompatibleVersion` error rather than an opaque serde failure.
    #[test]
    fn resolve_stamps_current_schema_version() {
        let m = man(r#"
            [defaults]
            model = "claude-sonnet-4-6"

            [[task]]
            id = "t1"
            directory = "/tmp"
            prompt = "x"
            "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(
            r.manifest_schema_version, CURRENT_MANIFEST_SCHEMA_VERSION,
            "resolve() must stamp manifest_schema_version with CURRENT"
        );
    }

    /// Regression test for serde alias coverage on every renamed field
    /// of `ResolvedManifest` / `ResolvedLead`. If a future rename drops
    /// the corresponding `#[serde(alias = "<old name>")]`, this test
    /// fails loudly — the contributor must either restore the alias
    /// (preferred) or remove the legacy key from the fixture below AND
    /// bump `CURRENT_MANIFEST_SCHEMA_VERSION` so resume rejects pre-rename
    /// snapshots with a typed error rather than silently succeeding with
    /// a default.
    ///
    /// Each row in the fixture exercises one historic JSON key. When you
    /// rename a field, add the old key here.
    #[test]
    fn resolved_manifest_accepts_legacy_serde_aliases() {
        // Pre-v0.9 snapshot using ALL legacy field names. No
        // `manifest_schema_version` field — defaults to 0 (legacy era).
        let legacy = serde_json::json!({
            "max_parallel": 7,                          // → max_parallel_tasks
            "halt_on_failure": true,
            "run_dir": "/tmp/runs",
            "worktree_cleanup": "on_success",
            "emit_event_stream": false,
            "tasks": [],
            "approval_policy": null,                    // → default_approval_policy
            "lead": {
                "id": "root",
                "directory": "/tmp",
                "prompt": "go",
                "branch": null,
                "model": "claude-sonnet-4-6",
                "effort": "high",
                "tools": [],
                "timeout_secs": 1800,
                "use_worktree": false,
                "env": {},
                "max_workers_across_tree": 12           // → max_total_workers
            }
        });

        let r: ResolvedManifest =
            serde_json::from_value(legacy).expect("legacy snapshot must deserialize via aliases");

        assert_eq!(
            r.manifest_schema_version, 0,
            "snapshot without the field must default to 0 (legacy)"
        );
        assert_eq!(
            r.max_parallel_tasks,
            Some(7),
            "alias `max_parallel` must populate max_parallel_tasks"
        );
        assert!(
            r.default_approval_policy.is_none(),
            "alias `approval_policy` must populate default_approval_policy"
        );
        let lead = r.lead.as_ref().expect("lead deserialized");
        assert_eq!(
            lead.max_total_workers,
            Some(12),
            "alias `max_workers_across_tree` must populate max_total_workers"
        );
    }

    /// Regression test for `#[serde(default)]` coverage on the
    /// `Option<T>` fields flagged by F-PROTO-8 (#521). Today serde
    /// already deserializes a missing `Option<T>` as `None` regardless
    /// of `#[serde(default)]`, so this test passes on the pre-fix code
    /// too. The attribute matters as defense-in-depth against a future
    /// `Option<T>` → `T` type change: without `default`, the same
    /// missing-field snapshot would start failing with a typed serde
    /// error rather than silently resuming. If you change any of these
    /// fields off `Option`, this test must keep passing — restore the
    /// default (`#[serde(default = "fn")]` with an explicit fallback)
    /// or bump `CURRENT_MANIFEST_SCHEMA_VERSION` so resume rejects the
    /// snapshot rather than mis-defaulting.
    #[test]
    fn resolved_manifest_accepts_missing_optional_top_level_fields() {
        // Sparse snapshot: only fields that are genuinely required
        // (no `Default` impl, no `#[serde(default)]`) are present.
        // Everything else — including `manifest_schema_version`, which
        // has its own `default_legacy_manifest_schema_version` — is
        // omitted to verify defaulting end-to-end.
        let sparse = serde_json::json!({
            "halt_on_failure": false,
            "run_dir": "/tmp/runs",
            "worktree_cleanup": "on_success",
            "emit_event_stream": false,
            "tasks": [],
        });

        let r: ResolvedManifest = serde_json::from_value(sparse)
            .expect("snapshot missing optional fields must deserialize");

        // Originally-named fields (F-PROTO-8 / #521).
        assert!(
            r.max_parallel_tasks.is_none(),
            "missing `max_parallel_tasks` must default to None"
        );
        assert!(
            r.default_approval_policy.is_none(),
            "missing `default_approval_policy` must default to None"
        );
        // Sibling Option<T> fields hardened in the same PR.
        assert!(r.lead.is_none(), "missing `lead` must default to None");
        assert!(
            r.max_workers.is_none(),
            "missing `max_workers` must default to None"
        );
        assert!(
            r.budget_usd.is_none(),
            "missing `budget_usd` must default to None"
        );
        assert!(
            r.lead_timeout_secs.is_none(),
            "missing `lead_timeout_secs` must default to None"
        );
        // `manifest_schema_version` carries its own `default = "fn"`;
        // confirm it falls back to the legacy-era sentinel rather than
        // failing deserialization.
        assert_eq!(
            r.manifest_schema_version, 0,
            "missing `manifest_schema_version` must default to 0 (legacy era)"
        );
    }

    /// Regression test for `#[serde(default)]` coverage on
    /// `ResolvedSubleadDefaults` (the nested type embedded inside
    /// `ResolvedLead.sublead_defaults`). Same forward-compat hazard as
    /// the parent struct: `lead_budget_usd` already had `default`, the
    /// other three `Option<T>` fields did not. Same fix logic applies.
    #[test]
    fn resolved_sublead_defaults_accepts_missing_optional_fields() {
        let sparse = serde_json::json!({
            "read_down": false,
        });
        let d: ResolvedSubleadDefaults =
            serde_json::from_value(sparse).expect("sparse sublead_defaults must deserialize");
        assert!(d.budget_usd.is_none());
        assert!(d.lead_budget_usd.is_none());
        assert!(d.max_workers.is_none());
        assert!(d.lead_timeout_secs.is_none());
    }
}
