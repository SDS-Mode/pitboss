//! Library surface for pitboss-cli. Exposes the internal module tree so that
//! integration tests (in `crates/pitboss-cli/tests/`) can drive the MCP server,
//! dispatch state, and manifest resolution directly — without shelling out to
//! the `pitboss` binary. The binary (`src/main.rs`) imports the same modules
//! from this lib crate rather than re-declaring them.

pub mod agents_md;
pub mod attach;
pub mod cli;
pub mod control;
pub mod diff;
pub mod dispatch;
pub mod manifest;
pub mod mcp;
pub mod notify;
pub mod prune;
pub mod runs;
pub mod shared_store;
pub mod status;
pub mod tree;
pub mod tui_table;
