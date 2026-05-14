//! Renderer for `pitboss schema --format=agent-profiles`.
//!
//! Lists the bundled built-in [`AgentProfile`] catalogue, optionally
//! merged with the profiles declared by a specific manifest, in a
//! human-scannable form. The output is a markdown document with a
//! summary table + one section per profile.
//!
//! Manifest-declared profiles are tagged `source = manifest`; built-ins
//! are tagged `source = builtin`. A manifest entry that shadows a
//! built-in id (intentional override) is rendered as `manifest`
//! (operators see the version that actually wins).

use std::collections::HashSet;

use crate::manifest::schema::AgentProfile;

/// Render the agent-profiles doc.
///
/// `manifest_profiles` is empty by default (just the built-ins are
/// listed); passing a manifest's `agent_profiles` adds them and marks
/// each with its source.
pub fn render(manifest_profiles: &[AgentProfile]) -> String {
    let builtins = crate::manifest::builtin_profiles::load_builtins();
    let builtin_ids: HashSet<&str> = builtins.iter().map(|p| p.id.as_str()).collect();

    // Manifest entries shadow built-ins; build the effective catalogue
    // by collecting from built-ins, then overwriting with manifest
    // entries.
    let mut effective: Vec<(AgentProfile, &'static str)> =
        builtins.iter().map(|p| (p.clone(), "builtin")).collect();
    for mp in manifest_profiles {
        if let Some(slot) = effective.iter_mut().find(|(p, _)| p.id == mp.id) {
            *slot = (mp.clone(), "manifest");
        } else {
            effective.push((mp.clone(), "manifest"));
        }
    }
    // Stable ordering for deterministic output: built-ins first (in
    // their declared order), then manifest-only ids alphabetically.
    let _ = builtin_ids;

    let mut out = String::new();
    out.push_str("# pitboss schema --format=agent-profiles\n\n");
    out.push_str(
        "Reusable role profiles available at resolve / spawn time. Each profile contributes\n\
         a `system_prompt` prelude prepended to the operator's prompt with a `--- TASK ---`\n\
         separator, plus optional `model`/`tools`/`env` defaults that the operator's per-actor\n\
         config can override.\n\n",
    );

    out.push_str("## Summary\n\n");
    out.push_str("| id | source | model | tools | env keys |\n");
    out.push_str("|---|---|---|---|---|\n");
    for (p, src) in &effective {
        let model = p.model.as_deref().unwrap_or("—");
        let tools = match &p.tools {
            Some(t) if !t.is_empty() => t.join(", "),
            _ => "—".to_string(),
        };
        let env_keys: Vec<&str> = p.env.keys().map(String::as_str).collect();
        let env_repr = if env_keys.is_empty() {
            "—".to_string()
        } else {
            let mut ks = env_keys.clone();
            ks.sort();
            ks.join(", ")
        };
        out.push_str(&format!(
            "| `{}` | {} | `{}` | {} | {} |\n",
            p.id, src, model, tools, env_repr
        ));
    }
    out.push('\n');

    out.push_str("## Profile bodies\n\n");
    for (p, src) in &effective {
        out.push_str(&format!("### `{}` ({})\n\n", p.id, src));
        out.push_str(&format!(
            "- **model**: {}\n",
            p.model.as_deref().unwrap_or("—")
        ));
        out.push_str(&format!(
            "- **tools**: {}\n",
            match &p.tools {
                Some(t) if !t.is_empty() => t.join(", "),
                _ => "—".to_string(),
            }
        ));
        if !p.env.is_empty() {
            let mut keys: Vec<&String> = p.env.keys().collect();
            keys.sort();
            out.push_str("- **env**:\n");
            for k in keys {
                out.push_str(&format!("  - `{}` = `{}`\n", k, p.env[k]));
            }
        }
        out.push_str("\n```\n");
        out.push_str(p.system_prompt.trim_end());
        out.push_str("\n```\n\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_builtins_only_when_manifest_empty() {
        let out = render(&[]);
        assert!(out.contains("pitboss/lead-opus"), "{out}");
        assert!(out.contains("pitboss/sublead-sonnet"), "{out}");
        assert!(out.contains("pitboss/worker-haiku"), "{out}");
    }

    #[test]
    fn marks_manifest_profile_as_manifest_source() {
        let manifest = vec![AgentProfile {
            id: "review/domain-worker".into(),
            system_prompt: "be domain-shaped".into(),
            model: Some("claude-haiku-4-5".into()),
            env: Default::default(),
            tools: Some(vec!["Read".into()]),
        }];
        let out = render(&manifest);
        assert!(out.contains("review/domain-worker"), "{out}");
        // Source column should call it out.
        assert!(out.contains("| manifest |"), "{out}");
    }

    #[test]
    fn manifest_entry_shadows_builtin_id() {
        let manifest = vec![AgentProfile {
            id: "pitboss/worker-haiku".into(),
            system_prompt: "OVERRIDDEN".into(),
            model: None,
            env: Default::default(),
            tools: None,
        }];
        let out = render(&manifest);
        // The overridden body should appear; the source column should
        // be "manifest" for the shadowed id.
        assert!(out.contains("OVERRIDDEN"), "{out}");
        // Counted once, not twice.
        let occurrences = out.matches("pitboss/worker-haiku").count();
        assert!(
            occurrences == 2, // once in summary table, once in body header
            "expected 2 occurrences, got {occurrences}:\n{out}"
        );
    }

    #[test]
    fn header_self_identifies_for_drift_checks() {
        let out = render(&[]);
        assert!(out.contains("pitboss schema --format=agent-profiles"));
    }
}
