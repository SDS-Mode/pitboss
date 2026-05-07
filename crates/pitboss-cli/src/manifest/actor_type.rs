//! Typed actor profile resolution and enforcement (#252).
//!
//! Operators declare `[[worker_type]]` / `[[sublead_type]]` in the
//! manifest to cap the capability surface a `spawn_worker` /
//! `spawn_sublead` call may request. The lead can never widen those
//! caps — `tools` arg must be a subset of the profile's allowlist;
//! `model` must be in `allowed_models` (when set); `timeout_secs`
//! / `budget_usd` only clamp DOWN.
//!
//! This module is the single source of truth for that enforcement.
//! Both `handle_spawn_worker` and the `spawn_sublead` MCP handler
//! call into [`resolve_worker_profile`] / [`resolve_sublead_profile`]
//! before applying any defaults.
//!
//! Behavior matrix:
//!
//! | `require_actor_type` | requested type | manifest declares any profiles? | outcome                                     |
//! |----------------------|----------------|---------------------------------|---------------------------------------------|
//! | true                 | None           | —                               | reject ("must name a worker_type")          |
//! | true                 | Some(unknown)  | —                               | reject ("unknown worker_type")              |
//! | true                 | Some(known)    | —                               | enforce caps                                |
//! | false                | None           | yes / no                        | back-compat: skip enforcement, legacy cascade |
//! | false                | Some(unknown)  | —                               | reject ("unknown worker_type")              |
//! | false                | Some(known)    | —                               | enforce caps                                |

use anyhow::{bail, Result};

use crate::manifest::schema::{SubleadType, WorkerType};

/// The outcome of resolving an actor type against the manifest's
/// declared profiles.
///
/// `Untyped` is the back-compat path: no `worker_type` arg supplied
/// AND `require_actor_type = false`. The caller should fall through
/// to the legacy `[lead].tools` cascade.
///
/// `Typed` carries a borrow of the resolved profile — the caller uses
/// it both to derive defaults (when args are omitted) and to clamp
/// numeric caps (timeouts, budgets) before spawning.
#[derive(Debug)]
pub enum WorkerProfileResolution<'m> {
    Untyped,
    Typed(&'m WorkerType),
}

#[derive(Debug)]
pub enum SubleadProfileResolution<'m> {
    Untyped,
    Typed(&'m SubleadType),
}

/// Resolve a `spawn_worker(worker_type = ...)` arg against the
/// manifest's `[[worker_type]]` profiles. Returns the resolution
/// (`Typed` for further enforcement, `Untyped` for back-compat) or
/// an error message suitable for surfacing to the lead's claude
/// session as the spawn rejection reason.
pub fn resolve_worker_profile<'m>(
    requested: Option<&str>,
    profiles: &'m [WorkerType],
    require_actor_type: bool,
) -> Result<WorkerProfileResolution<'m>> {
    match requested {
        Some(id) => match profiles.iter().find(|wt| wt.id == id) {
            Some(wt) => Ok(WorkerProfileResolution::Typed(wt)),
            None => {
                if profiles.is_empty() {
                    bail!(
                        "spawn_worker rejected: worker_type {id:?} requested \
                         but the manifest declares no `[[worker_type]]` \
                         profiles"
                    );
                }
                let known: Vec<&str> = profiles.iter().map(|wt| wt.id.as_str()).collect();
                bail!(
                    "spawn_worker rejected: unknown worker_type {id:?} (known: {})",
                    known.join(", ")
                );
            }
        },
        None => {
            if require_actor_type {
                let known: Vec<&str> = profiles.iter().map(|wt| wt.id.as_str()).collect();
                if known.is_empty() {
                    bail!(
                        "spawn_worker rejected: [run].require_actor_type = true \
                         but no `[[worker_type]]` declared"
                    );
                }
                bail!(
                    "spawn_worker rejected: [run].require_actor_type = true \
                     requires every spawn_worker call to name a worker_type \
                     (known: {})",
                    known.join(", ")
                );
            }
            Ok(WorkerProfileResolution::Untyped)
        }
    }
}

/// Sub-lead twin of [`resolve_worker_profile`].
pub fn resolve_sublead_profile<'m>(
    requested: Option<&str>,
    profiles: &'m [SubleadType],
    require_actor_type: bool,
) -> Result<SubleadProfileResolution<'m>> {
    match requested {
        Some(id) => match profiles.iter().find(|st| st.id == id) {
            Some(st) => Ok(SubleadProfileResolution::Typed(st)),
            None => {
                if profiles.is_empty() {
                    bail!(
                        "spawn_sublead rejected: sublead_type {id:?} requested \
                         but the manifest declares no `[[sublead_type]]` \
                         profiles"
                    );
                }
                let known: Vec<&str> = profiles.iter().map(|st| st.id.as_str()).collect();
                bail!(
                    "spawn_sublead rejected: unknown sublead_type {id:?} (known: {})",
                    known.join(", ")
                );
            }
        },
        None => {
            if require_actor_type {
                let known: Vec<&str> = profiles.iter().map(|st| st.id.as_str()).collect();
                if known.is_empty() {
                    bail!(
                        "spawn_sublead rejected: [run].require_actor_type = true \
                         but no `[[sublead_type]]` declared"
                    );
                }
                bail!(
                    "spawn_sublead rejected: [run].require_actor_type = true \
                     requires every spawn_sublead call to name a sublead_type \
                     (known: {})",
                    known.join(", ")
                );
            }
            Ok(SubleadProfileResolution::Untyped)
        }
    }
}

/// Validate that `requested_tools` (the lead's `tools` arg) is a
/// subset of `profile_tools`. Returns Ok(()) on subset; on violation
/// returns an error naming the first offending tool — the model
/// can fix it and retry without parsing a list.
pub fn check_tool_subset(
    spawn_kind: &str,
    profile_id: &str,
    profile_tools: &[String],
    requested_tools: &[String],
) -> Result<()> {
    for t in requested_tools {
        if !profile_tools.iter().any(|allowed| allowed == t) {
            bail!(
                "{spawn_kind} rejected: tool {t:?} is not in {spawn_kind_short}_type \
                 {profile_id:?} allowlist (allowed: {allowed})",
                spawn_kind_short = spawn_kind.trim_start_matches("spawn_"),
                allowed = profile_tools.join(", "),
            );
        }
    }
    Ok(())
}

/// Validate that `model` is in `allowed_models` (when non-empty).
/// An empty `allowed_models` means no restriction.
pub fn check_model_allowed(
    spawn_kind: &str,
    profile_id: &str,
    allowed_models: &[String],
    model: &str,
) -> Result<()> {
    if allowed_models.is_empty() {
        return Ok(());
    }
    if !allowed_models.iter().any(|m| m == model) {
        bail!(
            "{spawn_kind} rejected: model {model:?} is not in {spawn_kind_short}_type \
             {profile_id:?} allowed_models (allowed: {allowed})",
            spawn_kind_short = spawn_kind.trim_start_matches("spawn_"),
            allowed = allowed_models.join(", "),
        );
    }
    Ok(())
}

/// Clamp `requested` down to `cap` when both are present.
/// Returns the smaller of the two; preserves `requested` when it's
/// already within budget.
pub fn clamp_down_u64(requested: u64, cap: Option<u64>) -> u64 {
    match cap {
        Some(c) => requested.min(c),
        None => requested,
    }
}

/// Clamp `requested` (USD) down to `cap` when both are present.
pub fn clamp_down_f64(requested: Option<f64>, cap: Option<f64>) -> Option<f64> {
    match (requested, cap) {
        (Some(r), Some(c)) => Some(r.min(c)),
        (Some(r), None) => Some(r),
        (None, Some(c)) => Some(c),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wt(id: &str) -> WorkerType {
        WorkerType {
            id: id.to_string(),
            tools: vec!["Read".into(), "Glob".into(), "Grep".into()],
            allowed_models: vec!["claude-haiku-4-5".into()],
            max_timeout_secs: Some(900),
        }
    }

    fn st(id: &str) -> SubleadType {
        SubleadType {
            id: id.to_string(),
            tools: vec!["Read".into(), "Write".into()],
            allowed_models: vec!["claude-opus-4-7".into()],
            max_timeout_secs: Some(1800),
            max_budget_usd: Some(2.0),
        }
    }

    #[test]
    fn worker_resolution_typed_match() {
        let profiles = vec![wt("extraction")];
        match resolve_worker_profile(Some("extraction"), &profiles, false).unwrap() {
            WorkerProfileResolution::Typed(p) => assert_eq!(p.id, "extraction"),
            _ => panic!("expected Typed"),
        }
    }

    #[test]
    fn worker_resolution_unknown_id_listed() {
        let profiles = vec![wt("extraction"), wt("writer")];
        let err = resolve_worker_profile(Some("nope"), &profiles, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown worker_type"), "{msg}");
        assert!(msg.contains("extraction"), "{msg}");
        assert!(msg.contains("writer"), "{msg}");
    }

    #[test]
    fn worker_resolution_untyped_passes_when_not_required() {
        let profiles = vec![wt("extraction")];
        assert!(matches!(
            resolve_worker_profile(None, &profiles, false).unwrap(),
            WorkerProfileResolution::Untyped
        ));
    }

    #[test]
    fn worker_resolution_untyped_rejected_when_required() {
        let profiles = vec![wt("extraction")];
        let err = resolve_worker_profile(None, &profiles, true).unwrap_err();
        assert!(err.to_string().contains("require_actor_type"));
    }

    #[test]
    fn worker_resolution_unknown_id_no_profiles() {
        let err = resolve_worker_profile(Some("x"), &[], false).unwrap_err();
        assert!(err
            .to_string()
            .contains("manifest declares no `[[worker_type]]` profiles"));
    }

    #[test]
    fn tool_subset_accepts_subset() {
        check_tool_subset(
            "spawn_worker",
            "x",
            &["Read".into(), "Glob".into()],
            &["Read".into()],
        )
        .unwrap();
    }

    #[test]
    fn tool_subset_rejects_unknown() {
        let err = check_tool_subset(
            "spawn_worker",
            "extraction",
            &["Read".into()],
            &["Read".into(), "Bash".into()],
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("\"Bash\""), "{msg}");
        assert!(msg.contains("worker_type \"extraction\""), "{msg}");
    }

    #[test]
    fn model_allowed_empty_list_means_unrestricted() {
        check_model_allowed("spawn_worker", "x", &[], "anything").unwrap();
    }

    #[test]
    fn model_allowed_rejects_outside_list() {
        let err = check_model_allowed(
            "spawn_worker",
            "extraction",
            &["claude-haiku-4-5".into()],
            "claude-opus-4-7",
        )
        .unwrap_err();
        assert!(err.to_string().contains("claude-opus-4-7"));
    }

    #[test]
    fn clamp_down_u64_caps() {
        assert_eq!(clamp_down_u64(500, Some(900)), 500);
        assert_eq!(clamp_down_u64(2000, Some(900)), 900);
        assert_eq!(clamp_down_u64(2000, None), 2000);
    }

    #[test]
    fn clamp_down_f64_caps() {
        assert_eq!(clamp_down_f64(Some(1.5), Some(2.0)), Some(1.5));
        assert_eq!(clamp_down_f64(Some(5.0), Some(2.0)), Some(2.0));
        assert_eq!(clamp_down_f64(None, Some(2.0)), Some(2.0));
        assert_eq!(clamp_down_f64(Some(1.0), None), Some(1.0));
        assert_eq!(clamp_down_f64(None, None), None);
    }

    #[test]
    fn sublead_resolution_typed_match() {
        let profiles = vec![st("planner")];
        assert!(matches!(
            resolve_sublead_profile(Some("planner"), &profiles, false).unwrap(),
            SubleadProfileResolution::Typed(_)
        ));
    }

    #[test]
    fn sublead_resolution_required_without_arg_rejected() {
        let profiles = vec![st("planner")];
        let err = resolve_sublead_profile(None, &profiles, true).unwrap_err();
        assert!(err.to_string().contains("require_actor_type"));
    }
}
