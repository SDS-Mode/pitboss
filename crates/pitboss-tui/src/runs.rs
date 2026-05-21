//! Run discovery — re-exported from [`pitboss_core::runs`] so the TUI
//! and the `pitboss prune` subcommand share one classifier.
//!
//! Kept as a thin shim (rather than rewriting every `crate::runs::…`
//! import in the TUI) so the relocation lands as a pure refactor on
//! the consumer side. New code anywhere in the workspace should
//! import from `pitboss_core::runs` directly.
//!
//! Lifted out of `pitboss_cli` in #484 so this crate's dep on
//! `pitboss-cli` is no longer required just for run discovery.

pub use pitboss_core::runs::*;
