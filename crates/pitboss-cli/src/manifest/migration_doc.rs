//! Starter `[[worker_type]]` / `[[sublead_type]]` profile scaffold for
//! `pitboss schema --format=migration` (#252). Operators selecting Path B
//! without any profiles see a validate-time warning pointing them here;
//! the output is a hand-editable TOML block with comments explaining
//! each field's role.
//!
//! The output is deliberately STATIC — derived from neither the
//! manifest under inspection nor any past `summary.jsonl`. A v2 could
//! read the live manifest's `[lead].tools` cascade and produce a
//! tailored scaffold; for now the static form is enough to give
//! operators a copy-paste starting point with the right field names
//! and a working example of every cap.

/// Render the migration scaffold. Pure function — same output every
/// call. The leading hash-banner names the command that produced it
/// so a `--check`-style drift test (if added later) can ignore the
/// header line; the trailing newline matches the `--format=example`
/// convention.
pub fn render() -> String {
    String::from(
        r#"# pitboss schema --format=migration
#
# Starter [[worker_type]] / [[sublead_type]] profiles. Drop these
# blocks into your manifest to declare per-class capability caps that
# pitboss enforces at spawn time AND auto-approves under Path B
# (#252).
#
# How profiles compose with permission_routing:
#
#   [[approval_policy]] rules first  (operator override, cross-cutting)
#   profile.tools allowlist          (this section — the manifest is
#                                      the consent signal: tools in
#                                      the list ⇒ auto-approve, tools
#                                      not in the list ⇒ auto-deny via
#                                      `denied_by_profile`)
#   bridge fallback                  (un-typed callers; pre-#252
#                                      behavior)
#
# Workflow:
#
#   1) Pick names that describe each class's job (`extraction`,
#      `writer`, `planner`). `default` is fine for a single-class run.
#   2) Copy the matching block into your manifest.
#   3) Reference it: `spawn_worker(worker_type = "default", ...)` or
#      `spawn_sublead(sublead_type = "planner", ...)`. Pitboss enforces
#      the caps at spawn; the lead can never widen them.
#   4) Once every spawn site is typed, set
#      `[run].require_actor_type = true` to lock out type-less spawns.
#   5) For headless runs that must never round-trip to an operator,
#      add `[run].untyped_actor_policy = "block"` so any un-typed call
#      reaching permission_prompt auto-denies via `denied_by_profile`
#      instead of routing through the bridge. Requires step 1+ —
#      validate rejects `block` when no profiles are declared.

[[worker_type]]
id    = "default"

# Tools the worker may invoke. `--allowedTools` is set from this list;
# under Path B, anything outside the list auto-denies via
# `denied_by_profile` (the model receives a non-terminating
# `{behavior: "deny", message: ...}` and adapts).
tools = ["Read", "Glob", "Grep"]

# Optional: pin the worker to specific models. `spawn_worker(model =
# ...)` rejects values outside this list. Omit to inherit
# `[defaults].model`.
# allowed_models = ["claude-haiku-4-5", "claude-sonnet-4-6"]

# Optional: cap wall-clock timeout. spawn_worker(timeout_secs = N)
# clamps DOWN to this value; never up.
# max_timeout_secs = 1800

[[sublead_type]]
id    = "planner"

# Same shape as [[worker_type]], but applied to spawn_sublead. The
# orchestration tools (mcp__pitboss__spawn_worker, etc.) are
# auto-injected — list only the model-facing tools the sub-lead
# needs to read context and propose plans.
tools = ["Read", "Glob", "Grep"]

# allowed_models = ["claude-opus-4-7"]

# Optional: cap the sub-tree's USD budget. spawn_sublead(budget_usd
# = X) clamps DOWN.
# max_budget_usd = 5.0
"#,
    )
}

#[cfg(test)]
mod tests {
    use super::render;

    /// The output must include both profile sections and the workflow
    /// header so operators can identify what they're looking at when
    /// the validate-time warning sends them here.
    #[test]
    fn render_includes_profile_sections_and_header() {
        let out = render();
        assert!(
            out.contains("[[worker_type]]"),
            "missing worker_type:\n{out}"
        );
        assert!(
            out.contains("[[sublead_type]]"),
            "missing sublead_type:\n{out}"
        );
        assert!(
            out.contains("pitboss schema --format=migration"),
            "missing self-identifying header:\n{out}"
        );
    }

    /// Stability — the function is pure and idempotent. A
    /// `--format=migration --check` mode (if added later) and any
    /// docs-snapshot test depends on this.
    #[test]
    fn render_is_pure() {
        assert_eq!(render(), render());
    }
}
