//! MCP tool handlers for `analyze_run` / `analyze_recent`.
//!
//! Read-only triage surface over the per-run artifact tree. Resolves
//! against the canonical runs base directory
//! ([`crate::runs::runs_base_dir`]) — no `run_dir` override on the MCP
//! surface, so callers can't enumerate arbitrary paths through the tool.
//! Operators with a non-default runs root use the CLI's `--run-dir` flag
//! instead.

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::analyze::{
    analyze_recent_dirs, analyze_run_dir, RecentAnalysis, RunAnalysis, MAX_RECENT_LIMIT,
};
use crate::runs::{resolve_run_dir_by_prefix, runs_base_dir};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AnalyzeRunArgs {
    /// Run id (full UUID or unique prefix) to triage.
    pub run_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct AnalyzeRecentArgs {
    /// Number of most-recent runs to walk. Defaults to 10 when omitted;
    /// clamped to [`MAX_RECENT_LIMIT`] (50) when too large.
    #[serde(default)]
    pub limit: Option<u32>,
    /// When `true`, skip runs with zero failed tasks. Useful for
    /// cross-run failure-mode debugging without wading through
    /// successful runs.
    #[serde(default)]
    pub failed_only: bool,
}

pub fn handle_analyze_run(args: AnalyzeRunArgs) -> Result<RunAnalysis> {
    let base = runs_base_dir();
    let run_dir = resolve_run_dir_by_prefix(&base, &args.run_id)?;
    analyze_run_dir(&run_dir)
}

pub fn handle_analyze_recent(args: AnalyzeRecentArgs) -> Result<RecentAnalysis> {
    let base = runs_base_dir();
    let limit = args.limit.unwrap_or(10).clamp(1, MAX_RECENT_LIMIT);
    analyze_recent_dirs(&base, limit, args.failed_only)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_recent_args_default_failed_only_false() {
        let parsed: AnalyzeRecentArgs = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.limit, None);
        assert!(!parsed.failed_only);
    }

    #[test]
    fn analyze_recent_args_round_trip() {
        let parsed: AnalyzeRecentArgs =
            serde_json::from_str(r#"{"limit": 25, "failed_only": true}"#).unwrap();
        assert_eq!(parsed.limit, Some(25));
        assert!(parsed.failed_only);
    }
}
