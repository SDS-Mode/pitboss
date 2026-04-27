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

use pitboss_schema::FieldMetadata;
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
#[derive(Debug, Clone, Deserialize, Serialize, Default, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct ContainerConfig {
    /// Container image. Default: `ghcr.io/sds-mode/pitboss-with-claude:latest`.
    #[field(
        label = "Image",
        help = "Container image reference. Defaults to ghcr.io/sds-mode/pitboss-with-claude:latest."
    )]
    pub image: Option<String>,
    /// Container runtime: `"docker"`, `"podman"`, or `"auto"` (default).
    #[field(
        label = "Runtime",
        help = "Container runtime to invoke. \"auto\" prefers podman.",
        enum_values = ["docker", "podman", "auto"]
    )]
    pub runtime: Option<String>,
    /// Extra args inserted verbatim before the image name in the `run` call.
    #[serde(default)]
    #[field(
        label = "Extra args",
        help = "Args inserted verbatim before the image name in the run invocation."
    )]
    pub extra_args: Vec<String>,
    /// Host→container bind mounts (besides the auto-injected `~/.claude` and run_dir).
    #[serde(default, rename = "mount")]
    #[field(skip)]
    pub mounts: Vec<MountSpec>,
    /// Working directory inside the container.
    /// Defaults to the container path of the first `[[container.mount]]` entry,
    /// or `/home/pitboss` if no mounts are declared.
    #[field(
        label = "Working directory",
        help = "cwd inside the container; defaults to the first mount's container path."
    )]
    pub workdir: Option<PathBuf>,
}

/// A single host→container bind mount entry.
#[derive(Debug, Clone, Deserialize, Serialize, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct MountSpec {
    /// Absolute path on the host (tilde expansion is performed at dispatch time).
    #[field(label = "Host path", help = "Absolute host path. ~ is expanded.")]
    pub host: PathBuf,
    /// Absolute path inside the container.
    #[field(label = "Container path", help = "Absolute path inside the container.")]
    pub container: PathBuf,
    /// Mount as read-only. Default: false.
    #[serde(default)]
    #[field(label = "Read-only", help = "Mount read-only.")]
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
#[derive(Debug, Clone, Deserialize, Serialize, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct McpServerSpec {
    /// Key name for this server in the generated `mcpServers` JSON object.
    #[field(
        label = "Server ID",
        help = "Key under mcpServers in the generated config."
    )]
    pub id: String,
    /// Executable to launch (e.g. `"npx"`, `"uvx"`, absolute path).
    #[field(
        label = "Command",
        help = "Executable to launch (e.g. npx, uvx, or an absolute path)."
    )]
    pub command: String,
    /// Arguments passed to the command.
    #[serde(default)]
    #[field(label = "Args", help = "Arguments passed to the command.")]
    pub args: Vec<String>,
    /// Environment variables injected into the MCP server process.
    #[serde(default)]
    #[field(
        label = "Env vars",
        help = "Environment variables injected into the MCP server process."
    )]
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
    /// Optional `[lifecycle]` section: declares run-survival semantics and
    /// orchestrator notification expectations. See [`Lifecycle`] for the
    /// coupling rules enforced at validate time.
    #[serde(default)]
    pub lifecycle: Option<Lifecycle>,
}

/// `[lifecycle]` manifest section. Two coupled controls:
///
/// - `survive_parent` — opt in to outliving the process that spawned this
///   `pitboss dispatch`. Default `false` (the dispatch dies with its parent,
///   matching pitboss's existing "controlled cancellation" posture).
///
/// - `notify` — optional inline `[[notification]]`-style sink declaration,
///   convenient for "I want this run's lifecycle events sent to a specific
///   place without needing a separate `[[notification]]` block." Reuses the
///   existing [`crate::notify::config::NotificationConfig`] shape, so the
///   same SSRF rules apply (https-only, no loopback). Operators wanting
///   loopback orchestrator delivery should use `PITBOSS_PARENT_NOTIFY_URL`
///   instead — the env-var path is operator-trusted and bypasses the
///   manifest-author SSRF guard.
///
/// Coupling enforced at [`crate::manifest::validate`] time:
/// `survive_parent = true` requires AT LEAST ONE of:
///   - this section's `notify` field set, OR
///   - at least one `[[notification]]` section declared at the manifest top
///     level
///
/// A naked `survive_parent = true` with no notification target is rejected
/// because the orchestrator that's losing process-level control over the
/// run needs SOME signal that the run actually finished.
///
/// Why we don't ALSO accept `PITBOSS_PARENT_NOTIFY_URL` as satisfying the
/// coupling at validate time: validate runs against the manifest in
/// isolation (CI gate, pre-flight check) and cannot see the env vars that
/// will be present at the eventual `pitboss dispatch` invocation. The
/// dispatch-time check (in addition) verifies a router actually got built;
/// if the operator relies solely on the env-var path, the manifest must
/// still declare at least a no-cost `kind = "log"` notification to satisfy
/// the validate gate.
#[derive(Debug, Clone, Deserialize, Serialize, Default, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct Lifecycle {
    /// Opt-in: this dispatch is allowed to outlive its parent process.
    /// Pitboss communicates the intent via the [`crate::notify::PitbossEvent::RunDispatched`]
    /// event payload; the orchestrator decides whether to exclude the
    /// sub-pitboss process group from any cancel-tree-walk it performs.
    /// Default: `false`.
    #[serde(default)]
    #[field(
        label = "Survive parent",
        help = "Allow this dispatch to outlive its parent process. Requires a notify target."
    )]
    pub survive_parent: bool,
    /// Optional inline `[[notification]]`-style sink. When present, gets
    /// merged into the run's notification router alongside any top-level
    /// `[[notification]]` sections.
    #[serde(default)]
    #[field(skip)]
    pub notify: Option<crate::notify::config::NotificationConfig>,
}

/// TOML schema for a single `[[approval_policy]]` rule.
#[derive(Debug, Clone, Deserialize, Serialize, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRuleSpec {
    #[serde(default, rename = "match")]
    #[field(skip)]
    pub match_clause: ApprovalMatchSpec,
    /// Action to take when the rule matches.
    /// One of: "auto_approve", "auto_reject", "block".
    #[field(
        label = "Action",
        help = "Action when this rule matches.",
        enum_values = ["auto_approve", "auto_reject", "block"]
    )]
    pub action: String,
}

/// TOML schema for the `[match]` sub-table within an `[[approval_policy]]` rule.
#[derive(Debug, Clone, Deserialize, Serialize, Default, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct ApprovalMatchSpec {
    #[field(
        label = "Actor",
        help = "Actor path, e.g. \"root→S1\" or \"root→S1→W3\"."
    )]
    pub actor: Option<String>,
    #[field(
        label = "Category",
        help = "Event category: tool_use, plan, cost, etc."
    )]
    pub category: Option<String>,
    #[field(label = "Tool name", help = "Specific MCP tool name to match.")]
    pub tool_name: Option<String>,
    #[field(
        label = "Cost over (USD)",
        help = "Fires when the request's cost_estimate exceeds this value."
    )]
    pub cost_over: Option<f64>,
}

/// Effective default for [`RunConfig::max_parallel_tasks`] when neither
/// the manifest nor the `ANTHROPIC_MAX_CONCURRENT` env var sets it.
/// Re-exported from `resolve` so consumers reading just this schema
/// file (form renderers, `pitboss schema` output, doc generators) can
/// discover the default without grepping the resolver.
pub const DEFAULT_MAX_PARALLEL_TASKS: u32 = 4;

/// Run-wide infrastructure config (NOT lead-specific).
///
/// Lead-specific caps moved to `[lead]` in v0.9 (`max_workers`, `budget_usd`,
/// `lead_timeout_secs`).
#[derive(Debug, Clone, Deserialize, Serialize, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    /// Human-readable label used to group related runs in the operational
    /// console (e.g. `"build-db"`, `"nightly-sync"`). When unset, the
    /// console falls back to the manifest filename. The canonical reference
    /// to a run remains its UUIDv7 `run_id`; this name is purely for
    /// cross-run grouping.
    #[serde(default)]
    #[field(
        label = "Run name",
        help = "Human-readable label used to group related runs in the console (e.g. \"build-db\", \"nightly-sync\"). When unset, the manifest filename is used as fallback."
    )]
    pub name: Option<String>,
    /// Concurrency cap for `[[task]]` flat mode. Renamed from `max_parallel`
    /// in v0.9. Overridden by `ANTHROPIC_MAX_CONCURRENT` env var. Default:
    /// [`DEFAULT_MAX_PARALLEL_TASKS`] (4).
    #[serde(default)]
    #[field(
        label = "Max parallel tasks",
        help = "Flat-mode concurrency cap for [[task]] runs. Default 4. Overridden by ANTHROPIC_MAX_CONCURRENT."
    )]
    pub max_parallel_tasks: Option<u32>,
    /// Stop flat-mode runs on first failure. Ignored in hierarchical mode.
    #[serde(default)]
    #[field(
        label = "Halt on failure",
        help = "Stop remaining flat-mode tasks on first failure."
    )]
    pub halt_on_failure: bool,
    /// Where run artifacts land. Default `~/.local/share/pitboss/runs`.
    #[field(
        label = "Run directory",
        help = "Where per-run artifacts land. Default ~/.local/share/pitboss/runs."
    )]
    pub run_dir: Option<PathBuf>,
    /// Worktree-cleanup policy. Default: `on_success`.
    #[serde(default = "default_cleanup")]
    #[field(
        label = "Worktree cleanup",
        help = "What to do with each worker's git worktree after it finishes.",
        enum_values = ["always", "on_success", "never"]
    )]
    pub worktree_cleanup: WorktreeCleanup,
    /// Write an event-stream JSONL alongside `summary.jsonl`. Default off.
    #[serde(default)]
    #[field(
        label = "Emit event stream",
        help = "Write a JSONL event stream alongside summary.jsonl."
    )]
    pub emit_event_stream: bool,
    /// Default approval-policy action when no TUI is attached and no
    /// `[[approval_policy]]` rule matches. Renamed from `approval_policy`
    /// in v0.9 to disambiguate from the rules array. One of:
    /// `"block"` (default), `"auto_approve"`, `"auto_reject"`.
    #[serde(default)]
    #[field(
        label = "Default approval policy",
        help = "Default action for request_approval / propose_plan when no TUI is attached and no rule matches.",
        enum_values = ["block", "auto_approve", "auto_reject"]
    )]
    pub default_approval_policy: Option<crate::dispatch::state::ApprovalPolicy>,
    /// Dump the shared store (`/ref/*`, `/peer/*`, `/shared/*`, `/leases/*`)
    /// to `<run-dir>/shared-store.json` on finalize.
    #[serde(default)]
    #[field(
        label = "Dump shared store",
        help = "Write shared-store.json into the run directory on finalize."
    )]
    pub dump_shared_store: bool,
    /// When true, the lead must call `propose_plan` and have the resulting
    /// plan approved before any `spawn_worker` succeeds.
    #[serde(default)]
    #[field(
        label = "Require plan approval",
        help = "When true, spawn_worker is blocked until propose_plan has been approved."
    )]
    pub require_plan_approval: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            name: None,
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

#[derive(Debug, Clone, Deserialize, Serialize, Default, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    #[field(
        label = "Model",
        help = "Claude model id (e.g. claude-haiku-4-5, claude-sonnet-4-6, claude-opus-4-7)."
    )]
    pub model: Option<String>,
    #[field(
        label = "Effort",
        help = "Maps to the claude --effort flag.",
        enum_values = ["low", "medium", "high", "xhigh", "max"]
    )]
    pub effort: Option<Effort>,
    #[field(
        label = "Tools",
        help = "Allowed tool surface. Pitboss auto-appends its MCP tools for leads and workers."
    )]
    pub tools: Option<Vec<String>>,
    #[field(
        label = "Timeout (seconds)",
        help = "Per-task wall-clock cap. No default (no cap)."
    )]
    pub timeout_secs: Option<u64>,
    #[field(
        label = "Use git worktree",
        help = "Isolate each worker in a git worktree. Default true."
    )]
    pub use_worktree: Option<bool>,
    #[serde(default)]
    #[field(
        label = "Env vars",
        help = "Environment variables passed to the claude subprocess."
    )]
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

#[derive(Debug, Clone, Deserialize, Serialize, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct Template {
    #[field(
        label = "Template ID",
        help = "Slug referenced from [[task]].template."
    )]
    pub id: String,
    #[field(
        label = "Prompt",
        help = "Prompt body. Supports {var} placeholders supplied by [[task]].vars.",
        form_type = "long_text"
    )]
    pub prompt: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct Task {
    #[field(
        label = "Task ID",
        help = "Unique slug. Alphanumeric + _ + -. Used in logs and worktree names."
    )]
    pub id: String,
    #[field(
        label = "Directory",
        help = "Working directory. Must be inside a git repo if use_worktree = true."
    )]
    pub directory: PathBuf,
    #[field(
        label = "Prompt",
        help = "Prompt body sent to claude via -p. Mutually exclusive with `template`.",
        form_type = "long_text"
    )]
    pub prompt: Option<String>,
    #[field(
        label = "Template ID",
        help = "Reference to a [[template]] entry. Mutually exclusive with `prompt`."
    )]
    pub template: Option<String>,
    #[serde(default)]
    #[field(
        label = "Template vars",
        help = "Substitutions for {placeholders} when using `template`."
    )]
    pub vars: HashMap<String, String>,
    #[field(
        label = "Branch",
        help = "Worktree branch name. Auto-generated if omitted."
    )]
    pub branch: Option<String>,
    #[field(label = "Model", help = "Per-task override of [defaults].model.")]
    pub model: Option<String>,
    #[field(
        label = "Effort",
        help = "Per-task override of [defaults].effort.",
        enum_values = ["low", "medium", "high", "xhigh", "max"]
    )]
    pub effort: Option<Effort>,
    #[field(label = "Tools", help = "Per-task override of [defaults].tools.")]
    pub tools: Option<Vec<String>>,
    #[field(label = "Timeout (seconds)", help = "Per-task wall-clock cap.")]
    pub timeout_secs: Option<u64>,
    #[field(
        label = "Use git worktree",
        help = "Per-task override of [defaults].use_worktree."
    )]
    pub use_worktree: Option<bool>,
    #[serde(default)]
    #[field(
        label = "Env vars",
        help = "Per-task env vars merged on top of [defaults].env."
    )]
    pub env: HashMap<String, String>,
}

/// Single canonical lead shape (v0.9). Replaces the v0.8 `[[lead]]`/`[lead]` split.
///
/// Lead-level caps that previously lived under `[run]` (`max_workers`,
/// `budget_usd`, `lead_timeout_secs`) live here in v0.9 — they're properties
/// of the lead, not the run.
#[derive(Debug, Clone, Deserialize, Serialize, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct Lead {
    /// Unique slug for the lead (used as the TUI tile label and in run
    /// artifact paths). Required in v0.9 — no cwd-derived default.
    #[field(
        label = "Lead ID",
        help = "Unique slug used as the TUI tile label and in run artifact paths."
    )]
    pub id: String,
    /// Working directory for the lead's claude subprocess. Required in v0.9
    /// — no cwd-derived default. Tilde expansion is performed at load time.
    #[field(
        label = "Directory",
        help = "Working directory for the lead's claude subprocess. Must be a git work-tree if use_worktree = true."
    )]
    pub directory: PathBuf,
    /// Operator prompt for the lead. Required.
    ///
    /// **Important:** in TOML, `prompt =` MUST appear before any subtable
    /// declaration. A `prompt =` placed after a subtable header is silently
    /// reassigned to that subtable's scope; `pitboss validate` catches the
    /// resulting empty prompt and reports it.
    #[field(
        label = "Prompt",
        help = "Operator instructions passed to claude via -p. Must appear before any [lead.X] subtable in the source.",
        form_type = "long_text"
    )]
    pub prompt: String,

    /// Branch name for the lead's worktree. Auto-generated if omitted.
    #[serde(default)]
    #[field(
        label = "Branch",
        help = "Worktree branch name. Auto-generated if omitted."
    )]
    pub branch: Option<String>,
    #[serde(default)]
    #[field(label = "Model", help = "Per-lead override of [defaults].model.")]
    pub model: Option<String>,
    #[serde(default)]
    #[field(
        label = "Effort",
        help = "Per-lead override of [defaults].effort.",
        enum_values = ["low", "medium", "high", "xhigh", "max"]
    )]
    pub effort: Option<Effort>,
    #[serde(default)]
    #[field(
        label = "Tools",
        help = "Per-lead override of [defaults].tools. Pitboss auto-appends its MCP tools."
    )]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    #[field(
        label = "Timeout (seconds)",
        help = "Per-actor subprocess wall-clock cap (claude --timeout)."
    )]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    #[field(
        label = "Use git worktree",
        help = "Per-lead override of [defaults].use_worktree."
    )]
    pub use_worktree: Option<bool>,
    #[serde(default)]
    #[field(
        label = "Env vars",
        help = "Per-lead env vars merged on top of [defaults].env."
    )]
    pub env: HashMap<String, String>,

    // ── Lead-level caps (moved from [run] in v0.9) ───────────────────────
    /// Hard cap on the lead's concurrent + queued worker pool (1–16).
    #[serde(default)]
    #[field(
        label = "Max workers",
        help = "Hard cap on the lead's concurrent + queued worker pool (1–16). Required when the lead spawns workers."
    )]
    pub max_workers: Option<u32>,
    /// Soft cap on lead's spend (USD) with reservation accounting.
    /// `spawn_worker` fails with `budget exceeded` once
    /// `spent + reserved + next_estimate > budget`.
    #[serde(default)]
    #[field(
        label = "Budget (USD)",
        help = "Soft cap on lead spend with reservation accounting. spawn_worker fails once spent + reserved + next_estimate > budget."
    )]
    pub budget_usd: Option<f64>,
    /// Wall-clock cap on the lead session (seconds). Distinct from
    /// `timeout_secs` (which becomes the claude `--timeout` flag for
    /// per-actor subprocess wall-clock). Default 3600 if unset.
    #[serde(default)]
    #[field(
        label = "Lead timeout (seconds)",
        help = "Wall-clock cap on the lead session. Default 3600."
    )]
    pub lead_timeout_secs: Option<u64>,

    // ── v0.8 permission routing ──────────────────────────────────────────
    /// `"path_a"` (default): `CLAUDE_CODE_ENTRYPOINT=sdk-ts` bypasses claude's
    /// built-in gate; pitboss is sole authority via its approval queue.
    /// `"path_b"`: pitboss registers a `permission_prompt` MCP tool;
    /// claude routes each permission check through it.
    #[serde(default)]
    #[field(
        label = "Permission routing",
        help = "path_a (default) makes pitboss the sole permission authority. path_b routes claude's gate through pitboss (rejected at validate time pending stabilization).",
        enum_values = ["path_a", "path_b"]
    )]
    pub permission_routing: PermissionRouting,

    // ── v0.6 depth-2 controls ────────────────────────────────────────────
    /// When true, `spawn_sublead` is included in the lead's MCP toolset.
    #[serde(default)]
    #[field(
        label = "Allow sub-leads",
        help = "Expose spawn_sublead to the root lead."
    )]
    pub allow_subleads: bool,
    /// Hard cap on total live sub-leads under this root.
    #[serde(default)]
    #[field(
        label = "Max sub-leads",
        help = "Hard cap on total live sub-leads under this root."
    )]
    pub max_subleads: Option<u32>,
    /// Hard cap on per-sub-lead budget envelope (USD).
    #[serde(default)]
    #[field(
        label = "Max sub-lead budget (USD)",
        help = "Cap on the per-sub-lead budget envelope."
    )]
    pub max_sublead_budget_usd: Option<f64>,
    /// Hard cap on total live workers across the entire tree (root + sub-trees).
    /// Renamed from `max_workers_across_tree` in v0.9.
    #[serde(default)]
    #[field(
        label = "Max total workers",
        help = "Cap on total live workers across the entire tree (root + sub-trees)."
    )]
    pub max_total_workers: Option<u32>,
}

/// Top-level `[sublead_defaults]` block (promoted from `[lead.sublead_defaults]`
/// in v0.9). Supplies fallback values for `spawn_sublead` calls that omit them.
#[derive(Debug, Clone, Deserialize, Serialize, Default, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct SubleadDefaults {
    #[field(
        label = "Budget (USD)",
        help = "Per-sub-lead envelope when read_down = false."
    )]
    pub budget_usd: Option<f64>,
    #[field(
        label = "Max workers",
        help = "Per-sub-lead worker pool when read_down = false."
    )]
    pub max_workers: Option<u32>,
    #[field(
        label = "Lead timeout (seconds)",
        help = "Wall-clock cap for the sub-lead session."
    )]
    pub lead_timeout_secs: Option<u64>,
    #[serde(default)]
    #[field(
        label = "Read down",
        help = "When true, sub-lead shares root's budget + worker pool instead of carving its own envelope."
    )]
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
    fn parses_run_name_when_present() {
        let toml_src = r#"
            [run]
            name = "nightly-sync"

            [[task]]
            id = "x"
            directory = "/tmp"
            prompt = "p"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.run.name.as_deref(), Some("nightly-sync"));
    }

    #[test]
    fn run_name_defaults_to_none_for_back_compat() {
        let toml_src = r#"
            [[task]]
            id = "x"
            directory = "/tmp"
            prompt = "p"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert!(m.run.name.is_none(), "missing [run].name must be None");
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
