//! Type-erased mirror of the per-run control socket's wire types,
//! published from `pitboss-core` so the unified consumer stream (see
//! [`crate::stream`]) can drive socket connections without depending on
//! `pitboss-cli`.
//!
//! ## Why a type-erased mirror
//!
//! The strongly-typed `ControlOp` / `ControlEvent` enums live in
//! `pitboss-cli` and carry deep dependencies on the MCP policy module,
//! approval handling, and dispatcher actor types — none of which belong
//! in `pitboss-core`. The consumer stream's job is to forward
//! envelopes verbatim to subscribers (web SSE, tests, etc.); it doesn't
//! need to interpret the inner event. So we model the wire as
//! `EventEnvelope { actor_path, seq, event: serde_json::Value }` here
//! and let typed consumers downstream-parse if they wish.
//!
//! The serde layout matches `pitboss-cli`'s typed `EventEnvelope` —
//! `actor_path` and `seq` are skipped when empty/zero, and the inner
//! event flattens its `event` discriminator plus payload fields into
//! the outer JSON object. A round-trip preserves all values; field
//! ORDER inside the inner event may be re-sorted (`serde_json`'s `Map`
//! is ordered alphabetically by default), but JSON consumers are
//! order-agnostic so the SPA, TUI, and tests all parse the re-emitted
//! envelopes correctly.
//!
//! ## Subscribe op
//!
//! [`crate::stream`]'s live transport writes a `Subscribe { since_seq }`
//! op line that decodes into `pitboss-cli`'s typed
//! `ControlOp::Subscribe` — same tag (`"op":"subscribe"`), same payload
//! shape. See [`crate::stream`] for the consumer-side handshake.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Tree-path from root to the actor that produced an event. Serialized
/// as a JSON array of actor-id strings; an empty path is omitted from
/// the wire (matching `pitboss-cli`'s typed mirror).
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActorPath(pub Vec<String>);

impl ActorPath {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Tree depth (root = 1, sub-lead = 2, worker = 3).
    #[must_use]
    pub fn depth(&self) -> usize {
        self.0.len()
    }
}

/// One frame on the control socket. The inner event is held as a
/// `serde_json::Value` so this crate doesn't pull in the typed
/// `ControlEvent` enum from `pitboss-cli`. Re-serializing this struct
/// produces bytes identical to the dispatcher's original write for any
/// well-formed envelope — important because the web SSE handler tunnels
/// envelopes through to the SPA verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    #[serde(default, skip_serializing_if = "ActorPath::is_empty")]
    pub actor_path: ActorPath,
    /// Dispatcher-assigned monotonic per-run seq (PR-B of #438). Zero
    /// is the legacy sentinel for pre-PR-B dispatchers.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub seq: u64,
    /// The flattened inner event — discriminator tag (`event`) plus
    /// payload fields, as a `Value`.
    #[serde(flatten)]
    pub event: serde_json::Value,
}

/// Serde `skip_serializing_if` predicate. The `&T` shape is forced by
/// serde's contract, so the clippy trivially-copy-by-ref lint isn't
/// actionable here.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

/// Errors surfaced when establishing the live-socket transport. Lifted
/// from `pitboss-web::control_bridge` so the unified stream can carry
/// the same error taxonomy.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("control socket not found")]
    NotFound,
    #[error("control socket exists but no listener (dispatcher exited)")]
    Dead,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("handshake: {0}")]
    Handshake(String),
}

/// Resolve the per-run control socket path. Prefers
/// `$XDG_RUNTIME_DIR/pitboss/<run_id>.control.sock` (where the dispatcher
/// publishes on systems that provide an XDG runtime dir) and falls back
/// to `<run_dir>/control.sock`.
///
/// `run_dir` is the per-run subdirectory (`<base>/<uuid>/`), matching
/// what the writer side passes — *not* the runs base. Returns `None` if
/// neither location exists; callers should treat that as
/// [`BridgeError::NotFound`].
#[must_use]
pub fn resolve_control_socket(run_id: &str, run_dir: &Path) -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg)
            .join("pitboss")
            .join(format!("{run_id}.control.sock"));
        if p.exists() {
            return Some(p);
        }
    }
    let fallback = run_dir.join("control.sock");
    if fallback.exists() {
        Some(fallback)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty actor path is elided from the wire — matches the typed
    /// `pitboss-cli` mirror's behavior so consumers that switch between
    /// the two serializers see byte-identical output.
    #[test]
    fn actor_path_empty_is_elided_on_serialize() {
        let env = EventEnvelope {
            actor_path: ActorPath::default(),
            seq: 0,
            event: serde_json::json!({ "event": "superseded" }),
        };
        let s = serde_json::to_string(&env).unwrap();
        assert!(
            !s.contains("actor_path"),
            "empty actor_path must be elided: {s}"
        );
        assert!(!s.contains("\"seq\""), "zero seq must be elided: {s}");
        assert_eq!(s, r#"{"event":"superseded"}"#);
    }

    /// Non-empty actor path and non-zero seq round-trip through serde,
    /// keeping the flattened inner event addressable.
    #[test]
    fn full_envelope_roundtrips() {
        let env = EventEnvelope {
            actor_path: ActorPath(vec!["root".into(), "lead".into(), "w-1".into()]),
            seq: 42,
            event: serde_json::json!({
                "event": "op_acked",
                "op": "cancel_worker",
                "task_id": "w-1",
            }),
        };
        let s = serde_json::to_string(&env).unwrap();
        let back: EventEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(back.seq, 42);
        assert_eq!(back.actor_path.0, vec!["root", "lead", "w-1"]);
        assert_eq!(
            back.event.get("event").and_then(|v| v.as_str()),
            Some("op_acked")
        );
    }

    /// Pre-PR-B dispatchers wrote bare `ControlEvent` without the
    /// envelope wrapper; deserializing one of those lines must still
    /// produce a valid `EventEnvelope` (with default `actor_path` +
    /// `seq`).
    #[test]
    fn pre_pr_b_event_deserializes_with_defaults() {
        let raw = r#"{"event":"superseded"}"#;
        let env: EventEnvelope = serde_json::from_str(raw).unwrap();
        assert!(env.actor_path.is_empty());
        assert_eq!(env.seq, 0);
        assert_eq!(
            env.event.get("event").and_then(|v| v.as_str()),
            Some("superseded")
        );
    }

    /// Round-tripping a dispatcher-produced envelope through this
    /// type-erased mirror preserves all values. Field ORDER inside the
    /// inner event may change (`serde_json` sorts alphabetically), but
    /// SPA/TUI/test consumers parse JSON order-agnostically — so the
    /// SSE tunnel that drives the SPA stays correct.
    #[test]
    fn round_trip_preserves_values_for_typed_payloads() {
        let dispatcher_line = r#"{"actor_path":["lead"],"seq":7,"event":"approval_request","request_id":"req-1","task_id":"lead","summary":"go"}"#;
        let env: EventEnvelope = serde_json::from_str(dispatcher_line).unwrap();
        let reser = serde_json::to_string(&env).unwrap();
        let original: serde_json::Value = serde_json::from_str(dispatcher_line).unwrap();
        let re_parsed: serde_json::Value = serde_json::from_str(&reser).unwrap();
        assert_eq!(original, re_parsed, "values must round-trip");
        // Top-level field positions (actor_path / seq / event) are
        // pinned by the struct layout, so those still match exactly.
        assert!(reser.starts_with(r#"{"actor_path":["lead"],"seq":7,"event":"approval_request""#));
    }

    /// Resolver returns `None` when neither the XDG socket nor the
    /// in-run-dir fallback exists; callers map this to `NotFound`.
    #[test]
    fn resolve_control_socket_returns_none_when_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Override XDG so the XDG branch doesn't accidentally hit a
        // real socket on the host.
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path().join("xdg-empty"));
        let got = resolve_control_socket("abcd", tmp.path());
        match prev {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        assert!(got.is_none(), "expected None, got {got:?}");
    }

    /// When the in-run-dir fallback exists, the resolver returns it.
    /// (The XDG branch is harder to test hermetically — it's covered by
    /// the integration test in pitboss-cli that exercises a real
    /// dispatcher.)
    #[test]
    fn resolve_control_socket_finds_run_dir_fallback() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sock = tmp.path().join("control.sock");
        std::fs::write(&sock, b"").unwrap();
        let prev = std::env::var_os("XDG_RUNTIME_DIR");
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path().join("xdg-empty"));
        let got = resolve_control_socket("run-1", tmp.path());
        match prev {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        assert_eq!(got, Some(sock));
    }
}
