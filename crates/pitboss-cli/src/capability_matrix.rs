//! `pitboss validate --capability-matrix` formatter (#385).
//!
//! Walks the resolved manifest's `[[mcp_server]]` list and computes,
//! for each declared `[[worker_type]]` / `[[sublead_type]]` (plus an
//! "untyped / root" row representing the root lead and any
//! pre-#252-style un-profiled spawns), the set of MCP server ids that
//! pitboss would inject into actors of that class.
//!
//! Reuses the same admission helper as the runtime injection
//! (`crate::dispatch::hierarchical::mcp_server_scope_admits`) so the
//! report output and the live behavior cannot drift.
//!
//! The output is plain text — one row per actor class, server ids as
//! a comma-joined list. No JSON output here yet; operators piping
//! the table into grep/awk get a stable column structure.

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

/// One row of the actor-type × MCP-server matrix.
///
/// `actor_type` is `None` for the untyped/root row and `Some(id)` for
/// declared profiles; consumers that want to match a tile's
/// `actor_type` directly check `row.actor_type.as_deref() ==
/// tile.actor_type.as_deref()`.
///
/// `server_ids` is in manifest declaration order so two manifests
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
    /// MCP server ids that scope-admit for this row, in manifest
    /// declaration order.
    pub server_ids: Vec<String>,
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
    let servers: Vec<(String, Option<String>)> = manifest
        .mcp_servers
        .iter()
        .map(|s| (s.id.clone(), s.scope.clone()))
        .collect();
    let wt_ids: Vec<String> = manifest.worker_types.iter().map(|w| w.id.clone()).collect();
    let st_ids: Vec<String> = manifest
        .sublead_types
        .iter()
        .map(|s| s.id.clone())
        .collect();
    rows_from_parts(&servers, &wt_ids, &st_ids)
}

/// Compute matrix rows from the minimal data the algorithm needs. The
/// TUI deserializes a lightweight subset of `resolved.json` and feeds
/// it in here without having to construct a full `ResolvedManifest` —
/// keeps the matrix logic decoupled from the larger manifest type.
pub fn rows_from_parts(
    servers: &[(String, Option<String>)],
    worker_type_ids: &[String],
    sublead_type_ids: &[String],
) -> Vec<MatrixRow> {
    let mut out: Vec<MatrixRow> =
        Vec::with_capacity(1 + worker_type_ids.len() + sublead_type_ids.len());
    out.push(MatrixRow {
        label: "(untyped / root)".to_string(),
        actor_type: None,
        kind: RowKind::Untyped,
        server_ids: admitted_for(servers, None),
    });
    for id in worker_type_ids {
        out.push(MatrixRow {
            label: format!("worker_type:{id}"),
            actor_type: Some(id.clone()),
            kind: RowKind::WorkerType,
            server_ids: admitted_for(servers, Some(id.as_str())),
        });
    }
    for id in sublead_type_ids {
        out.push(MatrixRow {
            label: format!("sublead_type:{id}"),
            actor_type: Some(id.clone()),
            kind: RowKind::SubleadType,
            server_ids: admitted_for(servers, Some(id.as_str())),
        });
    }
    out
}

fn admitted_for(servers: &[(String, Option<String>)], actor_type: Option<&str>) -> Vec<String> {
    servers
        .iter()
        .filter_map(|(id, scope)| {
            if mcp_server_scope_admits(scope.as_deref(), actor_type) {
                Some(id.clone())
            } else {
                None
            }
        })
        .collect()
}

/// Render the matrix as a multi-line string ending in `\n`.
///
/// Thin formatter on top of [`rows`]. The widest label drives the
/// first column's width so multi-row output stays aligned without an
/// external table crate.
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
    for row in &rows {
        let cell = if row.server_ids.is_empty() {
            "(none)".to_string()
        } else {
            row.server_ids.join(", ")
        };
        let label = &row.label;
        out.push_str(&format!("{label:<label_width$}  {cell}\n"));
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
            tasks: vec![],
            lead: None,
            max_workers: None,
            budget_usd: None,
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

    fn wt(id: &str) -> WorkerType {
        WorkerType {
            id: id.to_string(),
            tools: vec![],
            allowed_models: vec![],
            max_timeout_secs: None,
        }
    }

    fn st(id: &str) -> SubleadType {
        SubleadType {
            id: id.to_string(),
            tools: vec![],
            allowed_models: vec![],
            max_timeout_secs: None,
            max_budget_usd: None,
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
        // Anchor row: contains pitboss (unscoped) but not the
        // writer-scoped server. Asserting on substring rather than the
        // full padded prefix because the label column width is driven
        // by the widest label in the matrix.
        let untyped_line = out
            .lines()
            .find(|l| l.starts_with("(untyped / root)"))
            .expect("untyped row missing");
        assert!(
            untyped_line.contains("pitboss") && !untyped_line.contains("fs-writer"),
            "untyped row must list only the unscoped server: {untyped_line}"
        );
        // Scoped server appears only on the matching row.
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
    /// renders just the untyped row. Verifies the helper's edge case:
    /// the matrix is still useful for un-migrated manifests, even
    /// though it collapses to a single row.
    #[test]
    fn renders_single_untyped_row_when_no_profiles_declared() {
        let mut m = empty_manifest();
        m.mcp_servers = vec![srv("pitboss", None)];

        let out = render(&m);
        assert!(out.contains("(untyped / root)"));
        // No worker_type or sublead_type rows.
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
        assert_eq!(rs[0].server_ids, vec!["pitboss".to_string()]);

        assert_eq!(rs[1].kind, RowKind::WorkerType);
        assert_eq!(rs[1].actor_type.as_deref(), Some("writer"));
        assert_eq!(rs[1].label, "worker_type:writer");
        // writer-scoped server admits for writer.
        assert_eq!(
            rs[1].server_ids,
            vec!["pitboss".to_string(), "fs-writer".to_string()]
        );

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
            server_ids: vec!["pitboss".to_string(), "fs-writer".to_string()],
        };
        let json = serde_json::to_value(&row).expect("serialise");
        assert_eq!(json["label"], "worker_type:writer");
        assert_eq!(json["actor_type"], "writer");
        assert_eq!(json["kind"], "worker_type");
        assert_eq!(
            json["server_ids"],
            serde_json::json!(["pitboss", "fs-writer"])
        );

        // Untyped row also pins the `null` shape for actor_type and the
        // `"untyped"` kind discriminant so SPA TS code can branch on
        // either field.
        let untyped = MatrixRow {
            label: "(untyped / root)".to_string(),
            actor_type: None,
            kind: RowKind::Untyped,
            server_ids: vec![],
        };
        let json = serde_json::to_value(&untyped).expect("serialise untyped");
        assert!(json["actor_type"].is_null());
        assert_eq!(json["kind"], "untyped");
    }

    /// `rows_from_parts` lets the TUI compute the matrix from a
    /// lightweight resolved.json deserialization without pulling in
    /// the full `ResolvedManifest` shape. Pin equivalence with the
    /// `ResolvedManifest`-driven path so the two stay in sync.
    #[test]
    fn rows_from_parts_matches_full_manifest_path() {
        let servers = vec![
            ("pitboss".to_string(), None),
            ("fs-writer".to_string(), Some("type:writer".to_string())),
        ];
        let workers = vec!["writer".to_string()];
        let subleads: Vec<String> = vec![];

        let parts_rows = rows_from_parts(&servers, &workers, &subleads);

        let mut m = empty_manifest();
        m.mcp_servers = vec![srv("pitboss", None), srv("fs-writer", Some("type:writer"))];
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
}
