//! Actor identity and tree-path types shared across dispatch, MCP, and
//! control-plane layers. `ActorRole` distinguishes root lead, sub-leads,
//! and workers for authz; `ActorPath` carries lineage for event routing
//! and TUI display.

use serde::{Deserialize, Serialize};

/// Unique actor identifier.
/// String is fine for depth ≤ 3; revisit if ActorPath clones show up in profiles.
pub type ActorId = String;

/// Tree-path from root to the actor that produced an event (#481).
/// Single source of truth lives in `pitboss-core` so this crate, the
/// unified consumer stream, and the read-side consumers share one
/// definition. Re-exported here so existing
/// `pitboss_cli::dispatch::actor::ActorPath` imports keep working.
pub use pitboss_core::control_protocol::ActorPath;

/// Actor role for authz in depth-2 dispatch model.
///
/// TODO(depth-2): This enum consolidates into `shared_store::ActorRole` once
/// the `_meta.actor_role` plumbing extends to recognize `sublead` (Task 1.4).
/// For now, both coexist:
/// - `shared_store::ActorRole { Lead, Worker }` (legacy, simple)
/// - `dispatch::ActorRole { RootLead, Sublead, Worker }` (new, rich)
///
/// Temporary mapping for bridging code that must convert depth-2 → legacy:
/// - `RootLead → Lead`
/// - `Sublead → Lead`
/// - `Worker → Worker`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorRole {
    RootLead,
    Sublead,
    Worker,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_role_round_trips_through_serde() {
        for role in [ActorRole::RootLead, ActorRole::Sublead, ActorRole::Worker] {
            let s = serde_json::to_string(&role).unwrap();
            let back: ActorRole = serde_json::from_str(&s).unwrap();
            assert_eq!(role, back);
        }
    }

    #[test]
    fn actor_role_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ActorRole::RootLead).unwrap(),
            r#""root_lead""#
        );
        assert_eq!(
            serde_json::to_string(&ActorRole::Sublead).unwrap(),
            r#""sublead""#
        );
        assert_eq!(
            serde_json::to_string(&ActorRole::Worker).unwrap(),
            r#""worker""#
        );
    }
}
