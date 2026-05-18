//! Lib-internal entry point for the shared test scaffolding.
//!
//! The canonical builder lives at `tests/support/state_builder.rs` so
//! integration-test files can pull it in via their existing
//! `mod support;` import. We `include!()` the same file here so the
//! lib's own `#[cfg(test)]` unit tests share one source of truth.
//!
//! The included file references its own crate as `pitboss_cli::...`,
//! which is the natural path for integration tests (where pitboss-cli
//! is a dev-dependency). The local `use crate as pitboss_cli;` below
//! makes the same path resolve to the lib crate root when the file is
//! compiled as part of the lib's own test build — no self-dependency
//! declaration required.

#[cfg(test)]
pub mod state_builder {
    use crate as pitboss_cli;
    include!("../tests/support/state_builder.rs");
}
