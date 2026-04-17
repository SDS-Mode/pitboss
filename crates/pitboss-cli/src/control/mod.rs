//! Per-run control socket: TUI ↔ dispatcher operator control plane.
//!
//! Split by file:
//! - `protocol` — serde types for line-based JSON messages.
//! - `server`   — unix-socket accept loop + per-connection op dispatch.
//!
//! See `docs/superpowers/specs/2026-04-17-pitboss-v0.4-live-control-design.md`
//! §4–§6 for the design.

pub mod protocol;
pub mod server;
