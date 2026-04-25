#![allow(dead_code)]

//! Manifest TOML schema (v0.9 redesign).
//!
//! ## Overview
//!
//! A pitboss manifest is a single TOML file describing one dispatch run. The
//! v0.9 schema collapses the previous dual-form layout (`[[lead]]` array vs
//! `[lead]` single-table) into a single canonical shape and relocates fields
//! to where they belong semantically. Pre-v1; older manifests must be migrated.
//!
//! ## Section reference (one source of truth)
//!
//! | TOML path                 | Type                  | Required | Notes                                                 |
//! |---------------------------|-----------------------|----------|-------------------------------------------------------|
//! | `[run]`                   | `RunConfig`           | no       | Run-wide infrastructure config                        |
//! | `[defaults]`              | `Defaults`            | no       | Per-actor inheritable knobs (model, tools, env, ...)  |
//! | `[[task]]`                | `Task`                | flat-mode | One-or-more tasks; mutually exclusive with `[lead]`  |
//! | `[lead]`                  | `Lead`                | hier-mode | Exactly one root lead; mutually exclusive with task  |
//! | `[sublead_defaults]`      | `SubleadDefaults`     | no       | Defaults applied to `spawn_sublead` calls            |
//! | `[container]`             | `ContainerConfig`     | no       | Enables `pitboss container-dispatch`                  |
//! | `[[container.mount]]`     | `MountSpec`           | no       | Bind mounts (when `[container]` set)                  |
//! | `[[mcp_server]]`          | `McpServerSpec`       | no       | External MCP servers injected into all actors        |
//! | `[[notification]]`        | `NotificationConfig`  | no       | Notification sinks                                   |
//! | `[[approval_policy]]`     | `ApprovalRuleSpec`    | no       | Declarative approval rules (matched in order)        |
//! | `[[template]]`            | `Template`            | no       | Prompt templates referenced by `[[task]]`            |
//!
//! ## Migration from v0.8 → v0.9
//!
//! - `[[lead]]` (array form) → `[lead]` (single-table; no array)
//! - `[run].max_workers` → `[lead].max_workers`
//! - `[run].budget_usd` → `[lead].budget_usd`
//! - `[run].lead_timeout_secs` → `[lead].lead_timeout_secs`
//! - `[run].max_parallel` → `[run].max_parallel_tasks`
//! - `[run].approval_policy` → `[run].default_approval_policy`
//! - `[lead].max_workers_across_tree` → `[lead].max_total_workers`
//! - `[lead.sublead_defaults]` → top-level `[sublead_defaults]`
//! - `[lead].id` and `[lead].directory` are now REQUIRED (no cwd default)

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How the pitboss-spawned claude handles its built-in per-tool permission gate.
///
/// **`PathA`** (default) sets `CLAUDE_CODE_ENTRYPOINT=sdk-ts`, which tells claude
/// it is running inside an SDK that manages permissions externally. Claude never
/// prompts; pitboss is the sole permission authority via its own approval queue / TUI.
///
/// **`PathB`** leaves the entrypoint unset (claude's own permission gate is active),
/// but pitboss registers a `permission_prompt` MCP tool. When claude asks for
/// permission to use a tool, it calls `permission_prompt`; pitboss routes the request
/// through its approval queue and TUI. The calling actor blocks until an operator
/// (or an `[[approval_policy]]` rule) responds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRouting {
    #[default]
    PathA,
    PathB,
}

/// Optional `[container]` section for `pitboss container-dispatch`.
/// When present, `pitboss container-dispatch` uses this config to build
/// the `docker`/`podman run` invocation. `directory` fields in tasks/lead
/// must be valid container-side paths (after mounts are applied).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ContainerConfig {
    /// Container image. Default: `ghcr.io/sds-mode/pitboss-with-claude:latest`.
    pub image: Option<String>,
    /// Container runtime: `"docker"`, `"podman"`, or `"auto"` (default).
    pub runtime: Option<String>,
    /// Extra args inserted verbatim before the image name in the `run` call.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Host→container bind mounts (besides the auto-injected `~/.claude` and run_dir).
    #[serde(default, rename = "mount")]
    pub mounts: Vec<MountSpec>,
    /// Working directory inside the container.
    /// Defaults to the container path of the first `[[container.mount]]` entry,
    /// or `/home/pitboss` if no mounts are declared.
    pub workdir: Option<PathBuf>,
}

/// A single host→container bind mount entry.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MountSpec {
    /// Absolute path on the host (tilde expansion is performed at dispatch time).
    pub host: PathBuf,
    /// Absolute path inside the container.
    pub container: PathBuf,
    /// Mount as read-only. Default: false.
    #[serde(default)]
    pub readonly: bool,
}

/// An external MCP server to inject into every actor's `--mcp-config`.
/// Declared as `[[mcp_server]]` in the manifest. All actors (lead, sub-lead,
/// and workers) receive the server so they can call its tools directly.
///
/// Per-actor scope granularity is deferred — see roadmap.
///
/// Example:
/// ```toml
/// [[mcp_server]]
/// id      = "context7"
/// command = "npx"
/// args    = ["-y", "@upstash/context7-mcp"]
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerSpec {
    /// Key name for this server in the generated `mcpServers` JSON object.
    pub id: String,
    /// Executable to launch (e.g. `"npx"`, `"uvx"`, absolute path).
    pub command: String,
    /// Arguments passed to the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables injected into the MCP server process.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Top-level manifest. One canonical shape: either flat-mode (`[[task]]`) or
/// hierarchical-mode (`[lead]`), mutually exclusive.
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
    /// Single-table `[lead]` for hierarchical mode. Exactly one is required
    /// when no `[[task]]` is declared. The v0.8 `[[lead]]` array form is gone.
    #[serde(default)]
    pub lead: Option<Lead>,
    /// Top-level `[sublead_defaults]` (promoted from the v0.8
    /// `[lead.sublead_defaults]` subtable). Applied to `spawn_sublead`
    /// calls that omit the corresponding fields.
    #[serde(default)]
    pub sublead_defaults: Option<SubleadDefaults>,
    /// Notification sinks. Parsed as `[[notification]]` sections.
    #[serde(default, rename = "notification")]
    pub notification: Vec<crate::notify::config::NotificationConfig>,
    /// Approval policy rules. Parsed as `[[approval_policy]]` sections.
    /// Rules are evaluated in declaration order; first match wins.
    #[serde(default, rename = "approval_policy")]
    pub approval_policy_rules: Vec<ApprovalRuleSpec>,
    /// Optional container config for `pitboss container-dispatch`.
    #[serde(default)]
    pub container: Option<ContainerConfig>,
    /// External MCP servers injected into all actor configs.
    #[serde(default, rename = "mcp_server")]
    pub mcp_servers: Vec<McpServerSpec>,
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

/// Run-wide infrastructure config (NOT lead-specific).
///
/// Lead-specific caps moved to `[lead]` in v0.9 (`max_workers`, `budget_usd`,
/// `lead_timeout_secs`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    /// Concurrency cap for `[[task]]` flat mode. Renamed from `max_parallel`
    /// in v0.9. Overridden by `ANTHROPIC_MAX_CONCURRENT` env var. Default: 4.
    pub max_parallel_tasks: Option<u32>,
    /// Stop flat-mode runs on first failure. Ignored in hierarchical mode.
    #[serde(default)]
    pub halt_on_failure: bool,
    /// Where run artifacts land. Default `~/.local/share/pitboss/runs`.
    pub run_dir: Option<PathBuf>,
    /// Worktree-cleanup policy. Default: `on_success`.
    #[serde(default = "default_cleanup")]
    pub worktree_cleanup: WorktreeCleanup,
    /// Write an event-stream JSONL alongside `summary.jsonl`. Default off.
    #[serde(default)]
    pub emit_event_stream: bool,
    /// Default approval-policy action when no TUI is attached and no
    /// `[[approval_policy]]` rule matches. Renamed from `approval_policy`
    /// in v0.9 to disambiguate from the rules array. One of:
    /// `"block"` (default), `"auto_approve"`, `"auto_reject"`.
    #[serde(default)]
    pub default_approval_policy: Option<crate::dispatch::state::ApprovalPolicy>,
    /// Dump the shared store (`/ref/*`, `/peer/*`, `/shared/*`, `/leases/*`)
    /// to `<run-dir>/shared-store.json` on finalize.
    #[serde(default)]
    pub dump_shared_store: bool,
    /// When true, the lead must call `propose_plan` and have the resulting
    /// plan approved before any `spawn_worker` succeeds.
    #[serde(default)]
    pub require_plan_approval: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            max_parallel_tasks: None,
            halt_on_failure: false,
            run_dir: None,
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            default_approval_policy: None,
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

/// Single canonical lead shape (v0.9). Replaces the v0.8 `[[lead]]`/`[lead]` split.
///
/// Lead-level caps that previously lived under `[run]` (`max_workers`,
/// `budget_usd`, `lead_timeout_secs`) live here in v0.9 — they're properties
/// of the lead, not the run.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lead {
    /// Unique slug for the lead (used as the TUI tile label and in run
    /// artifact paths). Required in v0.9 — no cwd-derived default.
    pub id: String,
    /// Working directory for the lead's claude subprocess. Required in v0.9
    /// — no cwd-derived default. Tilde expansion is performed at load time.
    pub directory: PathBuf,
    /// Operator prompt for the lead. Required.
    ///
    /// **Important:** in TOML, `prompt =` MUST appear before any subtable
    /// declaration. A `prompt =` placed after a subtable header is silently
    /// reassigned to that subtable's scope; `pitboss validate` catches the
    /// resulting empty prompt and reports it.
    pub prompt: String,

    /// Branch name for the lead's worktree. Auto-generated if omitted.
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

    // ── Lead-level caps (moved from [run] in v0.9) ───────────────────────
    /// Hard cap on the lead's concurrent + queued worker pool (1–16).
    #[serde(default)]
    pub max_workers: Option<u32>,
    /// Soft cap on lead's spend (USD) with reservation accounting.
    /// `spawn_worker` fails with `budget exceeded` once
    /// `spent + reserved + next_estimate > budget`.
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// Wall-clock cap on the lead session (seconds). Distinct from
    /// `timeout_secs` (which becomes the claude `--timeout` flag for
    /// per-actor subprocess wall-clock). Default 3600 if unset.
    #[serde(default)]
    pub lead_timeout_secs: Option<u64>,

    // ── v0.8 permission routing ──────────────────────────────────────────
    /// `"path_a"` (default): `CLAUDE_CODE_ENTRYPOINT=sdk-ts` bypasses claude's
    /// built-in gate; pitboss is sole authority via its approval queue.
    /// `"path_b"`: pitboss registers a `permission_prompt` MCP tool;
    /// claude routes each permission check through it.
    #[serde(default)]
    pub permission_routing: PermissionRouting,

    // ── v0.6 depth-2 controls ────────────────────────────────────────────
    /// When true, `spawn_sublead` is included in the lead's MCP toolset.
    #[serde(default)]
    pub allow_subleads: bool,
    /// Hard cap on total live sub-leads under this root.
    #[serde(default)]
    pub max_subleads: Option<u32>,
    /// Hard cap on per-sub-lead budget envelope (USD).
    #[serde(default)]
    pub max_sublead_budget_usd: Option<f64>,
    /// Hard cap on total live workers across the entire tree (root + sub-trees).
    /// Renamed from `max_workers_across_tree` in v0.9.
    #[serde(default)]
    pub max_total_workers: Option<u32>,
}

/// Top-level `[sublead_defaults]` block (promoted from `[lead.sublead_defaults]`
/// in v0.9). Supplies fallback values for `spawn_sublead` calls that omit them.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SubleadDefaults {
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
            max_parallel_tasks = 8
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
        assert_eq!(m.run.max_parallel_tasks, Some(8));
        assert!(m.run.halt_on_failure);
        assert_eq!(m.templates.len(), 1);
        assert_eq!(m.tasks[0].template.as_deref(), Some("sweep"));
    }

    #[test]
    fn parses_lead_section_with_caps_on_lead() {
        let toml_src = r#"
            [lead]
            id = "triage"
            directory = "/tmp"
            prompt = "coordinate the triage"
            branch = "feat/triage"
            max_workers = 4
            budget_usd = 5.00
            lead_timeout_secs = 1200
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        let lead = m.lead.unwrap();
        assert_eq!(lead.id, "triage");
        assert_eq!(lead.max_workers, Some(4));
        assert_eq!(lead.budget_usd, Some(5.00));
        assert_eq!(lead.lead_timeout_secs, Some(1200));
        assert_eq!(lead.branch.as_deref(), Some("feat/triage"));
    }

    #[test]
    fn rejects_unknown_lead_field() {
        let toml_src = r#"
            [lead]
            id = "x"
            directory = "/tmp"
            prompt = "p"
            wibble = "surprise"
        "#;
        let err: Result<Manifest, _> = toml::from_str(toml_src);
        assert!(err.is_err());
    }

    #[test]
    fn rejects_legacy_array_lead_form() {
        // The v0.8 [[lead]] array form is gone. Should fail with a TOML
        // type error that validate.rs translates into a migration message.
        let toml_src = r#"
            [[lead]]
            id = "x"
            directory = "/tmp"
            prompt = "p"
        "#;
        let err: Result<Manifest, _> = toml::from_str(toml_src);
        assert!(err.is_err(), "[[lead]] array form should not parse");
    }

    #[test]
    fn rejects_legacy_run_max_workers() {
        // `[run].max_workers` moved to `[lead].max_workers` in v0.9.
        let toml_src = r#"
            [run]
            max_workers = 4
        "#;
        let err: Result<Manifest, _> = toml::from_str(toml_src);
        assert!(err.is_err(), "legacy [run].max_workers should be rejected");
    }

    #[test]
    fn parses_top_level_sublead_defaults() {
        let toml_src = r#"
            [lead]
            id = "root"
            directory = "/tmp"
            prompt = "x"
            allow_subleads = true

            [sublead_defaults]
            budget_usd = 2.00
            max_workers = 4
            lead_timeout_secs = 1800
            read_down = false
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        let sd = m.sublead_defaults.unwrap();
        assert_eq!(sd.budget_usd, Some(2.00));
        assert_eq!(sd.max_workers, Some(4));
    }

    #[test]
    fn parses_default_approval_policy() {
        let toml_src = r#"
            [run]
            default_approval_policy = "auto_approve"

            [lead]
            id = "triage"
            directory = "/tmp"
            prompt = "p"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(
            m.run.default_approval_policy,
            Some(crate::dispatch::state::ApprovalPolicy::AutoApprove)
        );
    }

    #[test]
    fn parses_require_plan_approval() {
        let toml_src = r#"
            [run]
            require_plan_approval = true

            [lead]
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
    fn rejects_unknown_default_approval_policy() {
        let toml_src = r#"
            [run]
            default_approval_policy = "wibble"

            [lead]
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
