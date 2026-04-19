//! Actor identity and tree-path types shared across dispatch, MCP, and
//! control-plane layers. `ActorRole` distinguishes root lead, sub-leads,
//! and workers for authz; `ActorPath` carries lineage for event routing
//! and TUI display.

use serde::{Deserialize, Serialize};
use std::fmt;

pub type ActorId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorRole {
    RootLead,
    Sublead,
    Worker,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct ActorPath(pub Vec<ActorId>);

impl fmt::Display for ActorPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.join("→"))
    }
}

impl ActorPath {
    /// Construct from a slice of actor ids.
    pub fn new<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<ActorId>,
    {
        ActorPath(ids.into_iter().map(Into::into).collect())
    }

    /// Append an actor id to produce a deeper path.
    pub fn child(&self, id: impl Into<ActorId>) -> Self {
        let mut next = self.0.clone();
        next.push(id.into());
        ActorPath(next)
    }

    /// Last segment (the actor id this path identifies).
    pub fn leaf(&self) -> Option<&ActorId> {
        self.0.last()
    }

    /// Depth in the tree (root = 1, sub-lead = 2, worker = 3).
    pub fn depth(&self) -> usize {
        self.0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_path_renders_with_arrow_separator() {
        let path = ActorPath(vec!["root".into(), "S1".into(), "W3".into()]);
        assert_eq!(path.to_string(), "root→S1→W3");
    }

    #[test]
    fn actor_path_renders_root_only() {
        let path = ActorPath(vec!["root".into()]);
        assert_eq!(path.to_string(), "root");
    }

    #[test]
    fn actor_path_renders_empty_as_empty_string() {
        let path = ActorPath::default();
        assert_eq!(path.to_string(), "");
    }

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
