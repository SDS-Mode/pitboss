#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default)]
    pub run: RunConfig,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default, rename = "template")]
    pub templates: Vec<Template>,
    #[serde(default, rename = "task")]
    pub tasks: Vec<Task>,
    #[serde(default, rename = "lead")]
    pub leads: Vec<Lead>,
    /// Notification sinks (v0.4.1+). Parsed as [[notification]] sections.
    #[serde(default, rename = "notification")]
    pub notification: Vec<crate::notify::config::NotificationConfig>,
    /// Approval policy rules (v0.6+). Parsed as [[approval_policy]] sections.
    /// Rules are evaluated in declaration order; first match wins.
    #[serde(default, rename = "approval_policy")]
    pub approval_policy_rules: Vec<ApprovalRuleSpec>,
}

/// TOML schema for a single `[[approval_policy]]` rule.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApprovalRuleSpec {
    #[serde(default, rename = "match")]
    pub match_clause: ApprovalMatchSpec,
    /// Action to take when the rule matches.
    /// One of: "auto_approve", "auto_reject", "block".
    pub action: String,
}

/// TOML schema for the `[match]` sub-table within an `[[approval_policy]]` rule.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ApprovalMatchSpec {
    pub actor: Option<String>,
    pub category: Option<String>,
    pub tool_name: Option<String>,
    pub cost_over: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    pub max_parallel: Option<u32>,
    #[serde(default)]
    pub halt_on_failure: bool,
    pub run_dir: Option<PathBuf>,
    #[serde(default = "default_cleanup")]
    pub worktree_cleanup: WorktreeCleanup,
    #[serde(default)]
    pub emit_event_stream: bool,

    // NEW in v0.3 — only meaningful when [[lead]] is present.
    #[serde(default)]
    pub max_workers: Option<u32>,
    #[serde(default)]
    pub budget_usd: Option<f64>,
    #[serde(default)]
    pub lead_timeout_secs: Option<u64>,

    // NEW in v0.4 — approval policy for when no TUI is attached.
    #[serde(default)]
    pub approval_policy: Option<crate::dispatch::state::ApprovalPolicy>,

    // NEW in v0.4.2 — write shared-store.json on finalize for post-mortem.
    #[serde(default)]
    pub dump_shared_store: bool,

    // NEW in v0.4.5 — when true, the lead must call `propose_plan` and have
    // the resulting plan approved before any `spawn_worker` call succeeds.
    // Default off so existing runs behave unchanged.
    #[serde(default)]
    pub require_plan_approval: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            max_parallel: None,
            halt_on_failure: false,
            run_dir: None,
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            max_workers: None,
            budget_usd: None,
            lead_timeout_secs: None,
            approval_policy: None,
            dump_shared_store: false,
            require_plan_approval: false,
        }
    }
}

fn default_cleanup() -> WorktreeCleanup {
    WorktreeCleanup::OnSuccess
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCleanup {
    Always,
    OnSuccess,
    Never,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub tools: Option<Vec<String>>,
    pub timeout_secs: Option<u64>,
    pub use_worktree: Option<bool>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub directory: PathBuf,
    pub prompt: Option<String>,
    pub template: Option<String>,
    #[serde(default)]
    pub vars: HashMap<String, String>,
    pub branch: Option<String>,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub tools: Option<Vec<String>>,
    pub timeout_secs: Option<u64>,
    pub use_worktree: Option<bool>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lead {
    pub id: String,
    pub directory: PathBuf,
    pub prompt: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub use_worktree: Option<bool>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// v0.6 single-table manifest variant. Used by `load_manifest_from_str` when
/// the TOML author writes `[lead]` (one table) instead of `[[lead]]` (array).
/// Carries all per-run settings under `[run]` and per-lead settings (including
/// depth-2 caps) under `[lead]`.
#[derive(Debug, Clone, Deserialize)]
pub struct SingleLeadManifest {
    #[serde(default)]
    pub run: RunConfig,
    /// Single-table `[lead]` block.
    pub lead: Option<LeadSpec>,
    /// Notification sinks (v0.4.1+). Parsed as [[notification]] sections.
    #[serde(default, rename = "notification")]
    pub notification: Vec<crate::notify::config::NotificationConfig>,
    /// Approval policy rules (v0.6+).
    #[serde(default, rename = "approval_policy")]
    pub approval_policy_rules: Vec<ApprovalRuleSpec>,
}

/// v0.6 single-table `[lead]` schema. Used when the manifest author writes
/// `[lead]` (one table) rather than `[[lead]]` (array). Carries the new
/// depth-2 cap fields in addition to the v0.5 per-lead fields.
///
/// Defaults to a "no-op" state so existing manifests without this block
/// continue to work identically.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct LeadSpec {
    pub prompt: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub use_worktree: Option<bool>,
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// v0.6: when true, root lead's MCP toolset includes `spawn_sublead`.
    /// Defaults to false for v0.5 backward compatibility.
    #[serde(default)]
    pub allow_subleads: bool,

    /// v0.6: hard cap on total live sub-leads under this root.
    #[serde(default)]
    pub max_subleads: Option<u32>,

    /// v0.6: hard cap on per-sub-lead budget envelope (USD).
    #[serde(default)]
    pub max_sublead_budget_usd: Option<f64>,

    /// v0.6: hard cap on total live workers across the entire tree
    /// (sum of root-level workers + all sub-tree workers).
    #[serde(default)]
    pub max_workers_across_tree: Option<u32>,

    /// v0.6: optional defaults applied when `spawn_sublead` omits a param.
    #[serde(default)]
    pub sublead_defaults: Option<SubleadDefaultsSpec>,
}

/// v0.6: optional `[lead.sublead_defaults]` block that supplies fallback values
/// for `spawn_sublead` when the caller omits them.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SubleadDefaultsSpec {
    pub budget_usd: Option<f64>,
    pub max_workers: Option<u32>,
    pub lead_timeout_secs: Option<u64>,
    #[serde(default)]
    pub read_down: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_top_level_key() {
        let toml_src = r#"
            wibble = "surprise"
            [[task]]
            id = "x"
            directory = "/tmp"
            prompt = "p"
        "#;
        let err: Result<Manifest, _> = toml::from_str(toml_src);
        assert!(err.is_err(), "should reject unknown key");
    }

    #[test]
    fn accepts_minimal_manifest() {
        let toml_src = r#"
            [[task]]
            id = "x"
            directory = "/tmp"
            prompt = "hi"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.tasks.len(), 1);
        assert_eq!(m.tasks[0].id, "x");
    }

    #[test]
    fn parses_full_manifest_with_template() {
        let toml_src = r#"
            [run]
            max_parallel = 8
            halt_on_failure = true
            worktree_cleanup = "never"

            [defaults]
            model = "claude-sonnet-4-6"
            effort = "high"
            tools = ["Read", "Bash"]

            [[template]]
            id = "sweep"
            prompt = "Audit {pm} in {dir}"

            [[task]]
            id = "t1"
            directory = "/tmp"
            template = "sweep"
            vars = { pm = "npm", dir = "/tmp" }
            branch = "feat/x"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.run.max_parallel, Some(8));
        assert!(m.run.halt_on_failure);
        assert_eq!(m.templates.len(), 1);
        assert_eq!(m.tasks[0].template.as_deref(), Some("sweep"));
    }

    #[test]
    fn parses_lead_section() {
        let toml_src = r#"
            [run]
            max_workers = 4
            budget_usd = 5.00
            lead_timeout_secs = 1200

            [[lead]]
            id = "triage"
            directory = "/tmp"
            prompt = "coordinate the triage"
            branch = "feat/triage"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.run.max_workers, Some(4));
        assert_eq!(m.run.budget_usd, Some(5.00));
        assert_eq!(m.run.lead_timeout_secs, Some(1200));
        assert_eq!(m.leads.len(), 1);
        assert_eq!(m.leads[0].id, "triage");
        assert_eq!(m.leads[0].branch.as_deref(), Some("feat/triage"));
    }

    #[test]
    fn rejects_unknown_lead_field() {
        let toml_src = r#"
            [[lead]]
            id = "x"
            directory = "/tmp"
            prompt = "p"
            wibble = "surprise"
        "#;
        let err: Result<Manifest, _> = toml::from_str(toml_src);
        assert!(err.is_err());
    }

    #[test]
    fn parses_run_fields_without_lead_section() {
        // These fields parse fine on their own; validation rejects them later
        // when no [[lead]] is present.
        let toml_src = r#"
            [run]
            max_workers = 2
            budget_usd = 1.00

            [[task]]
            id = "x"
            directory = "/tmp"
            prompt = "p"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.run.max_workers, Some(2));
    }

    #[test]
    fn parses_approval_policy() {
        let toml_src = r#"
            [run]
            approval_policy = "auto_approve"

            [[lead]]
            id = "triage"
            directory = "/tmp"
            prompt = "p"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(
            m.run.approval_policy,
            Some(crate::dispatch::state::ApprovalPolicy::AutoApprove)
        );
    }

    #[test]
    fn parses_require_plan_approval() {
        let toml_src = r#"
            [run]
            require_plan_approval = true

            [[lead]]
            id = "triage"
            directory = "/tmp"
            prompt = "p"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert!(m.run.require_plan_approval);
    }

    #[test]
    fn require_plan_approval_defaults_false() {
        let toml_src = r#"
            [run]

            [[task]]
            id = "x"
            directory = "/tmp"
            prompt = "p"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert!(!m.run.require_plan_approval);
    }

    #[test]
    fn rejects_unknown_approval_policy() {
        let toml_src = r#"
            [run]
            approval_policy = "wibble"

            [[lead]]
            id = "triage"
            directory = "/tmp"
            prompt = "p"
        "#;
        let err: Result<Manifest, _> = toml::from_str(toml_src);
        assert!(err.is_err());
    }

    #[test]
    fn parses_notification_section() {
        let toml_src = r#"
[[notification]]
kind = "webhook"
url  = "https://example.com/hook"
events = ["run_finished"]
severity_min = "info"

[[task]]
id = "t"
directory = "/tmp"
prompt = "p"
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.notification.len(), 1);
        assert_eq!(m.notification[0].events.as_ref().unwrap().len(), 1);
    }
}
