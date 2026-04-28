//! Depth-2 invariant enforcement.
//!
//! Pitboss caps the actor hierarchy at two levels: **root lead → sub-leads →
//! workers**. Workers cannot spawn anything; sub-leads cannot spawn further
//! sub-leads. Only the root lead can call `spawn_sublead`. The cap is
//! enforced at four independent points to provide defense-in-depth:
//!
//! 1. **CLI allowlist** — sub-lead claude subprocesses are launched with
//!    `--allowedTools` deliberately excluding [`ROOT_ONLY_TOOLS`]
//!    ([`crate::dispatch::runner::SUBLEAD_MCP_TOOLS`] is the static
//!    allowlist).
//! 2. **MCP `list_tools` filter** — root-only tools are hidden from the
//!    advertised toolset unless the manifest declares `allow_subleads = true`
//!    AND the connected actor is the root lead.
//! 3. **Manifest capability check** — even if the wire bypasses (1) and (2),
//!    [`validate_spawn_sublead_capability`] re-checks the manifest at
//!    handler-entry time.
//! 4. **Caller role check** — [`validate_spawn_sublead_caller`] rejects any
//!    caller whose `actor_role` is not `root_lead` / `lead`.
//!
//! This module owns the rules so a future tool addition cannot miss a layer.
//! **To add a new "root-lead-only" tool**: append its bare name (without the
//! `mcp__pitboss__` prefix) to [`ROOT_ONLY_TOOLS`]. The `list_tools` filter
//! and [`assert_sublead_toolset_excludes_root_only`] (regression-tested) will
//! pick it up automatically. You also need to remove it from
//! [`crate::dispatch::runner::SUBLEAD_MCP_TOOLS`] if it was ever added there.

use thiserror::Error;

use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};

/// Bare MCP tool names (without the `mcp__pitboss__` prefix) that only the
/// root lead may invoke. Sub-leads and workers must not see these in
/// `list_tools` and must not be able to invoke them.
///
/// **This is the single source of truth** for the depth-2 invariant.
/// Adding a tool here is the only edit required to extend enforcement
/// across the four gating sites listed in this module's doc.
pub const ROOT_ONLY_TOOLS: &[&str] = &["spawn_sublead"];

/// Returns true when `name` (a bare tool name, e.g. `"spawn_sublead"`)
/// is restricted to the root lead. Used by the `list_tools` server-side
/// filter to hide depth-2-violating tools from sub-leads and workers.
#[must_use]
pub fn is_root_only_tool(name: &str) -> bool {
    ROOT_ONLY_TOOLS.contains(&name)
}

/// Errors raised by depth-2 invariant checks. Distinct from the broader
/// MCP error space because each variant maps to a *specific* invariant
/// violation that an operator can act on.
#[derive(Debug, Error)]
pub enum DepthError {
    /// `spawn_sublead` invoked by an actor whose role is not `root_lead` /
    /// `lead`. This is the depth-2 invariant proper: sub-leads and workers
    /// cannot spawn sub-leads.
    #[error(
        "spawn_sublead is only available to the root lead (got role: {role}; depth-2 invariant: workers and sub-leads cannot spawn sub-leads)"
    )]
    DisallowedSubleadSpawnCaller { role: String },

    /// `spawn_sublead` invoked but the manifest's `[lead]` block declares
    /// `allow_subleads = false` (or omits the field). The capability is
    /// opt-in to keep simple flat-mode hierarchical runs from accidentally
    /// growing sub-trees.
    #[error("spawn_sublead requires allow_subleads=true in the manifest [lead] block")]
    AllowSubleadsDisabled,

    /// `spawn_sublead` invoked on a non-hierarchical run. Should not happen
    /// in practice (the tool is hidden via `list_tools` for flat-mode
    /// dispatches) but we surface a typed variant for completeness.
    #[error("spawn_sublead requires a hierarchical manifest (no [lead] declared)")]
    NotHierarchical,
}

/// Validate that the caller's `actor_role` permits calling `spawn_sublead`.
/// Per the depth-2 invariant, only `root_lead` / `lead` may. Sub-leads and
/// workers are rejected with [`DepthError::DisallowedSubleadSpawnCaller`].
pub fn validate_spawn_sublead_caller(role: &str) -> Result<(), DepthError> {
    match role {
        "root_lead" | "lead" => Ok(()),
        other => Err(DepthError::DisallowedSubleadSpawnCaller {
            role: other.to_string(),
        }),
    }
}

/// Validate that the manifest permits sub-lead spawning, returning a
/// reference to the `[lead]` block on success. Useful for handlers that
/// need to read sub-lead-related fields immediately after the check.
pub fn validate_spawn_sublead_capability(
    manifest: &ResolvedManifest,
) -> Result<&ResolvedLead, DepthError> {
    let lead = manifest.lead.as_ref().ok_or(DepthError::NotHierarchical)?;
    if !lead.allow_subleads {
        return Err(DepthError::AllowSubleadsDisabled);
    }
    Ok(lead)
}

/// Compile/test-time invariant: every entry in [`ROOT_ONLY_TOOLS`] must be
/// **absent** from the sub-lead allowlist. Returns the offending names so a
/// failing regression test can tell the contributor exactly which tool
/// slipped in.
#[doc(hidden)]
#[must_use]
pub fn assert_sublead_toolset_excludes_root_only(sublead_allowlist: &[&str]) -> Vec<&'static str> {
    let mut leaks: Vec<&'static str> = Vec::new();
    for &bare in ROOT_ONLY_TOOLS {
        let prefixed = format!("mcp__pitboss__{bare}");
        if sublead_allowlist.iter().any(|t| *t == prefixed) {
            leaks.push(bare);
        }
    }
    leaks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::runner::{PITBOSS_MCP_TOOLS, SUBLEAD_MCP_TOOLS};

    #[test]
    fn is_root_only_tool_recognises_known_entries() {
        assert!(is_root_only_tool("spawn_sublead"));
        assert!(!is_root_only_tool("spawn_worker"));
        assert!(!is_root_only_tool("kv_get"));
    }

    /// Regression: the static `SUBLEAD_MCP_TOOLS` allowlist (used to
    /// construct sub-lead claude subprocess `--allowedTools`) must never
    /// contain any tool that `ROOT_ONLY_TOOLS` declares as root-only. If
    /// this fails, a sub-lead would be able to invoke a depth-2-violating
    /// tool from the CLI side even though the MCP server's `list_tools`
    /// filter would still hide it (defense-in-depth would degrade to
    /// single-line-of-defense).
    #[test]
    fn sublead_allowlist_excludes_all_root_only_tools() {
        let leaks = assert_sublead_toolset_excludes_root_only(SUBLEAD_MCP_TOOLS);
        assert!(
            leaks.is_empty(),
            "SUBLEAD_MCP_TOOLS leaks root-only tools: {leaks:?}. \
             Either remove them from SUBLEAD_MCP_TOOLS or remove them from \
             ROOT_ONLY_TOOLS — they are mutually exclusive by design."
        );
    }

    /// Regression: the root-lead base allowlist (`PITBOSS_MCP_TOOLS`) must
    /// also exclude root-only tools by default. Spawn paths that need to
    /// permit them (only the root lead, only when `allow_subleads = true`)
    /// add them explicitly via `runner::root_lead_allowed_tools` — see
    /// `runner.rs` argv builders. This keeps the static const honest.
    #[test]
    fn pitboss_base_allowlist_excludes_root_only_tools() {
        let leaks = assert_sublead_toolset_excludes_root_only(PITBOSS_MCP_TOOLS);
        assert!(
            leaks.is_empty(),
            "PITBOSS_MCP_TOOLS unexpectedly contains root-only tools: {leaks:?}. \
             Root-only tools must be added conditionally at the spawn site, not \
             baked into the static base."
        );
    }

    #[test]
    fn validate_spawn_sublead_caller_accepts_root_lead() {
        assert!(validate_spawn_sublead_caller("root_lead").is_ok());
        assert!(validate_spawn_sublead_caller("lead").is_ok());
    }

    #[test]
    fn validate_spawn_sublead_caller_rejects_sublead_and_worker() {
        let err = validate_spawn_sublead_caller("sublead").unwrap_err();
        assert!(
            matches!(err, DepthError::DisallowedSubleadSpawnCaller { ref role } if role == "sublead")
        );
        let err = validate_spawn_sublead_caller("worker").unwrap_err();
        assert!(
            matches!(err, DepthError::DisallowedSubleadSpawnCaller { ref role } if role == "worker")
        );
    }
}
