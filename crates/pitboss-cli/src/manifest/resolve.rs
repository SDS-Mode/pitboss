#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use pitboss_core::provider::Provider;
use serde::{Deserialize, Serialize};

use super::schema::{
    ContainerConfig, Defaults, Effort, Lead, Manifest, SubleadDefaults, Task, Template,
    WorktreeCleanup,
};

/// Fully resolved task ready for dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedTask {
    pub id: String,
    pub directory: PathBuf,
    pub prompt: String,
    pub branch: Option<String>,
    #[serde(default)]
    pub provider: Provider,
    pub model: String,
    #[serde(default)]
    pub goose_max_turns: Option<u32>,
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
    #[serde(default)]
    pub provider: Provider,
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
    pub provider: Provider,
    pub budget_usd: Option<f64>,
    pub max_workers: Option<u32>,
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
    /// Renamed from `max_parallel` in v0.9 to match the TOML field name.
    /// `alias` keeps pre-v0.9 `resolved.json` snapshots resumable.
    #[serde(alias = "max_parallel")]
    pub max_parallel_tasks: u32,
    pub halt_on_failure: bool,
    pub run_dir: PathBuf,
    pub worktree_cleanup: WorktreeCleanup,
    pub emit_event_stream: bool,
    pub tasks: Vec<ResolvedTask>,
    pub lead: Option<ResolvedLead>,
    /// Surfaced from `[lead].max_workers` for consumer convenience.
    /// `None` in flat mode.
    pub max_workers: Option<u32>,
    /// Surfaced from `[lead].budget_usd`. `None` in flat mode.
    pub budget_usd: Option<f64>,
    /// Surfaced from `[lead].lead_timeout_secs`. `None` in flat mode.
    pub lead_timeout_secs: Option<u64>,
    /// Renamed from `approval_policy` in v0.9 to match the TOML field name
    /// and disambiguate from `approval_rules`.
    /// `alias` keeps pre-v0.9 `resolved.json` snapshots resumable.
    #[serde(alias = "approval_policy")]
    pub default_approval_policy: Option<crate::dispatch::state::ApprovalPolicy>,
    #[serde(default)]
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
    /// Resolved `[lifecycle]` section. `None` when the manifest omits it
    /// entirely (the common case — pitboss's default semantics apply: dies
    /// with parent, no out-of-band lifecycle notify expected).
    #[serde(default)]
    pub lifecycle: Option<crate::manifest::schema::Lifecycle>,
}

const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_PROVIDER: Provider = Provider::Anthropic;
const DEFAULT_EFFORT: Effort = Effort::High;
const DEFAULT_TIMEOUT_SECS: u64 = 3600;
use super::schema::DEFAULT_MAX_PARALLEL_TASKS;
fn default_tools() -> Vec<String> {
    ["Read", "Write", "Edit", "Bash", "Glob", "Grep"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn parse_provider(raw: Option<&str>) -> Result<Option<Provider>> {
    raw.map(str::parse)
        .transpose()
        .map_err(|e: String| anyhow!(e))
}

fn split_provider_model(model: &str) -> Option<(Provider, String)> {
    let (provider, model) = model.split_once('/')?;
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    let provider = provider.parse().ok()?;
    Some((provider, model.to_string()))
}

fn resolve_provider_and_model(
    actor_provider: Option<&str>,
    actor_model: Option<&str>,
    defaults: &Defaults,
    goose: &crate::manifest::schema::GooseConfig,
    context: &str,
) -> Result<(Provider, String)> {
    let default_provider = parse_provider(defaults.provider.as_deref())
        .context("[defaults].provider")?
        .or(parse_provider(goose.default_provider.as_deref())
            .context("[goose].default_provider")?)
        .unwrap_or(DEFAULT_PROVIDER);
    let explicit_actor_provider =
        parse_provider(actor_provider).with_context(|| format!("{context}.provider"))?;

    let raw_model = actor_model
        .map(str::to_string)
        .or_else(|| defaults.model.clone());
    let model_was_supplied = raw_model.is_some();

    let (model_provider, model) = match raw_model {
        Some(model) => match split_provider_model(&model) {
            Some((provider, stripped)) => (Some(provider), stripped),
            None => (None, model),
        },
        None => (None, DEFAULT_MODEL.to_string()),
    };

    let provider = explicit_actor_provider
        .or(model_provider)
        .unwrap_or(default_provider);
    if !model_was_supplied && provider != DEFAULT_PROVIDER {
        bail!(
            "{context} resolves provider '{}' but no model; set model explicitly for non-default providers",
            provider.goose_arg()
        );
    }
    Ok((provider, model))
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

    let mut resolved_tasks = Vec::with_capacity(manifest.tasks.len());
    for task in &manifest.tasks {
        resolved_tasks.push(resolve_task(
            task,
            &manifest.defaults,
            &manifest.goose,
            &templates,
        )?);
    }

    let resolved_sublead_defaults = manifest
        .sublead_defaults
        .as_ref()
        .map(|spec| resolve_sublead_defaults(spec, &manifest.defaults, &manifest.goose))
        .transpose()?;

    let resolved_lead = if let Some(l) = &manifest.lead {
        Some(resolve_lead(
            l,
            &manifest.defaults,
            &manifest.goose,
            resolved_sublead_defaults.clone(),
        )?)
    } else {
        None
    };

    let max_parallel_tasks = manifest
        .run
        .max_parallel_tasks
        .or(env_max_parallel_tasks)
        .unwrap_or(DEFAULT_MAX_PARALLEL_TASKS);

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
    let (max_workers, budget_usd, lead_timeout_secs) = match manifest.lead.as_ref() {
        Some(l) => (l.max_workers, l.budget_usd, l.lead_timeout_secs),
        None => (None, None, None),
    };

    Ok(ResolvedManifest {
        manifest_schema_version: CURRENT_MANIFEST_SCHEMA_VERSION,
        name: manifest.run.name.clone(),
        max_parallel_tasks,
        halt_on_failure: manifest.run.halt_on_failure,
        run_dir,
        worktree_cleanup: manifest.run.worktree_cleanup,
        emit_event_stream: manifest.run.emit_event_stream,
        tasks: resolved_tasks,
        lead: resolved_lead,
        max_workers,
        budget_usd,
        lead_timeout_secs,
        default_approval_policy: manifest.run.default_approval_policy,
        notifications,
        dump_shared_store: manifest.run.dump_shared_store,
        require_plan_approval: manifest.run.require_plan_approval,
        approval_rules,
        container: manifest.container,
        mcp_servers: manifest.mcp_servers,
        lifecycle: manifest.lifecycle,
    })
}

fn resolve_lead(
    lead: &Lead,
    defaults: &Defaults,
    goose: &crate::manifest::schema::GooseConfig,
    sublead_defaults: Option<ResolvedSubleadDefaults>,
) -> Result<ResolvedLead> {
    let mut env = defaults.env.clone();
    env.extend(lead.env.clone());

    // Lead timeout cascade: per-lead timeout_secs > lead.lead_timeout_secs
    // > defaults.timeout_secs > 3600.
    let timeout_secs = lead
        .timeout_secs
        .or(lead.lead_timeout_secs)
        .or(defaults.timeout_secs)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    let (provider, model) = resolve_provider_and_model(
        lead.provider.as_deref(),
        lead.model.as_deref(),
        defaults,
        goose,
        "[lead]",
    )?;

    Ok(ResolvedLead {
        id: lead.id.clone(),
        directory: lead.directory.clone(),
        prompt: lead.prompt.clone(),
        branch: lead.branch.clone(),
        provider,
        model,
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
    goose: &crate::manifest::schema::GooseConfig,
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

    let context = format!("[[task]] id='{}'", task.id);
    let (provider, model) = resolve_provider_and_model(
        task.provider.as_deref(),
        task.model.as_deref(),
        defaults,
        goose,
        &context,
    )?;

    Ok(ResolvedTask {
        id: task.id.clone(),
        directory: task.directory.clone(),
        prompt,
        branch: task.branch.clone(),
        provider,
        model,
        goose_max_turns: goose.default_max_turns,
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

/// Convert a `SubleadDefaults` (TOML deserialized) into a `ResolvedSubleadDefaults`.
fn resolve_sublead_defaults(
    spec: &SubleadDefaults,
    defaults: &Defaults,
    goose: &crate::manifest::schema::GooseConfig,
) -> Result<ResolvedSubleadDefaults> {
    let provider = parse_provider(spec.provider.as_deref())
        .context("[sublead_defaults].provider")?
        .or(parse_provider(defaults.provider.as_deref()).context("[defaults].provider")?)
        .or(parse_provider(goose.default_provider.as_deref())
            .context("[goose].default_provider")?)
        .unwrap_or(DEFAULT_PROVIDER);

    Ok(ResolvedSubleadDefaults {
        provider,
        budget_usd: spec.budget_usd,
        max_workers: spec.max_workers,
        lead_timeout_secs: spec.lead_timeout_secs,
        read_down: spec.read_down,
    })
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
        assert_eq!(r.max_parallel_tasks, 4);
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
    fn task_provider_resolves_from_defaults() {
        let m = man(r#"
            [defaults]
            provider = "google"
            model = "gemini-2.5-flash"

            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.tasks[0].provider, Provider::Google);
        assert_eq!(r.tasks[0].model, "gemini-2.5-flash");
    }

    #[test]
    fn task_model_short_form_seeds_provider() {
        let m = man(r#"
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
            model = "ollama/llama3.1"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.tasks[0].provider, Provider::Ollama);
        assert_eq!(r.tasks[0].model, "llama3.1");
    }

    #[test]
    fn goose_default_max_turns_resolves_to_tasks() {
        let m = man(r#"
            [goose]
            default_max_turns = 3

            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.tasks[0].goose_max_turns, Some(3));
    }

    #[test]
    fn non_default_provider_requires_model() {
        let m = man(r#"
            [[task]]
            id = "a"
            directory = "/tmp"
            prompt = "p"
            provider = "google"
        "#);
        let err = resolve(m, None).unwrap_err();
        assert!(
            err.to_string().contains("no model"),
            "expected no-model error, got: {err:#}"
        );
    }

    #[test]
    fn lead_provider_and_short_model_resolve() {
        let m = man(r#"
            [lead]
            id = "lead"
            directory = "/tmp"
            prompt = "p"
            model = "openai/gpt-4o"
        "#);
        let r = resolve(m, None).unwrap();
        let lead = r.lead.unwrap();
        assert_eq!(lead.provider, Provider::OpenAi);
        assert_eq!(lead.model, "gpt-4o");
    }

    #[test]
    fn lead_non_default_provider_requires_model() {
        let m = man(r#"
            [lead]
            id = "lead"
            directory = "/tmp"
            prompt = "p"
            provider = "google"
        "#);
        let err = resolve(m, None).unwrap_err();
        assert!(
            err.to_string().contains("no model"),
            "expected no-model error, got: {err:#}"
        );
    }

    #[test]
    fn sublead_defaults_provider_resolves_from_defaults() {
        let m = man(r#"
            [defaults]
            provider = "google"
            model = "gemini-2.5-flash"

            [lead]
            id = "lead"
            directory = "/tmp"
            prompt = "p"
            allow_subleads = true
            max_workers = 2
            max_subleads = 1
            max_total_workers = 2

            [sublead_defaults]
            read_down = true
        "#);
        let r = resolve(m, None).unwrap();
        let sublead_defaults = r.lead.unwrap().sublead_defaults.unwrap();
        assert_eq!(sublead_defaults.provider, Provider::Google);
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
        assert_eq!(r.max_parallel_tasks, 16);
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
        assert_eq!(r.max_parallel_tasks, 2);
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
            r.max_parallel_tasks, 7,
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
}
