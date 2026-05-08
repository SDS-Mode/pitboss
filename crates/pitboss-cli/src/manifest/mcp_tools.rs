//! MCP tool name parsing against the declared `[[mcp_server]]` registry.
//!
//! Three callers want the same logic and must agree byte-for-byte:
//!
//! 1. `manifest::validate::validate_mcp_tool_consistency` — rejects an
//!    actor surface (`[lead].tools`, `[[task]].tools`, `[[worker_type]].tools`,
//!    `[[sublead_type]].tools`) that lists `mcp__<server>__<tool>` for a
//!    server with an allowlist excluding `<tool>`.
//! 2. `dispatch::runner::filter_actor_tools_by_mcp_allowlists` — at
//!    spawn time, drops `mcp__<server>__<tool>` from `--allowedTools`
//!    when the server's allowlist excludes it. Path B only — Path A
//!    bypasses the entire permission layer, so the allowlist has no
//!    runtime effect there. (#391 / #399)
//! 3. `mcp::tools::approval::handle_permission_prompt` — runtime gate
//!    that denies a routed `mcp__<server>__<tool>` call against the
//!    same allowlist.
//!
//! Each consumer needs longest-prefix-match against declared server
//! ids so `mcp__some_server__some_tool` resolves correctly when both
//! `some` and `some_server` are declared. Keeping the parser in one
//! place prevents drift between the three sites.

use crate::manifest::schema::{McpServerSpec, PermissionRouting};

/// A `mcp__<server>__<tool>` call resolved against an `[[mcp_server]]`
/// registry. Returned by [`parse_mcp_tool_name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedMcpTool<'a> {
    /// Declared server id matched by longest-prefix-match.
    pub server_id: &'a str,
    /// Tool name suffix after `mcp__<server>__`.
    pub tool_name: &'a str,
    /// Per-server tools allowlist (when the server declared one). The
    /// caller decides whether to enforce — `None` means "no restriction
    /// at this layer", `Some(slice)` means the tool must be in the
    /// slice to be admitted.
    pub allowlist: Option<&'a [String]>,
}

impl ParsedMcpTool<'_> {
    /// Returns true when the parsed tool is admitted by its server's
    /// allowlist (or when the server has no allowlist). Mirrors the
    /// runtime / spawn-time / validate-time admission decision so all
    /// three sites stay in lockstep.
    #[must_use]
    pub fn is_admitted(&self) -> bool {
        match self.allowlist {
            None => true,
            Some(allow) => allow.iter().any(|t| t == self.tool_name),
        }
    }
}

/// Parse a fully-qualified MCP tool name (`mcp__<server>__<tool>`)
/// against the declared `[[mcp_server]]` registry.
///
/// Returns `None` when:
/// - `name` doesn't start with `mcp__` (top-level claude tool, not MCP).
/// - The remainder doesn't match any declared server id followed by
///   `__` (server isn't in the manifest — out of scope for this gate).
/// - The longest-prefix match leaves an empty tool name (malformed).
///
/// Server-id matching uses **longest-prefix-match** so server ids
/// containing underscores resolve unambiguously: with both `fs` and
/// `fs_writer` declared, `mcp__fs_writer__read_file` resolves to
/// `fs_writer` + `read_file`, not `fs` + `writer__read_file`.
#[must_use]
pub fn parse_mcp_tool_name<'a>(
    name: &'a str,
    servers: &'a [McpServerSpec],
) -> Option<ParsedMcpTool<'a>> {
    let rest = name.strip_prefix("mcp__")?;
    // Iterate in descending id length so the longest match wins. We
    // could pre-sort the registry once on `ResolvedManifest`, but the
    // registry is small (operators rarely declare more than a handful
    // of servers) and the cost of sorting per call is negligible.
    let mut candidates: Vec<&McpServerSpec> = servers.iter().collect();
    candidates.sort_by_key(|s| std::cmp::Reverse(s.id.len()));
    for spec in candidates {
        let id = spec.id.as_str();
        if rest.len() > id.len() + 2 && rest.starts_with(id) && rest[id.len()..].starts_with("__") {
            let tool_name = &rest[id.len() + 2..];
            return Some(ParsedMcpTool {
                server_id: id,
                tool_name,
                allowlist: spec.tools.as_deref(),
            });
        }
    }
    None
}

/// Filter an actor's `tools` list against the per-server
/// `[[mcp_server]].tools` allowlists, returning a new vector that
/// drops any `mcp__<server>__<tool>` entry whose `<tool>` is not in
/// the server's allowlist.
///
/// **Path-B-only enforcement.** Under Path A
/// (`--dangerously-skip-permissions` / `bypassPermissions`), claude
/// skips the entire permission layer — `--allowedTools` is ignored,
/// non-listed tools are not blocked, and there is no `permission_prompt`
/// to deny through. Filtering at spawn time under Path A would change
/// nothing about runtime enforcement and would also mislead operators
/// who explicitly chose Path A as the "skip all gates" escape hatch.
/// So under Path A this returns the input unchanged.
///
/// Under Path B, dropping the entry from `--allowedTools` ensures the
/// call routes through `mcp__pitboss__permission_prompt` instead of
/// being silently auto-approved. The runtime gate
/// (`handle_permission_prompt` short-circuit) then issues the actual
/// deny — so this filter is the "stop pre-approving" half of the
/// two-step enforcement; the runtime gate is the "actually deny"
/// half. Both are needed: validate (PR #400) only constrains the
/// manifest, not programmatic spawns from `spawn_worker(tools = […])`,
/// so the filter is the source-of-truth for spawn-time argv.
///
/// (#391 / #399)
#[must_use]
pub fn filter_actor_tools_by_mcp_allowlists(
    tools: &[String],
    servers: &[McpServerSpec],
    routing: PermissionRouting,
) -> Vec<String> {
    if matches!(routing, PermissionRouting::PathA) {
        return tools.to_vec();
    }
    tools
        .iter()
        .filter(|entry| match parse_mcp_tool_name(entry, servers) {
            Some(parsed) => parsed.is_admitted(),
            None => true,
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::schema::McpServerSpec;

    fn srv(id: &str, tools: Option<Vec<&str>>) -> McpServerSpec {
        McpServerSpec {
            id: id.into(),
            command: "/bin/true".into(),
            args: vec![],
            env: Default::default(),
            scope: None,
            tools: tools.map(|v| v.into_iter().map(String::from).collect()),
        }
    }

    /// A fully-qualified MCP tool against a declared server resolves to
    /// the right (server, tool) pair and surfaces the allowlist.
    #[test]
    fn resolves_simple_mcp_tool_with_allowlist() {
        let servers = vec![srv("fs", Some(vec!["read_file"]))];
        let parsed = parse_mcp_tool_name("mcp__fs__read_file", &servers).expect("should parse");
        assert_eq!(parsed.server_id, "fs");
        assert_eq!(parsed.tool_name, "read_file");
        assert_eq!(parsed.allowlist.unwrap(), &["read_file".to_string()]);
        assert!(parsed.is_admitted());
    }

    /// Tool not in the server's allowlist parses fine but `is_admitted`
    /// returns false. Pin both: the parser doesn't pre-filter, the
    /// admission decision is the caller's.
    #[test]
    fn resolves_blocked_tool_but_marks_not_admitted() {
        let servers = vec![srv("fs", Some(vec!["read_file"]))];
        let parsed = parse_mcp_tool_name("mcp__fs__write_file", &servers).expect("should parse");
        assert_eq!(parsed.tool_name, "write_file");
        assert!(!parsed.is_admitted());
    }

    /// Server with no allowlist — `is_admitted` returns true regardless
    /// of tool name. Pins the opt-in semantic: only servers that
    /// declared `tools = […]` are constrained at this layer.
    #[test]
    fn server_without_allowlist_admits_any_tool() {
        let servers = vec![srv("fs", None)];
        let parsed = parse_mcp_tool_name("mcp__fs__delete_all", &servers).expect("should parse");
        assert!(parsed.allowlist.is_none());
        assert!(parsed.is_admitted());
    }

    /// Longest-prefix-match: with `fs` and `fs_writer` both declared,
    /// `mcp__fs_writer__read_file` must resolve to `fs_writer`, not
    /// `fs`. This is the crucial property that decoupling parser from
    /// the consumers protects.
    #[test]
    fn longest_prefix_match_resolves_underscored_server_ids() {
        let servers = vec![
            srv("fs", Some(vec!["never_called"])),
            srv("fs_writer", Some(vec!["read_file"])),
        ];
        let parsed = parse_mcp_tool_name("mcp__fs_writer__read_file", &servers)
            .expect("should parse to fs_writer");
        assert_eq!(parsed.server_id, "fs_writer");
        assert_eq!(parsed.tool_name, "read_file");
        assert!(parsed.is_admitted());
    }

    /// Top-level claude tool (no `mcp__` prefix) returns `None` —
    /// the caller falls through.
    #[test]
    fn returns_none_for_non_mcp_tool() {
        let servers = vec![srv("fs", Some(vec!["read_file"]))];
        assert!(parse_mcp_tool_name("Read", &servers).is_none());
        assert!(parse_mcp_tool_name("Bash(git status)", &servers).is_none());
    }

    /// Fully-qualified MCP tool referencing an undeclared server
    /// returns `None`. The gate only constrains servers the operator
    /// actually declared in `[[mcp_server]]`.
    #[test]
    fn returns_none_for_undeclared_server() {
        let servers = vec![srv("fs", Some(vec!["read_file"]))];
        assert!(parse_mcp_tool_name("mcp__other_server__some_tool", &servers).is_none());
    }

    /// Malformed names (`mcp__server__` with empty tool name, or
    /// `mcp__only_one_segment`) return `None` rather than panicking
    /// or returning an empty `tool_name`. Pin so the spawn-time and
    /// runtime callers don't have to defensive-check.
    #[test]
    fn returns_none_for_malformed_names() {
        let servers = vec![srv("fs", Some(vec!["read_file"]))];
        assert!(parse_mcp_tool_name("mcp__fs__", &servers).is_none());
        assert!(parse_mcp_tool_name("mcp__fs", &servers).is_none());
        assert!(parse_mcp_tool_name("mcp__", &servers).is_none());
    }

    /// Filter under Path B drops a non-allowlisted MCP tool while
    /// preserving every other entry (top-level claude tools, allowlisted
    /// MCP tools, MCP tools for servers without an allowlist).
    #[test]
    fn filter_under_path_b_drops_non_allowlisted_mcp_tool() {
        let servers = vec![srv("fs", Some(vec!["read_file"]))];
        let tools = vec![
            "Read".to_string(),
            "Bash".to_string(),
            "mcp__fs__read_file".to_string(),
            "mcp__fs__write_file".to_string(),
        ];
        let filtered =
            filter_actor_tools_by_mcp_allowlists(&tools, &servers, PermissionRouting::PathB);
        assert!(filtered.iter().any(|t| t == "Read"));
        assert!(filtered.iter().any(|t| t == "Bash"));
        assert!(filtered.iter().any(|t| t == "mcp__fs__read_file"));
        assert!(
            !filtered.iter().any(|t| t == "mcp__fs__write_file"),
            "non-allowlisted MCP tool must be dropped under Path B: got {filtered:?}"
        );
    }

    /// **Load-bearing contract pin (advisor-flagged):** under Path A,
    /// the filter is a no-op. The allowlist has zero runtime effect
    /// because Path A bypasses the entire permission layer
    /// (`--dangerously-skip-permissions`); filtering `--allowedTools`
    /// would change nothing about enforcement (claude doesn't gate on
    /// `--allowedTools` under Path A) AND would mislead operators who
    /// chose Path A explicitly as the "skip all gates" escape hatch.
    /// A future "let's just filter under both paths for safety" refactor
    /// would silently break this contract — this test catches it. (#391
    /// / #399)
    #[test]
    fn filter_under_path_a_is_a_no_op() {
        let servers = vec![srv("fs", Some(vec!["read_file"]))];
        let tools = vec!["Read".to_string(), "mcp__fs__write_file".to_string()];
        let filtered =
            filter_actor_tools_by_mcp_allowlists(&tools, &servers, PermissionRouting::PathA);
        assert_eq!(
            filtered, tools,
            "under Path A the filter must be a no-op — Path A is the documented \
             'skip all gates' escape hatch and per-server allowlists have no \
             runtime effect there"
        );
    }

    /// Servers without a `tools` allowlist must not have any of their
    /// tools filtered. Pin the opt-in semantic at the filter layer.
    #[test]
    fn filter_skips_servers_without_allowlist() {
        let servers = vec![srv("fs", None)];
        let tools = vec![
            "mcp__fs__delete_all".to_string(),
            "mcp__fs__anything".to_string(),
        ];
        let filtered =
            filter_actor_tools_by_mcp_allowlists(&tools, &servers, PermissionRouting::PathB);
        assert_eq!(filtered, tools);
    }
}
