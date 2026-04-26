//! Run discovery — re-exported from [`pitboss_cli::runs`] so the TUI
//! and the `pitboss prune` subcommand share one classifier.
//!
//! Kept as a thin shim (rather than rewriting every `crate::runs::…`
//! import in the TUI) so the relocation lands as a pure refactor on
//! the consumer side. New code anywhere in the workspace should
//! import from `pitboss_cli::runs` directly.

pub use pitboss_cli::runs::*;
