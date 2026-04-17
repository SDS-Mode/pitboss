//! MCP server exposed to the lead Hobbit so it can spawn and coordinate
//! worker Hobbits via structured tool calls. Bound to a single hierarchical
//! run; started before the lead, shut down after the lead + workers drain.

pub mod bridge;
pub mod server;
pub mod tools;

#[allow(unused_imports)]
pub use bridge::run_bridge;
#[allow(unused_imports)]
pub use server::{socket_path_for_run, McpServer};
