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
//! | `[communication]`         | `CommunicationConfig` | no       | Mailbox + artifact MCP tools (opt-in, default off)   |
//! | `[[notification]]`        | `NotificationConfig`  | no       | Notification sinks                                   |
//! | `[[approval_policy]]`     | `ApprovalRuleSpec`    | no       | Declarative approval rules (matched in order)        |
//! | `[[template]]`            | `Template`            | no       | Prompt templates referenced by `[[task]]`            |
//! | `[[agent_profile]]`       | `AgentProfile`        | no       | Reusable role prelude + env/model/tools defaults     |
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
/// **`PathB`** (default since v0.12) leaves the entrypoint unset so claude's own
/// permission gate is active, and pitboss registers a `permission_prompt` MCP tool.
/// Each per-tool check the model wants outside its `--allowedTools` routes through
/// pitboss's approval queue. With a declared `[[worker_type]]` / `[[sublead_type]]`,
/// the call fast-paths via the profile's `tools` allowlist (#388); without one, it
/// routes through the operator approval bridge unless `[run].untyped_actor_policy
/// = "block"` opts into auto-deny via the synthesized empty profile (#392).
///
/// **`PathA`** sets `CLAUDE_CODE_ENTRYPOINT=sdk-ts` plus
/// `--dangerously-skip-permissions`, which together bypass claude's gate entirely.
/// Pitboss becomes the sole permission authority via its own approval queue / TUI
/// — operator-declared `[[approval_policy]]` rules are the only enforcement.
/// Useful for runs that want to opt out of per-tool gating (e.g., trusted internal
/// scripts), but loses the defense-in-depth that Path B provides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRouting {
    PathA,
    #[default]
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
    /// This is the escape hatch for any `podman run` / `docker run` flag —
    /// networking (`--network=...`, `--dns=...`, `--add-host=...`),
    /// capabilities (`--cap-add=NET_ADMIN`), security (`--security-opt=...`),
    /// resources (`--memory=4g`, `--cpus=2`), and so on.
    #[serde(default)]
    #[field(
        label = "Extra args",
        help = "Verbatim podman/docker run flags. Use for networking, capabilities, DNS, resources, etc. Example: [\"--network=corp-fw\", \"--dns=10.0.0.53\", \"--cap-add=NET_ADMIN\"]."
    )]
    pub extra_args: Vec<String>,
    /// Apt packages installed inside the container at run start, before
    /// `pitboss dispatch` runs. Useful for adding small tools (mdbook, jq,
    /// pandoc) without rebuilding the base image. Each dispatch pays the
    /// install cost; for a faster cached path see the future
    /// `pitboss container-build` subcommand.
    ///
    /// Names are passed verbatim to `apt-get install -y --no-install-recommends`,
    /// so each entry must match `[a-zA-Z0-9][a-zA-Z0-9.+-]*` — anything else
    /// is rejected at dispatch time to keep the joined command shell-safe.
    #[serde(default)]
    #[field(
        label = "Extra apt packages",
        help = "Debian/Ubuntu packages installed inside the container before pitboss dispatch starts. Adds 30–90s spin-up per dispatch. Example: [\"mdbook\", \"jq\"]."
    )]
    pub extra_apt: Vec<String>,
    /// Host→container bind mounts (besides the auto-injected `~/.claude` and run_dir).
    #[serde(default, rename = "mount")]
    #[field(skip)]
    pub mounts: Vec<MountSpec>,
    /// Host→container file copies baked into a derived image at
    /// `pitboss container-build` time. Unlike `mount`, these are not
    /// runtime bind-mounts — the file contents are layered into the
    /// container image, so they're available even when no host path is
    /// mounted there. Use this for small build inputs (scripts,
    /// configuration) that should travel with the image.
    #[serde(default, rename = "copy")]
    #[field(skip)]
    pub copy: Vec<CopySpec>,
    /// Working directory inside the container.
    /// Defaults to the container path of the first `[[container.mount]]` entry,
    /// or `/home/pitboss` if no mounts are declared.
    #[field(
        label = "Working directory",
        help = "cwd inside the container; defaults to the first mount's container path."
    )]
    pub workdir: Option<PathBuf>,
    /// Auto-injected `~/.claude` mount mode. Default `false` mounts the
    /// host's `~/.claude` as read-only — sufficient for static OAuth
    /// tokens and `settings.json`-driven configuration, and prevents a
    /// compromised or buggy worker from writing host credentials or
    /// injecting `settings.json` hooks. Set `true` to restore the
    /// pre-v0.15 behaviour (`rw,z`) when claude needs to refresh OAuth
    /// tokens in-place. (#525 / F-SEC-11)
    ///
    /// An explicit `[[container.mount]]` targeting `/home/pitboss/.claude`
    /// always wins — its `readonly` field controls the mode, this flag
    /// is consulted only for the auto-inject path.
    #[serde(default)]
    #[field(
        label = "Claude mount rw",
        help = "Set true to mount the auto-injected ~/.claude as rw (needed for OAuth token refresh). Default false = read-only."
    )]
    pub claude_mount_rw: bool,
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

/// A single host→container `COPY` entry baked into the derived image
/// by `pitboss container-build`. Unlike `MountSpec`, this is image
/// content, not a runtime mount — the host file is read at build time
/// and layered into the image.
#[derive(Debug, Clone, Deserialize, Serialize, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct CopySpec {
    /// Absolute path on the host (tilde expansion is performed at build time).
    /// Must be a regular file or a directory. Read at `container-build` time
    /// only; subsequent host mutations require a rebuild.
    #[field(label = "Host path", help = "Absolute host path. ~ is expanded.")]
    pub host: PathBuf,
    /// Absolute path inside the container.
    #[field(label = "Container path", help = "Absolute path inside the container.")]
    pub container: PathBuf,
}

/// An external MCP server to inject into actors' `--mcp-config`.
/// Declared as `[[mcp_server]]` in the manifest. By default all actors
/// (lead, sub-lead, and workers) receive the server. The optional `scope`
/// narrows injection to actors of a given typed profile (#252 Phase 1.5).
///
/// Example:
/// ```toml
/// [[mcp_server]]
/// id      = "context7"
/// command = "npx"
/// args    = ["-y", "@upstash/context7-mcp"]
///
/// [[mcp_server]]
/// id      = "fs-writer"
/// command = "/usr/local/bin/fs-mcp"
/// scope   = "type:writer"   # only injected into worker_type/sublead_type "writer"
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
    /// Optional injection scope. Currently the only supported form is
    /// `"type:<id>"`, where `<id>` references a `[[worker_type]]` or
    /// `[[sublead_type]]`. When set, the server is injected ONLY into
    /// actors spawned under that type — including untyped actors when
    /// no profiles are required is NOT supported (`require_actor_type`
    /// still respects its own gate). Unset means inject into all actors
    /// (the v0.11 behaviour). (#252 Phase 1.5)
    #[serde(default)]
    #[field(
        label = "Scope",
        help = "Optional injection scope. Form: \"type:<id>\" — narrows this server to actors spawned under that worker_type/sublead_type. Unset = inject into all actors."
    )]
    pub scope: Option<String>,
    /// Optional per-server tool allowlist. When set, the per-server
    /// allowlist enforces at three layers (most-restrictive-wins):
    ///
    /// 1. **Manifest validate** — actor surfaces (`[lead].tools`,
    ///    `[[task]].tools`, `[[worker_type]].tools`,
    ///    `[[sublead_type]].tools`) referencing `mcp__<id>__<tool>`
    ///    must name a `<tool>` in this allowlist. Mismatches are
    ///    rejected at parse time.
    /// 2. **Spawn-time argv (Path B)** — non-allowlisted entries are
    ///    dropped from the spawned actor's `--allowedTools` so the
    ///    call routes through `mcp__pitboss__permission_prompt`
    ///    instead of being silently auto-approved.
    /// 3. **Runtime permission_prompt (Path B)** — when claude routes
    ///    a `mcp__<id>__<tool>` call, the gate denies if `<tool>` is
    ///    not in this allowlist.
    ///
    /// **Path A note:** `--dangerously-skip-permissions` bypasses the
    /// entire claude permission layer (`--allowedTools` is ignored;
    /// permission_prompt never fires), so the per-server allowlist
    /// has NO runtime effect under Path A. Manifest validate still
    /// rejects conflicting actor surfaces; only the runtime gate is
    /// inert. Operators who chose Path A as the "skip all gates"
    /// escape hatch retain that semantic. (#391 / #399)
    ///
    /// Unset means "no per-server restriction" (the v0.12 default).
    /// Empty list (`tools = []`) is rejected as self-defeating —
    /// remove the `[[mcp_server]]` block instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[field(
        label = "Tools",
        help = "Optional per-tool allowlist for this MCP server. Path B: enforced at validate + spawn argv + permission_prompt runtime. Path A: no runtime effect (claude bypasses the permission layer). Unset = no restriction."
    )]
    pub tools: Option<Vec<String>>,
}

/// Communication policy mode for the Pitboss-owned mailbox + artifact MCP
/// tools. Default is [`CommunicationMode::Disabled`] — operators opt in by
/// declaring `[communication] mode = "parent_child"`. Conservative default
/// so existing manifests upgrading to v0.10 do not silently grow the worker
/// tool surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunicationMode {
    /// `message_*` and `artifact_*` MCP tools are hidden from `list_tools`
    /// and rejected at the handler with `CommunicationError::Disabled`. The
    /// shared-store KV surface remains available for coordination.
    #[default]
    Disabled,
    /// Parent-child actor transfer: root reads everything, sub-leads read
    /// their own sub-tree, workers may message only their direct parent.
    /// Sibling-worker artifact handoff requires the parent (or root) to
    /// call `artifact_grant`.
    ParentChild,
}

/// Default ceilings for [`CommunicationConfig`].
pub const DEFAULT_MAX_MESSAGE_BYTES: u64 = 8 * 1024;
pub const DEFAULT_MAX_ARTIFACT_BYTES: u64 = 10 * 1024 * 1024;
pub const DEFAULT_MAX_ARTIFACTS_PER_ACTOR: u32 = 128;

/// Pitboss-owned directional communication controls for the mailbox and
/// artifact MCP tools. The public surface is deliberately coarse; the
/// internal authorization checks can later accept explicit
/// `[[communication.rule]]` entries without replacing the runtime store.
///
/// **Default**: [`CommunicationMode::Disabled`] — opt in via
/// `[communication] mode = "parent_child"`.
#[derive(Debug, Clone, Deserialize, Serialize, FieldMetadata)]
#[serde(default, deny_unknown_fields)]
pub struct CommunicationConfig {
    #[field(
        label = "Mode",
        help = "Actor communication policy mode. \"disabled\" hides the message_* and artifact_* tools (conservative default). \"parent_child\" enables them with parent/sublead/worker authz.",
        enum_values = ["disabled", "parent_child"],
        required = false
    )]
    pub mode: CommunicationMode,
    #[field(
        label = "Max message bytes",
        help = "Maximum UTF-8 body size accepted by message_send.",
        required = false
    )]
    pub max_message_bytes: u64,
    #[field(
        label = "Max artifact bytes",
        help = "Maximum decoded artifact payload size accepted by artifact_put.",
        required = false
    )]
    pub max_artifact_bytes: u64,
    #[field(
        label = "Max artifacts per actor",
        help = "Maximum artifacts one actor may publish in a single run.",
        required = false
    )]
    pub max_artifacts_per_actor: u32,
}

impl Default for CommunicationConfig {
    fn default() -> Self {
        Self {
            mode: CommunicationMode::default(),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            max_artifact_bytes: DEFAULT_MAX_ARTIFACT_BYTES,
            max_artifacts_per_actor: DEFAULT_MAX_ARTIFACTS_PER_ACTOR,
        }
    }
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
    /// Reusable role profiles (`[[agent_profile]]`). The effective
    /// catalogue at resolve time is the union of bundled built-ins
    /// (`pitboss/lead-opus`, `pitboss/sublead-sonnet`,
    /// `pitboss/worker-haiku`) and these manifest-declared entries;
    /// manifest entries shadow built-ins by matching id exactly. See
    /// [`AgentProfile`].
    #[serde(default, rename = "agent_profile")]
    pub agent_profiles: Vec<AgentProfile>,
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
    /// Typed worker profiles. Each entry caps the capability surface for
    /// `spawn_worker(worker_type = "<id>")` calls. The lead can never
    /// widen these caps — `tools` arg must be a subset of the profile's
    /// allowlist; `model` must be in `allowed_models`; `timeout_secs`
    /// only clamps downward. See [`WorkerType`]. (#252)
    #[serde(default, rename = "worker_type")]
    pub worker_types: Vec<WorkerType>,
    /// Typed sub-lead profiles. Same semantics as `[[worker_type]]`,
    /// applied to `spawn_sublead(sublead_type = "<id>")`. Adds a
    /// `max_budget_usd` cap that clamps the sub-tree's envelope.
    /// (#252)
    #[serde(default, rename = "sublead_type")]
    pub sublead_types: Vec<SubleadType>,
    /// Optional `[communication]` section: opt in to the Pitboss-owned
    /// mailbox + artifact MCP tools. Default
    /// [`CommunicationMode::Disabled`] hides the tools and rejects calls
    /// at the handler — coordination falls back to the existing
    /// shared-store KV surface.
    #[serde(default)]
    pub communication: CommunicationConfig,
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
    /// Default approval-policy action applied to every approval request
    /// that no `[[approval_policy]]` rule matches. Renamed from
    /// `approval_policy` in v0.9 to disambiguate from the rules array.
    /// One of:
    /// - `"block"` (default): if a TUI/web console is attached, route
    ///   the request to the operator; otherwise queue until one
    ///   connects.
    /// - `"auto_approve"`: unconditionally auto-approve. Operator UI is
    ///   never paged; the response is synthesized inside the dispatcher.
    /// - `"auto_reject"`: unconditionally auto-reject. Same UI behavior.
    ///
    /// Pre-v0.9.2 these only applied "when no TUI is attached" — a
    /// connected console silently bypassed `auto_approve` /
    /// `auto_reject` and routed to the operator anyway. That meant the
    /// field's effective behavior depended on whether someone happened
    /// to be watching, which was the worst kind of bug class. Now both
    /// `auto_*` actions short-circuit unconditionally; only `block`
    /// routes through the TUI. Operators who want manual review with a
    /// "headless = approve" fallback should leave this at `block` and
    /// add a `[[approval_policy]]` rule for the auto-approve case
    /// (rules already short-circuit before the TUI hop).
    #[serde(default)]
    #[field(
        label = "Default approval policy",
        help = "Default action for request_approval / propose_plan when no rule matches. `auto_approve`/`auto_reject` are unconditional; `block` routes to the operator if attached, else queues.",
        enum_values = ["block", "auto_approve", "auto_reject"]
    )]
    pub default_approval_policy: Option<crate::dispatch::state::ApprovalPolicy>,
    /// What happens to an actor's terminal status when its most recent
    /// `permission_prompt` was denied. `adapt` (default) trusts the
    /// actor's exit code; `reclassify` re-labels clean exits within
    /// 30s of a denial as `ApprovalRejected`. See #377.
    #[serde(default)]
    #[field(
        label = "Denial termination policy",
        help = "How a denied permission_prompt affects the actor's terminal status. `adapt` (default) keeps the actor's exit code as-is; `reclassify` re-labels clean exits within 30s of a denial as ApprovalRejected.",
        enum_values = ["adapt", "reclassify"]
    )]
    pub denial_termination_policy: Option<crate::dispatch::state::DenialTerminationPolicy>,
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
    /// When true, every `spawn_worker` and `spawn_sublead` call MUST name
    /// a `worker_type` / `sublead_type` declared in the manifest;
    /// type-less spawns are rejected at the dispatcher. Default `false`
    /// for back-compat with manifests that pre-date typed profiles
    /// (#252).
    #[serde(default)]
    #[field(
        label = "Require actor type",
        help = "When true, every spawn_worker and spawn_sublead call must name a declared [[worker_type]]/[[sublead_type]]. Default false."
    )]
    pub require_actor_type: bool,
    /// What happens when a `spawn_worker` / `spawn_sublead` call
    /// reaches `permission_prompt` for an actor that has no declared
    /// `[[worker_type]]` / `[[sublead_type]]`. Only meaningful under
    /// `permission_routing = "path_b"`. Default `bridge` preserves
    /// pre-#252 behavior (route to the operator approval bridge);
    /// `block` synthesizes an empty profile so anything outside
    /// `--allowedTools` auto-denies via `denied_by_profile` without
    /// an operator round-trip — the strict counterpart to declaring
    /// every actor's profile, useful for headless production runs
    /// once the operator has migrated their manifests.
    #[serde(default)]
    #[field(
        label = "Untyped actor policy",
        help = "Path-B-only behavior for un-typed callers reaching permission_prompt. `bridge` (default) routes to the operator queue; `block` auto-denies via a synthesized empty profile.",
        enum_values = ["bridge", "block"]
    )]
    pub untyped_actor_policy: UntypedActorPolicy,
}

/// What pitboss does when an un-typed Worker / Sublead reaches
/// `permission_prompt` under Path B (#252). See the field doc on
/// `RunConfig::untyped_actor_policy` for the full semantics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UntypedActorPolicy {
    /// Route to the operator approval bridge (pre-#252 behavior).
    #[default]
    Bridge,
    /// Auto-deny via a synthesized empty profile so the model receives
    /// `denied_by_profile` without an operator round-trip. Pairs with
    /// declared profiles to make the manifest the consent signal.
    Block,
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
            denial_termination_policy: None,
            dump_shared_store: false,
            require_plan_approval: false,
            require_actor_type: false,
            untyped_actor_policy: UntypedActorPolicy::Bridge,
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

/// Reusable role profile (`[[agent_profile]]`). Carries a prepended
/// system-prompt body plus optional model/tools/env *defaults*. Unlike
/// [`WorkerType`]/[`SubleadType`] which encode capability **caps**,
/// agent profiles supply role conventions and fallbacks the operator's
/// `[lead]`/`[[task]]` config can override.
///
/// Referenced by id from `[lead]`, `[[task]]`, `[[worker_type]]`, and
/// `[[sublead_type]]`. The catalogue is the union of three bundled
/// built-ins (`pitboss/lead-opus`, `pitboss/sublead-sonnet`,
/// `pitboss/worker-haiku`) and any manifest-declared profiles; manifest
/// entries shadow built-ins by matching id exactly.
///
/// Precedence (later wins): `DEFAULT_*` → `profile` → `[defaults]` →
/// per-actor (`[lead]` / `[[task]]` / spawn args). Operator config
/// always wins; the profile is a default-provider, never an override.
/// The `system_prompt` is **prepended** to the operator's prompt with a
/// fixed `\n\n--- TASK ---\n\n` separator (never replaced).
#[derive(Debug, Clone, Deserialize, Serialize, Default, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct AgentProfile {
    /// Unique id. Must match `^[A-Za-z0-9_/-]+$` (validate-time check).
    /// The `pitboss/` namespace is reserved for built-ins — a manifest
    /// may shadow a built-in by matching its id exactly, but may not
    /// introduce novel `pitboss/<other>` ids.
    #[field(
        label = "Profile id",
        help = "Unique id referenced from [lead]/[[task]]/[[worker_type]]/[[sublead_type]] .agent_profile. Match: ^[A-Za-z0-9_/-]+$. The pitboss/ namespace is reserved for built-ins."
    )]
    pub id: String,
    /// Role prelude prepended to the operator's prompt with a
    /// `\n\n--- TASK ---\n\n` separator. Empty (default) means no
    /// prelude — profile is just an env/model/tools default-provider.
    #[serde(default)]
    #[field(
        label = "System prompt",
        help = "Role-shared prelude prepended to the operator's prompt with `\\n\\n--- TASK ---\\n\\n` separator.",
        form_type = "long_text"
    )]
    pub system_prompt: String,
    /// Default model. The operator's `model =` on the actor wins; this
    /// fills any slot the operator didn't fill. Distinct from
    /// `[[worker_type]].allowed_models` which is a *cap*.
    #[serde(default)]
    #[field(
        label = "Model",
        help = "Default Claude model id when the actor's `model =` is unset. Not a cap — operator config wins."
    )]
    pub model: Option<String>,
    /// Env vars merged in between `[defaults].env` and the per-actor
    /// env. Operator env wins on collision.
    #[serde(default)]
    #[field(
        label = "Env vars",
        help = "Env merged between [defaults].env and the per-actor env. Operator wins on collision."
    )]
    pub env: HashMap<String, String>,
    /// Default tool allowlist. The operator's `tools =` wins; this
    /// fills any slot the operator didn't fill. Distinct from
    /// `[[worker_type]].tools` which is a *cap*.
    #[serde(default)]
    #[field(
        label = "Tools",
        help = "Default tool allowlist when the actor's `tools =` is unset. Not a cap — operator config wins."
    )]
    pub tools: Option<Vec<String>>,
}

/// Typed worker profile (`[[worker_type]]`). Caps the capability
/// surface a `spawn_worker(worker_type = "<id>")` call may request.
/// The lead-supplied `tools` arg must be a subset of `tools`;
/// `model` must be in `allowed_models` (when set); `timeout_secs`
/// is clamped *down* to `max_timeout_secs` (never up). When the
/// caller omits `tools`, the spawn gets the profile's full
/// allowlist — explicitly, not the lead's `[lead].tools` cascade.
/// (#252)
#[derive(Debug, Clone, Deserialize, Serialize, Default, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct WorkerType {
    /// Unique id referenced by `spawn_worker(worker_type = "<id>")`.
    /// Must match `^[A-Za-z0-9_-]+$` (validate-time check).
    #[field(
        label = "Worker type id",
        help = "Unique id used in spawn_worker(worker_type = \"<id>\"). Match: ^[A-Za-z0-9_-]+$."
    )]
    pub id: String,
    /// Allowed tool surface for this worker class. Lead must request a
    /// subset; omit-on-spawn means "give me the whole allowlist."
    #[serde(default)]
    #[field(
        label = "Tools",
        help = "Allowed tool surface for this worker class. Lead's spawn_worker(tools=...) must be a subset; omit means \"give me the whole allowlist.\""
    )]
    pub tools: Vec<String>,
    /// When non-empty, the lead's `model` arg must appear in this list.
    /// Empty = no model restriction (any model the lead can pay for).
    #[serde(default)]
    #[field(
        label = "Allowed models",
        help = "When non-empty, spawn_worker(model=...) must be in this list. Empty means any model."
    )]
    pub allowed_models: Vec<String>,
    /// Hard cap on `spawn_worker(timeout_secs = ...)`. Lead values larger
    /// than this clamp DOWN; smaller values are honored.
    #[serde(default)]
    #[field(
        label = "Max timeout (seconds)",
        help = "Hard cap on spawn_worker(timeout_secs=...). Lead values clamp DOWN; smaller values pass through."
    )]
    pub max_timeout_secs: Option<u64>,
    /// Optional reference to an [`AgentProfile`] id (built-in or
    /// manifest-declared). When set, every `spawn_worker(worker_type =
    /// "<id>")` call gets the profile's `system_prompt` prepended to its
    /// operator prompt and inherits the profile's env/model/tools
    /// defaults for any slot the call didn't fill.
    #[serde(default)]
    #[field(
        label = "Agent profile",
        help = "Reference to an [[agent_profile]].id. Workers spawned under this worker_type inherit the profile's system_prompt prelude + env/model/tools defaults."
    )]
    pub agent_profile: Option<String>,
}

/// Typed sub-lead profile (`[[sublead_type]]`). Same enforcement model
/// as [`WorkerType`] with an additional `max_budget_usd` clamp on the
/// sub-tree's spawn-time budget. (#252)
#[derive(Debug, Clone, Deserialize, Serialize, Default, FieldMetadata)]
#[serde(deny_unknown_fields)]
pub struct SubleadType {
    /// Unique id referenced by `spawn_sublead(sublead_type = "<id>")`.
    /// Must match `^[A-Za-z0-9_-]+$` (validate-time check).
    #[field(
        label = "Sub-lead type id",
        help = "Unique id used in spawn_sublead(sublead_type = \"<id>\"). Match: ^[A-Za-z0-9_-]+$."
    )]
    pub id: String,
    /// Allowed tool surface for this sub-lead class.
    #[serde(default)]
    #[field(
        label = "Tools",
        help = "Allowed tool surface for this sub-lead class. spawn_sublead(tools=...) must be a subset; empty means \"give me the whole allowlist.\""
    )]
    pub tools: Vec<String>,
    /// When non-empty, the caller's `model` arg must appear in this list.
    #[serde(default)]
    #[field(
        label = "Allowed models",
        help = "When non-empty, spawn_sublead(model=...) must be in this list."
    )]
    pub allowed_models: Vec<String>,
    /// Hard cap on `spawn_sublead(lead_timeout_secs = ...)`. Clamps DOWN.
    #[serde(default)]
    #[field(
        label = "Max lead timeout (seconds)",
        help = "Hard cap on spawn_sublead(lead_timeout_secs=...). Clamps DOWN."
    )]
    pub max_timeout_secs: Option<u64>,
    /// Hard cap on `spawn_sublead(budget_usd = ...)`. Clamps DOWN.
    #[serde(default)]
    #[field(
        label = "Max budget (USD)",
        help = "Hard cap on spawn_sublead(budget_usd=...). Clamps DOWN."
    )]
    pub max_budget_usd: Option<f64>,
    /// Optional reference to an [`AgentProfile`] id (built-in or
    /// manifest-declared). When set, every
    /// `spawn_sublead(sublead_type = "<id>")` call gets the profile's
    /// `system_prompt` prepended to its operator prompt and inherits
    /// the profile's env/model/tools defaults.
    #[serde(default)]
    #[field(
        label = "Agent profile",
        help = "Reference to an [[agent_profile]].id. Sub-leads spawned under this sublead_type inherit the profile's system_prompt prelude + env/model/tools defaults."
    )]
    pub agent_profile: Option<String>,
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
    /// Optional reference to an [`AgentProfile`] id (built-in or
    /// manifest-declared). When set, the profile's `system_prompt` is
    /// prepended to the task prompt at resolve time, and the profile's
    /// env/model/tools defaults fill any slot the task didn't fill.
    #[serde(default)]
    #[field(
        label = "Agent profile",
        help = "Reference to an [[agent_profile]].id. The profile's system_prompt is prepended; its env/model/tools defaults fill any slot the task didn't set."
    )]
    pub agent_profile: Option<String>,
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
    /// Soft cap on the run's total spend (USD): workers + lead + sub-leads.
    /// `spawn_worker` fails with `budget exceeded` once
    /// `total_spent + reserved + next_estimate > budget`, and the lead is
    /// aborted if accumulated lead/sub-lead token spend pushes `total_spent`
    /// past this cap on any reconciled turn.
    #[serde(default)]
    #[field(
        label = "Budget (USD)",
        help = "Soft cap on the run's total spend (workers + lead + sub-leads). spawn_worker fails once total_spent + reserved + next_estimate > budget; the lead is aborted if a reconciled turn drives total_spent past the cap."
    )]
    pub budget_usd: Option<f64>,
    /// Optional separate cap on the lead's own + sub-lead's own token spend
    /// (USD). Independent of `budget_usd` (which is the run-wide total).
    /// When set, the lead is aborted once accumulated lead/sub-lead spend
    /// exceeds this cap, even if `budget_usd` still has headroom.
    #[serde(default)]
    #[field(
        label = "Lead budget (USD)",
        help = "Optional separate cap on lead + sub-lead token spend (orchestration cost). Independent of budget_usd. Lead is aborted when reached."
    )]
    pub lead_budget_usd: Option<f64>,
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
    /// `"path_b"` (default since v0.12): pitboss registers a
    /// `permission_prompt` MCP tool; claude routes each per-tool check
    /// through it. Layered enforcement runs the per-server
    /// `[[mcp_server]].tools` allowlist first, then operator
    /// `[[approval_policy]]` rules, then the typed-profile gate.
    /// `"path_a"`: `CLAUDE_CODE_ENTRYPOINT=sdk-ts` plus
    /// `--dangerously-skip-permissions` bypasses claude's built-in gate;
    /// pitboss becomes sole authority via the approval queue. Use only
    /// for runs that want to opt out of per-tool gating entirely.
    #[serde(default)]
    #[field(
        label = "Permission routing",
        help = "path_b (default) routes claude's per-tool gate through pitboss's permission_prompt MCP tool — layered enforcement (mcp_server tools allowlist, operator policy, typed profile). path_a bypasses claude's gate via --dangerously-skip-permissions and makes pitboss the sole authority.",
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
    /// Optional reference to an [`AgentProfile`] id (built-in or
    /// manifest-declared). When set, the profile's `system_prompt` is
    /// prepended to `[lead].prompt` at resolve time, and the profile's
    /// env/model/tools defaults fill any slot the lead didn't fill.
    #[serde(default)]
    #[field(
        label = "Agent profile",
        help = "Reference to an [[agent_profile]].id. The profile's system_prompt is prepended to [lead].prompt; its env/model/tools defaults fill any slot the lead didn't set."
    )]
    pub agent_profile: Option<String>,
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
        label = "Lead budget (USD)",
        help = "Per-sub-lead cap on the sub-lead's own token spend (orchestration cost), independent of budget_usd. Honored when read_down = false."
    )]
    pub lead_budget_usd: Option<f64>,
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
