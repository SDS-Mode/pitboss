//! Library surface for pitboss-tui, so integration tests in `tests/` can
//! pull state types and render functions without shelling out to the bin.

#![deny(unsafe_code)]
#![warn(clippy::all, clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::must_use_candidate
)]

pub mod app;
pub mod control;
pub mod runs;
pub mod state;
pub mod theme;
pub mod tui;
pub mod watcher;
