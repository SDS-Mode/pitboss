//! Run discovery — relocated to `pitboss_core::runs` (#484) so the TUI
//! and web console can reach it without pulling in this crate. This
//! module is now a thin re-export shim — every existing
//! `pitboss_cli::runs::...` import keeps working, but new code should
//! reach `pitboss_core::runs::...` directly.

pub use pitboss_core::runs::*;
