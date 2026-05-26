//! Per-worker / per-actor task event records — relocated to
//! `pitboss_core::task_events` (F-ARCH-5 / #484) so the web console can
//! read the audit trail without depending on this crate. This module is
//! now a thin re-export shim — every existing
//! `crate::dispatch::events::...` import keeps working, but new code
//! should reach `pitboss_core::task_events::...` directly.

pub use pitboss_core::task_events::*;
