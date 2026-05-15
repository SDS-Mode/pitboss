//! Built-in [`AgentProfile`] catalogue, compile-time embedded.
//!
//! Three profiles ship bundled so the common lead/sublead/worker roles
//! work zero-config:
//!
//! - `pitboss/lead-opus` — root orchestrator on Opus.
//! - `pitboss/sublead-sonnet` — sub-tree orchestrator on Sonnet.
//! - `pitboss/worker-haiku` — leaf investigator on Haiku.
//!
//! Each bundled file is markdown with a TOML frontmatter block fenced
//! by `+++`. The frontmatter parses into [`AgentProfile`] fields
//! (`id`, `model`, `tools`, `env`); the body becomes `system_prompt`.
//!
//! ```text
//! +++
//! id = "pitboss/worker-haiku"
//! model = "claude-haiku-4-5"
//! tools = ["Read", "Glob", "Grep"]
//! [env]
//! PITBOSS_ACTOR_ROLE = "worker"
//! +++
//! You are a Pitboss worker...
//! ```
//!
//! The catalogue is constructed by [`load_builtins`] at resolve time;
//! manifest-declared `[[agent_profile]]` entries union with this set
//! and shadow built-ins by matching id exactly.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use crate::manifest::schema::AgentProfile;

/// The three bundled profile bodies, embedded at compile time.
const BUILTIN_SOURCES: &[(&str, &str)] = &[
    (
        "lead-opus.md",
        include_str!("builtin_profiles/lead-opus.md"),
    ),
    (
        "sublead-sonnet.md",
        include_str!("builtin_profiles/sublead-sonnet.md"),
    ),
    (
        "worker-haiku.md",
        include_str!("builtin_profiles/worker-haiku.md"),
    ),
];

/// TOML frontmatter shape. Mirrors the [`AgentProfile`] fields minus
/// `system_prompt` (which is the file body, not the frontmatter).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    env: HashMap<String, String>,
}

/// Parse a single bundled `+++`-fenced markdown source into an
/// [`AgentProfile`]. Returns an error if the source lacks the fence
/// pair or the frontmatter doesn't deserialize.
fn parse_profile(filename: &str, source: &str) -> Result<AgentProfile> {
    let rest = source
        .strip_prefix("+++")
        .ok_or_else(|| anyhow!("{filename}: missing opening `+++` fence"))?;
    let (frontmatter_raw, body_raw) = rest
        .split_once("+++")
        .ok_or_else(|| anyhow!("{filename}: missing closing `+++` fence"))?;

    let fm: Frontmatter = toml::from_str(frontmatter_raw.trim())
        .with_context(|| format!("{filename}: parsing TOML frontmatter"))?;

    if !fm.id.starts_with("pitboss/") {
        bail!(
            "{filename}: built-in profile id {:?} must be in the `pitboss/` namespace",
            fm.id
        );
    }

    let system_prompt = body_raw.trim_start_matches('\n').to_string();

    Ok(AgentProfile {
        id: fm.id,
        system_prompt,
        model: fm.model,
        env: fm.env,
        tools: fm.tools,
    })
}

/// Return the three bundled profiles. Pure — same output every call.
/// Panics on a malformed bundled file because that is a build-time
/// bug, not an operator condition.
pub fn load_builtins() -> Vec<AgentProfile> {
    BUILTIN_SOURCES
        .iter()
        .map(|(name, src)| {
            parse_profile(name, src)
                .unwrap_or_else(|e| panic!("built-in profile {name} failed to parse: {e:?}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_parses() {
        // Smoke: load_builtins must not panic; every bundled file
        // must produce an AgentProfile.
        let profiles = load_builtins();
        assert_eq!(profiles.len(), BUILTIN_SOURCES.len());
    }

    #[test]
    fn every_builtin_id_is_pitboss_namespaced() {
        for p in load_builtins() {
            assert!(
                p.id.starts_with("pitboss/"),
                "built-in id {:?} not in pitboss/ namespace",
                p.id
            );
        }
    }

    #[test]
    fn built_ins_cover_three_roles() {
        let ids: Vec<String> = load_builtins().into_iter().map(|p| p.id).collect();
        assert!(ids.contains(&"pitboss/lead-opus".to_string()));
        assert!(ids.contains(&"pitboss/sublead-sonnet".to_string()));
        assert!(ids.contains(&"pitboss/worker-haiku".to_string()));
    }

    #[test]
    fn worker_haiku_body_mentions_publish_ceremony() {
        let worker = load_builtins()
            .into_iter()
            .find(|p| p.id == "pitboss/worker-haiku")
            .expect("worker-haiku built-in missing");
        assert!(
            worker.system_prompt.contains("artifact_put"),
            "worker-haiku body should describe artifact_put publish step:\n{}",
            worker.system_prompt
        );
        assert!(
            worker.system_prompt.contains("message_send"),
            "worker-haiku body should describe message_send publish step:\n{}",
            worker.system_prompt
        );
    }

    #[test]
    fn lead_body_mentions_spawn_and_wait() {
        let lead = load_builtins()
            .into_iter()
            .find(|p| p.id == "pitboss/lead-opus")
            .expect("lead-opus built-in missing");
        assert!(
            lead.system_prompt.contains("spawn_worker"),
            "lead body should describe spawn_worker"
        );
        assert!(
            lead.system_prompt.contains("wait_for_worker")
                || lead.system_prompt.contains("wait_actor"),
            "lead body should describe waiting on workers"
        );
    }

    #[test]
    fn frontmatter_carries_role_env() {
        for p in load_builtins() {
            assert!(
                p.env.contains_key("PITBOSS_ACTOR_ROLE"),
                "{:?} missing PITBOSS_ACTOR_ROLE",
                p.id
            );
        }
    }

    #[test]
    fn parser_rejects_missing_opening_fence() {
        let err = parse_profile("x.md", "no fence\nbody").unwrap_err();
        assert!(err.to_string().contains("opening `+++`"));
    }

    #[test]
    fn parser_rejects_missing_closing_fence() {
        let err = parse_profile("x.md", "+++\nid = \"pitboss/x\"\n").unwrap_err();
        assert!(err.to_string().contains("closing `+++`"));
    }

    #[test]
    fn parser_rejects_non_pitboss_builtin_id() {
        let src = "+++\nid = \"x\"\n+++\nbody";
        let err = parse_profile("x.md", src).unwrap_err();
        assert!(err.to_string().contains("pitboss/"));
    }
}
