//! `pitboss audit <run-id>` data types — relocated to
//! `pitboss_core::audit` (F-ARCH-5 / #484) so the web console can read
//! the audit trail without depending on this crate. This module is now
//! a thin re-export shim — every existing `crate::audit::...` import
//! keeps working, but new code should reach `pitboss_core::audit::...`
//! directly.
//!
//! The `pitboss audit` subcommand entry (`run`) still routes through
//! the shim, so `pitboss audit <run-id>` continues to work unchanged.

pub use pitboss_core::audit::*;
