//! Hand-curated starter manifests for `pitboss init`.
//!
//! Two tiers, kept deliberately small and prescriptive:
//!
//! * [`InitTemplate::Simple`] — one root `[lead]` driving a flat worker
//!   pool. Two levels deep at most. The 80% case for new operators.
//! * [`InitTemplate::Full`] — coordinator + sub-leads + workers + KV/budget
//!   fields, with the optional sections (`[[mcp_server]]`,
//!   `[[approval_policy]]`, `[[template]]`, `[container]`) commented out
//!   so they survive copy-paste editing.
//!
//! Why hand-curated rather than registry-derived: a template is opinionated
//! teaching code. It needs comments, sensible defaults, and a "fill these
//! placeholders" through-line that the [`super::example_doc`] reference
//! generator (PR 1.D) intentionally lacks. The drift guard tests below run
//! each template through `load_manifest_from_str` so a v0.9 schema change
//! that breaks a template surfaces immediately.

/// Which canned manifest to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitTemplate {
    Simple,
    Full,
}

impl InitTemplate {
    /// Render the template body as TOML. Returns a `&'static str` because
    /// both templates are compile-time embedded — no per-call allocation.
    pub fn render(self) -> &'static str {
        match self {
            Self::Simple => SIMPLE,
            Self::Full => FULL,
        }
    }

    /// Short slug suitable for filenames / log output / regenerate hints.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::Full => "full",
        }
    }
}

const SIMPLE: &str = r#"# Pitboss manifest — simple template.
#
# One root [lead] driving a flat worker pool (no sub-leads). Edit the
# placeholders below, then run `pitboss validate <this-file>` to verify.
# For every available field, see `docs/manifest-reference.toml`.

[run]
worktree_cleanup = "on_success"

[defaults]
model        = "claude-sonnet-4-6"
effort       = "medium"
tools        = ["Read", "Grep", "Glob", "Bash"]
use_worktree = true

[lead]
id        = "coordinator"
directory = "/path/to/your/project"
prompt    = """
Replace this with operator instructions for the lead.
Describe the goal, constraints, and how to delegate to workers.
"""

# Lead-level caps (required when the lead spawns workers).
max_workers       = 4
budget_usd        = 5.00
lead_timeout_secs = 1800
"#;

const FULL: &str = r#"# Pitboss manifest — full template.
#
# Coordinator + sub-leads + workers, with depth-2 controls and
# [sublead_defaults] populated. Optional sections (MCP servers,
# approval policy, prompt templates, container dispatch) are
# commented at the bottom — uncomment as needed.
#
# Edit the placeholders below, then run `pitboss validate <this-file>`.
# For every available field, see `docs/manifest-reference.toml`.

[run]
worktree_cleanup      = "on_success"
emit_event_stream     = true
require_plan_approval = false  # Set true to gate spawn_worker on propose_plan.

[defaults]
model        = "claude-sonnet-4-6"
effort       = "medium"
tools        = ["Read", "Grep", "Glob", "Bash"]
use_worktree = true
timeout_secs = 1800

[lead]
id        = "coordinator"
directory = "/path/to/your/project"
prompt    = """
Replace this with operator instructions for the root lead.
Plan the work via propose_plan, then delegate to sub-leads
(one per work-stream) and workers as appropriate.
"""

# Lead-level caps.
max_workers       = 8
budget_usd        = 20.00
lead_timeout_secs = 7200

# Depth-2 controls.
allow_subleads         = true
max_subleads           = 3
max_sublead_budget_usd = 5.00
max_total_workers      = 16

[sublead_defaults]
budget_usd        = 5.00
max_workers       = 4
lead_timeout_secs = 3600
read_down         = false  # When true, sub-leads share the root's pool.

# ── Optional sections (uncomment as needed) ─────────────────────────────

# [[mcp_server]]
# id      = "context7"
# command = "npx"
# args    = ["-y", "@upstash/context7-mcp"]

# [[approval_policy]]
# action = "auto_approve"
# match  = { actor = "root", category = "tool_use" }

# [[template]]
# id     = "audit"
# prompt = "Audit {pkg} in {dir}."

# [container]
# image   = "ghcr.io/sds-mode/pitboss-with-goose:latest"
# runtime = "auto"
#
# [[container.mount]]
# host      = "/path/to/project"
# container = "/home/pitboss/project"
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::load::load_manifest_from_str;

    /// Drift guard: each template must parse + resolve under the v0.9
    /// schema. If a future schema change breaks a template, this test
    /// flags it before the broken `init` output reaches a user.
    #[test]
    fn simple_template_parses_and_resolves() {
        let r = load_manifest_from_str(InitTemplate::Simple.render())
            .expect("simple template should parse and resolve");
        let lead = r.lead.expect("simple template must have a [lead]");
        assert_eq!(lead.id, "coordinator");
        assert_eq!(r.max_workers, Some(4));
        assert_eq!(r.budget_usd, Some(5.00));
    }

    /// Same drift guard for the full template, with extra checks on the
    /// depth-2 fields the simple template omits.
    #[test]
    fn full_template_parses_and_resolves() {
        let r = load_manifest_from_str(InitTemplate::Full.render())
            .expect("full template should parse and resolve");
        let lead = r.lead.expect("full template must have a [lead]");
        assert_eq!(lead.id, "coordinator");
        assert_eq!(r.max_workers, Some(8));
        assert_eq!(r.budget_usd, Some(20.00));
        assert!(
            lead.allow_subleads,
            "full template should set allow_subleads = true"
        );
        assert_eq!(lead.max_subleads, Some(3));
        assert_eq!(lead.max_total_workers, Some(16));
        let sd = lead
            .sublead_defaults
            .as_ref()
            .expect("full template must declare [sublead_defaults]");
        assert_eq!(sd.max_workers, Some(4));
    }

    /// Templates must not contain v0.8 fields. A copy-paste regression
    /// where the wrong field name lands in a template would silently
    /// reach users — guard against the ones that changed in v0.9.
    #[test]
    fn templates_avoid_legacy_field_names() {
        const FORBIDDEN: &[&str] = &[
            "max_workers_across_tree", // → max_total_workers
            "max_parallel ", // → max_parallel_tasks (note trailing space anchors the key, not max_parallel_tasks)
            "approval_policy =", // [run] field; renamed to default_approval_policy
            "[lead.sublead_defaults]", // promoted to top-level [sublead_defaults]
            "[[lead]]",      // array form is gone
        ];
        for kind in [InitTemplate::Simple, InitTemplate::Full] {
            let body = kind.render();
            for needle in FORBIDDEN {
                assert!(
                    !body.contains(needle),
                    "{} template contains legacy fragment {:?}",
                    kind.slug(),
                    needle
                );
            }
        }
    }

    /// Each template must explain itself — a header comment, the
    /// `pitboss validate` reminder, and a pointer to the reference doc.
    /// Catches the classic regression where someone strips comments to
    /// "clean up" the template.
    #[test]
    fn templates_have_orientation_comments() {
        for kind in [InitTemplate::Simple, InitTemplate::Full] {
            let body = kind.render();
            assert!(
                body.contains("pitboss validate"),
                "{} template should mention `pitboss validate`",
                kind.slug()
            );
            assert!(
                body.contains("docs/manifest-reference.toml"),
                "{} template should point at the reference doc",
                kind.slug()
            );
        }
    }

    /// Slug stability — these strings show up in the CLI's clap value-enum
    /// and in error messages, so a rename here is a user-visible change.
    #[test]
    fn template_slugs_are_stable() {
        assert_eq!(InitTemplate::Simple.slug(), "simple");
        assert_eq!(InitTemplate::Full.slug(), "full");
    }
}
