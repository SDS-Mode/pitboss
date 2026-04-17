//! mosaic-core — shared runtime for Agent Shire and future Mosaic TUI.

#![deny(unsafe_code)]
#![warn(clippy::all, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

pub mod error;
pub mod parser;
pub mod process;
pub mod session;
pub mod worktree;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod smoke {
    #[test]
    fn version_is_set() {
        assert!(!super::VERSION.is_empty());
    }
}
