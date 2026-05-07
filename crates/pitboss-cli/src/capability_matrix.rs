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

/// Render the matrix as a multi-line string ending in `\n`.
///
/// Row order:
/// 1. `(untyped / root)` — what the root lead and any un-profiled
///    spawn sees. Always first; operators reading the table want the
///    "default" row anchored at the top.
/// 2. One row per `[[worker_type]]`, in declaration order.
/// 3. One row per `[[sublead_type]]`, in declaration order.
///
/// `[[mcp_server]]` ids inside each row are emitted in declaration
/// order so two manifests producing the same matrix shape compare
/// stable diff-wise.
pub fn render(manifest: &ResolvedManifest) -> String {
    let server_ids: Vec<&str> = manifest.mcp_servers.iter().map(|s| s.id.as_str()).collect();
    let server_scopes: Vec<Option<&str>> = manifest
        .mcp_servers
        .iter()
        .map(|s| s.scope.as_deref())
        .collect();

    // Pre-compute the matrix as Vec<(label, Vec<&str>)>. The widest
    // label drives the first column's width so multi-row output stays
    // aligned without an external table crate.
    let mut rows: Vec<(String, Vec<&str>)> = Vec::new();
    rows.push((
        "(untyped / root)".to_string(),
        admitted_servers(&server_ids, &server_scopes, None),
    ));
    for wt in &manifest.worker_types {
        rows.push((
            format!("worker_type:{}", wt.id),
            admitted_servers(&server_ids, &server_scopes, Some(wt.id.as_str())),
        ));
    }
    for st in &manifest.sublead_types {
        rows.push((
            format!("sublead_type:{}", st.id),
            admitted_servers(&server_ids, &server_scopes, Some(st.id.as_str())),
        ));
    }

    let label_width = rows
        .iter()
        .map(|(l, _)| l.len())
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
    for (label, ids) in &rows {
        let cell = if ids.is_empty() {
            "(none)".to_string()
        } else {
            ids.join(", ")
        };
        out.push_str(&format!("{label:<label_width$}  {cell}\n"));
    }
    out
}

/// Compute the subset of server ids that scope-admit for a given
/// `actor_type` (`None` for the untyped / root row). Preserves
/// declaration order — the upstream zip of ids with scopes is
/// position-aligned by `render`'s caller.
fn admitted_servers<'a>(
    ids: &[&'a str],
    scopes: &[Option<&str>],
    actor_type: Option<&str>,
) -> Vec<&'a str> {
    ids.iter()
        .zip(scopes.iter())
        .filter_map(|(id, scope)| {
            if mcp_server_scope_admits(*scope, actor_type) {
                Some(*id)
            } else {
                None
            }
        })
        .collect()
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
