//! `pitboss validate --capability-matrix` formatter (#385, #391).
//!
//! Walks the resolved manifest's `[[mcp_server]]` list and computes,
//! for each declared `[[worker_type]]` / `[[sublead_type]]` (plus an
//! "untyped / root" row representing the root lead and any
//! pre-#252-style un-profiled spawns), the set of MCP servers — and
//! their per-server tool allowlists — that pitboss would inject into
//! actors of that class.
//!
//! Reuses the same admission helper as the runtime injection
//! (`crate::dispatch::hierarchical::mcp_server_scope_admits`) so the
//! report output and the live behavior cannot drift.
//!
//! ## What this matrix means (and what it does not)
//!
//! The matrix shows the **server-side** allowlist as it applies on
//! each row: which MCP servers admit, and the `[[mcp_server]].tools`
//! filter pitboss enforces against `--allowedTools` and the
//! `permission_prompt` short-circuit (#399).
//!
//! It is **not** an intersection with the actor-side
//! `[[worker_type]].tools` / `[[sublead_type]].tools` profile
//! allowlist — that orthogonal restriction is surfaced in the actor
//! profile panel of the wizard. The matrix answers "what can the
//! server admit for this row," not "what can this actor actually
//! call."
//!
//! ## Wire-shape contract
//!
//! `MatrixRow` ships JSON over `pitboss-web`'s `validate` endpoint,
//! is consumed by the SPA's TS `MatrixRow` interface, and is read
//! by the TUI from `resolved.json`. Three consumers; one shape.
//! Field names and the snake-case `kind` / `MatrixServerEntry`
//! discriminants are pinned by tests in this module.

use crate::dispatch::hierarchical::mcp_server_scope_admits;
use crate::manifest::resolve::ResolvedManifest;
use serde::Serialize;

/// Which class of actor a [`MatrixRow`] represents.
///
/// The CLI text formatter prefixes each label with this kind
/// (`worker_type:` / `sublead_type:`) and renders the untyped row as
/// `(untyped / root)`. Programmatic consumers (TUI Detail view,
/// `pitboss-web` manifest panel) use the kind directly to pick which
/// row matches a focused tile's `actor_type`.
///
/// Serialized as snake_case strings (`"untyped"` / `"worker_type"` /
/// `"sublead_type"`) on the wire, so the SPA's TypeScript stays
/// idiomatic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    /// Root lead and any un-profiled (`actor_type = None`) spawn.
    Untyped,
    /// A declared `[[worker_type]]`.
    WorkerType,
    /// A declared `[[sublead_type]]`.
    SubleadType,
}

/// One MCP server admitted on a [`MatrixRow`], plus the per-server
/// `[[mcp_server]].tools` allowlist as it applies to actors on that
/// row.
///
/// `tools = None` means the server has no allowlist — every tool the
/// server exports is admitted at the server gate. `tools = Some(list)`
/// means pitboss enforces that allowlist at spawn-time
/// (`--allowedTools` filter) and at runtime (`permission_prompt`
/// short-circuit) — see #399.
///
/// `validate.rs` rejects `tools = Some([])` as self-defeating, so
/// consumers can safely treat `Some(non_empty)` and `None` as the
/// only two cases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatrixServerEntry {
    pub server_id: String,
    /// `None` = no per-server filter (all tools admitted).
    /// `Some(non_empty)` = explicit allowlist enforced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
}

/// One row of the actor-type × MCP-server matrix.
///
/// `actor_type` is `None` for the untyped/root row and `Some(id)` for
/// declared profiles; consumers that want to match a tile's
/// `actor_type` directly check `row.actor_type.as_deref() ==
/// tile.actor_type.as_deref()`.
///
/// `servers` is in manifest declaration order so two manifests
/// producing the same matrix shape compare stable diff-wise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatrixRow {
    /// Display label as the CLI formatter emits it (`(untyped / root)`,
    /// `worker_type:writer`, `sublead_type:planner`).
    pub label: String,
    /// `None` for the untyped row, `Some(id)` for typed rows. Drives
    /// matching against `TaskRecord.actor_type`.
    pub actor_type: Option<String>,
    pub kind: RowKind,
    /// MCP servers (with their per-server tool allowlists) that
    /// scope-admit for this row, in manifest declaration order.
    pub servers: Vec<MatrixServerEntry>,
}

/// Lightweight server descriptor for [`rows_from_parts`]. The TUI
/// deserializes a subset of `resolved.json` and feeds these in
/// without having to construct a full `ResolvedManifest`.
#[derive(Debug, Clone)]
pub struct MatrixServerSpec {
    pub id: String,
    pub scope: Option<String>,
    pub tools: Option<Vec<String>>,
}

/// Compute matrix rows from a fully resolved manifest. Used by the CLI
/// `--capability-matrix` formatter and any Rust consumer that already
/// owns a `ResolvedManifest`.
///
/// Row order:
/// 1. Untyped (always first; the "default" anchor row).
/// 2. One row per `[[worker_type]]`, in declaration order.
/// 3. One row per `[[sublead_type]]`, in declaration order.
pub fn rows(manifest: &ResolvedManifest) -> Vec<MatrixRow> {
    let servers: Vec<MatrixServerSpec> = manifest
        .mcp_servers
        .iter()
        .map(|s| MatrixServerSpec {
            id: s.id.clone(),
            scope: s.scope.clone(),
            tools: s.tools.clone(),
        })
        .collect();
    let wt_ids: Vec<String> = manifest.worker_types.iter().map(|w| w.id.clone()).collect();
    let st_ids: Vec<String> = manifest
        .sublead_types
        .iter()
        .map(|s| s.id.clone())
        .collect();
    rows_from_parts(&servers, &wt_ids, &st_ids)
}

/// Compute matrix rows from the minimal data the algorithm needs.
/// Keeps the matrix logic decoupled from the larger manifest type.
pub fn rows_from_parts(
    servers: &[MatrixServerSpec],
    worker_type_ids: &[String],
    sublead_type_ids: &[String],
) -> Vec<MatrixRow> {
    let mut out: Vec<MatrixRow> =
        Vec::with_capacity(1 + worker_type_ids.len() + sublead_type_ids.len());
    out.push(MatrixRow {
        label: "(untyped / root)".to_string(),
        actor_type: None,
        kind: RowKind::Untyped,
        servers: admitted_for(servers, None),
    });
    for id in worker_type_ids {
        out.push(MatrixRow {
            label: format!("worker_type:{id}"),
            actor_type: Some(id.clone()),
            kind: RowKind::WorkerType,
            servers: admitted_for(servers, Some(id.as_str())),
        });
    }
    for id in sublead_type_ids {
        out.push(MatrixRow {
            label: format!("sublead_type:{id}"),
            actor_type: Some(id.clone()),
            kind: RowKind::SubleadType,
            servers: admitted_for(servers, Some(id.as_str())),
        });
    }
    out
}

fn admitted_for(servers: &[MatrixServerSpec], actor_type: Option<&str>) -> Vec<MatrixServerEntry> {
    servers
        .iter()
        .filter_map(|s| {
            if mcp_server_scope_admits(s.scope.as_deref(), actor_type) {
                Some(MatrixServerEntry {
                    server_id: s.id.clone(),
                    tools: s.tools.clone(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Render the matrix as a multi-line string ending in `\n`.
///
/// Servers admitted on a row print as a comma-joined list. Each
/// server with a `[[mcp_server]].tools` allowlist gets a continuation
/// line listing those tools, indented under its server. Servers with
/// no allowlist render bare — absence is implicit "all tools" and
/// printing every per-server `(all)` annotation would clutter the
/// common case.
pub fn render(manifest: &ResolvedManifest) -> String {
    let rows = rows(manifest);

    let label_width = rows
        .iter()
        .map(|r| r.label.len())
        .chain(std::iter::once("actor type".len()))
        .max()
        .unwrap_or(20);

    let mut out = String::new();
    out.push_str(&format!(
        "{:<label_width$}  mcp servers injected\n",
        "actor type"
    ));
    out.push_str(&format!(
        "{}  {}\n",
        "-".repeat(label_width),
        "-".repeat(36)
    ));
    let indent = " ".repeat(label_width + 2);
    for row in &rows {
        let label = &row.label;
        if row.servers.is_empty() {
            out.push_str(&format!("{label:<label_width$}  (none)\n"));
            continue;
        }
        let ids = row
            .servers
            .iter()
            .map(|s| s.server_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("{label:<label_width$}  {ids}\n"));
        for s in &row.servers {
            if let Some(tools) = &s.tools {
                let joined = tools.join(", ");
                let server_id = &s.server_id;
                out.push_str(&format!("{indent}  {server_id} tools: {joined}\n"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::resolve::ResolvedManifest;
    use crate::manifest::schema::{McpServerSpec, SubleadType, WorkerType, WorktreeCleanup};
    use std::path::PathBuf;

    fn empty_manifest() -> ResolvedManifest {
        ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(1),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: None,
            max_workers: None,
            budget_usd: None,
            lead_budget_usd: None,
            lead_timeout_secs: None,
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        }
    }

    fn srv(id: &str, scope: Option<&str>) -> McpServerSpec {
        McpServerSpec {
            id: id.to_string(),
            command: "/bin/true".into(),
            args: vec![],
            env: Default::default(),
            scope: scope.map(str::to_string),
            tools: None,
        }
    }

    fn srv_with_tools(id: &str, scope: Option<&str>, tools: &[&str]) -> McpServerSpec {
        McpServerSpec {
            id: id.to_string(),
            command: "/bin/true".into(),
            args: vec![],
            env: Default::default(),
            scope: scope.map(str::to_string),
            tools: Some(tools.iter().map(|t| (*t).to_string()).collect()),
        }
    }

    fn wt(id: &str) -> WorkerType {
        WorkerType {
            id: id.to_string(),
            tools: vec![],
            allowed_models: vec![],
            max_timeout_secs: None,
            agent_profile: None,
        }
    }

    fn st(id: &str) -> SubleadType {
        SubleadType {
            id: id.to_string(),
            tools: vec![],
            allowed_models: vec![],
            max_timeout_secs: None,
            max_budget_usd: None,
            agent_profile: None,
        }
    }

    /// Acceptance scenario from #385: two `[[worker_type]]`, one
    /// `[[sublead_type]]`, one unscoped MCP server, one server scoped
    /// to "writer". The untyped row plus both worker rows see the
    /// unscoped server; only the writer row also sees the scoped one;
    /// the reader and planner rows do not.
    #[test]
    fn renders_acceptance_scenario_from_issue() {
        let mut m = empty_manifest();
        m.mcp_servers = vec![srv("pitboss", None), srv("fs-writer", Some("type:writer"))];
        m.worker_types = vec![wt("writer"), wt("reader")];
        m.sublead_types = vec![st("planner")];

        let out = render(&m);
        let untyped_line = out
            .lines()
            .find(|l| l.starts_with("(untyped / root)"))
            .expect("untyped row missing");
        assert!(
            untyped_line.contains("pitboss") && !untyped_line.contains("fs-writer"),
            "untyped row must list only the unscoped server: {untyped_line}"
        );
        let writer_line = out
            .lines()
            .find(|l| l.starts_with("worker_type:writer"))
            .expect("writer row missing");
        assert!(
            writer_line.contains("pitboss") && writer_line.contains("fs-writer"),
            "writer row must list both servers: {writer_line}"
        );
        let reader_line = out
            .lines()
            .find(|l| l.starts_with("worker_type:reader"))
            .expect("reader row missing");
        assert!(
            reader_line.contains("pitboss") && !reader_line.contains("fs-writer"),
            "reader row must not list the writer-scoped server: {reader_line}"
        );
        let planner_line = out
            .lines()
            .find(|l| l.starts_with("sublead_type:planner"))
            .expect("planner row missing");
        assert!(
            !planner_line.contains("fs-writer"),
            "planner row must not list a worker-scoped server: {planner_line}"
        );
    }

    /// A manifest with no MCP servers must still render without
    /// panic — operators run validate on partial drafts and the
    /// `--capability-matrix` flag should be useful before any
    /// `[[mcp_server]]` is declared. Each row prints "(none)" so the
    /// emptiness is visible rather than a blank cell.
    #[test]
    fn renders_empty_servers_as_explicit_none() {
        let mut m = empty_manifest();
        m.worker_types = vec![wt("writer")];

        let out = render(&m);
        let writer_line = out
            .lines()
            .find(|l| l.starts_with("worker_type:writer"))
            .expect("writer row missing");
        assert!(
            writer_line.contains("(none)"),
            "empty server set must show as '(none)' so the empty cell is unambiguous: {writer_line}"
        );
    }

    /// An unscoped manifest (no `[[worker_type]]`/`[[sublead_type]]`)
    /// renders just the untyped row.
    #[test]
    fn renders_single_untyped_row_when_no_profiles_declared() {
        let mut m = empty_manifest();
        m.mcp_servers = vec![srv("pitboss", None)];

        let out = render(&m);
        assert!(out.contains("(untyped / root)"));
        assert!(
            !out.contains("worker_type:") && !out.contains("sublead_type:"),
            "must not emit profile rows when no profiles are declared: {out}"
        );
    }

    /// `rows()` exposes the structured matrix data the TUI Detail view
    /// and `pitboss-web` manifest panel will consume. Pin the row
    /// shape: untyped first, then declared profiles in declaration
    /// order, with `actor_type` set so consumers can match a tile's
    /// `TaskRecord.actor_type` directly.
    #[test]
    fn rows_emits_untyped_then_worker_then_sublead_in_declaration_order() {
        let mut m = empty_manifest();
        m.mcp_servers = vec![srv("pitboss", None), srv("fs-writer", Some("type:writer"))];
        m.worker_types = vec![wt("writer"), wt("reader")];
        m.sublead_types = vec![st("planner")];

        let rs = rows(&m);
        assert_eq!(rs.len(), 4, "expected 1 untyped + 2 worker + 1 sublead");

        assert_eq!(rs[0].kind, RowKind::Untyped);
        assert!(rs[0].actor_type.is_none());
        let untyped_ids: Vec<&str> = rs[0].servers.iter().map(|s| s.server_id.as_str()).collect();
        assert_eq!(untyped_ids, vec!["pitboss"]);

        assert_eq!(rs[1].kind, RowKind::WorkerType);
        assert_eq!(rs[1].actor_type.as_deref(), Some("writer"));
        assert_eq!(rs[1].label, "worker_type:writer");
        let writer_ids: Vec<&str> = rs[1].servers.iter().map(|s| s.server_id.as_str()).collect();
        assert_eq!(writer_ids, vec!["pitboss", "fs-writer"]);

        assert_eq!(rs[2].kind, RowKind::WorkerType);
        assert_eq!(rs[2].actor_type.as_deref(), Some("reader"));

        assert_eq!(rs[3].kind, RowKind::SubleadType);
        assert_eq!(rs[3].actor_type.as_deref(), Some("planner"));
    }

    /// `MatrixRow` ships over the `pitboss-web` validate endpoint's
    /// JSON response. Pin the wire shape so the SPA's TypeScript
    /// `MatrixRow` interface and any future external consumer don't
    /// silently drift when fields are renamed or `RowKind` variants
    /// are reshuffled. Snake-case `kind` discrimination is the
    /// contract; flipping to PascalCase would break the SPA.
    #[test]
    fn matrix_row_serialises_with_stable_field_and_kind_names() {
        let row = MatrixRow {
            label: "worker_type:writer".to_string(),
            actor_type: Some("writer".to_string()),
            kind: RowKind::WorkerType,
            servers: vec![
                MatrixServerEntry {
                    server_id: "pitboss".to_string(),
                    tools: None,
                },
                MatrixServerEntry {
                    server_id: "fs-writer".to_string(),
                    tools: Some(vec!["Read".to_string(), "Write".to_string()]),
                },
            ],
        };
        let json = serde_json::to_value(&row).expect("serialise");
        assert_eq!(json["label"], "worker_type:writer");
        assert_eq!(json["actor_type"], "writer");
        assert_eq!(json["kind"], "worker_type");
        assert_eq!(json["servers"][0]["server_id"], "pitboss");
        // `tools = None` must be omitted (not serialised as
        // `null`) so the SPA can branch on key-presence and keep its
        // TS shape `tools?: string[]` clean.
        assert!(
            !json["servers"][0]
                .as_object()
                .unwrap()
                .contains_key("tools"),
            "absent tools must be omitted from JSON, not null: {json}"
        );
        assert_eq!(json["servers"][1]["server_id"], "fs-writer");
        assert_eq!(
            json["servers"][1]["tools"],
            serde_json::json!(["Read", "Write"])
        );

        let untyped = MatrixRow {
            label: "(untyped / root)".to_string(),
            actor_type: None,
            kind: RowKind::Untyped,
            servers: vec![],
        };
        let json = serde_json::to_value(&untyped).expect("serialise untyped");
        assert!(json["actor_type"].is_null());
        assert_eq!(json["kind"], "untyped");
        assert_eq!(json["servers"], serde_json::json!([]));
    }

    /// `rows_from_parts` lets the TUI compute the matrix from a
    /// lightweight resolved.json deserialization without pulling in
    /// the full `ResolvedManifest` shape. Pin equivalence with the
    /// `ResolvedManifest`-driven path so the two stay in sync.
    #[test]
    fn rows_from_parts_matches_full_manifest_path() {
        let servers = vec![
            MatrixServerSpec {
                id: "pitboss".to_string(),
                scope: None,
                tools: None,
            },
            MatrixServerSpec {
                id: "fs-writer".to_string(),
                scope: Some("type:writer".to_string()),
                tools: Some(vec!["Read".to_string(), "Write".to_string()]),
            },
        ];
        let workers = vec!["writer".to_string()];
        let subleads: Vec<String> = vec![];

        let parts_rows = rows_from_parts(&servers, &workers, &subleads);

        let mut m = empty_manifest();
        m.mcp_servers = vec![
            srv("pitboss", None),
            srv_with_tools("fs-writer", Some("type:writer"), &["Read", "Write"]),
        ];
        m.worker_types = vec![wt("writer")];
        let manifest_rows = rows(&m);

        assert_eq!(parts_rows, manifest_rows);
    }

    /// A scoped server must be excluded from the untyped row even
    /// when no `[[worker_type]]` matches its scope. Otherwise an
    /// orphaned scoped server (operator typo, deleted profile) would
    /// silently widen what the lead sees.
    #[test]
    fn untyped_row_excludes_scoped_servers_with_no_matching_profile() {
        let mut m = empty_manifest();
        m.mcp_servers = vec![srv("orphan", Some("type:no-such-profile"))];

        let out = render(&m);
        let untyped_line = out
            .lines()
            .find(|l| l.starts_with("(untyped / root)"))
            .expect("untyped row missing");
        assert!(
            !untyped_line.contains("orphan"),
            "untyped row must not list orphaned scoped servers: {untyped_line}"
        );
    }

    /// A `[[mcp_server]]` with a `tools = […]` allowlist renders the
    /// tools on a continuation line under the server. Servers with no
    /// allowlist render bare on the same line. Pins the layout the
    /// CLI text formatter ships so operators piping the table into
    /// grep/awk get a stable two-section output.
    #[test]
    fn render_emits_per_server_tool_lists_on_continuation_lines() {
        let mut m = empty_manifest();
        m.mcp_servers = vec![
            srv("pitboss", None),
            srv_with_tools("fs-writer", Some("type:writer"), &["Read", "Write", "Edit"]),
        ];
        m.worker_types = vec![wt("writer")];

        let out = render(&m);
        // Writer row lists both servers comma-joined.
        let writer_line = out
            .lines()
            .find(|l| l.starts_with("worker_type:writer"))
            .expect("writer row missing");
        assert!(writer_line.contains("pitboss") && writer_line.contains("fs-writer"));

        // The tools continuation line lists the allowlist for fs-writer.
        let tools_line = out
            .lines()
            .find(|l| l.contains("fs-writer tools:"))
            .expect("fs-writer tools continuation line missing");
        assert!(
            tools_line.contains("Read")
                && tools_line.contains("Write")
                && tools_line.contains("Edit"),
            "expected continuation line listing fs-writer's tools allowlist: {tools_line}"
        );

        // Servers without an allowlist must not emit a continuation line.
        assert!(
            !out.contains("pitboss tools:"),
            "pitboss has no allowlist; must not render a 'tools:' continuation line: {out}"
        );
    }
}
