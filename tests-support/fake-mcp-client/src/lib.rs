//! Minimal MCP client library used by integration tests to drive the shire
//! MCP server as if we were a lead claude subprocess. Connects over unix
//! socket, handles init handshake, and exposes a `call_tool` helper.

#![allow(dead_code)]

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

use rmcp::model::{CallToolRequestParam, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::ServiceExt;

/// Minimal MCP client backed by rmcp 0.8.5. The unit type `()` implements
/// `ClientHandler` with default client info, which is all tests need.
pub struct FakeMcpClient {
    inner: RunningService<RoleClient, ()>,
}

impl FakeMcpClient {
    /// Connect to a shire MCP server on a unix socket and complete the MCP
    /// initialization handshake.
    pub async fn connect(socket: &Path) -> Result<Self> {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .with_context(|| format!("connect to {}", socket.display()))?;
        let inner = ()
            .serve(stream)
            .await
            .with_context(|| format!("mcp client init handshake on {}", socket.display()))?;
        Ok(Self { inner })
    }

    /// Call a tool and return the tool's structured content as JSON.
    ///
    /// Shire tools always populate `CallToolResult::structured_content` via
    /// `CallToolResult::structured(...)` on the server side, so we prefer that
    /// field. If it's missing (e.g. a tool returned only text content), we
    /// fall back to serializing the full `CallToolResult` as JSON so callers
    /// can still inspect it.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value> {
        let arguments = match args {
            Value::Null => None,
            Value::Object(map) => Some(map),
            other => {
                return Err(anyhow::anyhow!(
                    "call_tool args must be a JSON object or null, got {}",
                    other
                ));
            }
        };
        let param = CallToolRequestParam {
            name: name.to_owned().into(),
            arguments,
        };
        let result: CallToolResult = self
            .inner
            .call_tool(param)
            .await
            .with_context(|| format!("call_tool {name}"))?;
        if let Some(structured) = result.structured_content {
            Ok(structured)
        } else {
            serde_json::to_value(&result).context("serialize CallToolResult")
        }
    }

    /// Shut down the client, closing the MCP session cleanly.
    pub async fn close(self) -> Result<()> {
        // `cancel` drives the client to `QuitReason::Cancelled` and drops the
        // transport. Any JoinError on the underlying task bubbles up here.
        self.inner
            .cancel()
            .await
            .context("cancel mcp client session")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Integration-level round-trip testing happens in
    // crates/shire-cli/tests/hierarchical_flows.rs (Task 24+).
    // This module just validates the crate compiles.
}
