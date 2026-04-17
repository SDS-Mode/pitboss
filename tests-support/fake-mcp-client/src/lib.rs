//! Minimal MCP client library used by integration tests to drive the shire
//! MCP server as if we were a lead claude subprocess. Connects over unix
//! socket, handles init handshake, and exposes a `call_tool` helper.

#![allow(dead_code)]

use anyhow::Result;
use serde_json::Value;
use std::path::Path;

pub struct FakeMcpClient {
    // Real rmcp client populated after `connect`.
    // Implementation flushed out in Task 8 step 3.
}

impl FakeMcpClient {
    /// Connect to a shire MCP server on a unix socket and complete the MCP
    /// initialization handshake.
    pub async fn connect(socket: &Path) -> Result<Self> {
        let _ = socket;
        unimplemented!("fake MCP client wire-up — see docs/superpowers/specs/2026-04-17-hierarchical-orchestration-design.md §10.2")
    }

    /// Call a tool and return its raw result payload as JSON.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value> {
        let _ = (name, args);
        unimplemented!()
    }

    /// Shut down the client.
    pub async fn close(self) -> Result<()> {
        Ok(())
    }
}
