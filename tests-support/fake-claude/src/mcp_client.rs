//! Minimal MCP client for fake-claude's MCP-client mode.
//!
//! Connects to a pitboss MCP server over a unix socket, performs the
//! init handshake, and exposes `call_tool`. Intentionally near-duplicate
//! of `fake-mcp-client/src/lib.rs` — the two crates have different roles
//! (one drives the server as a test peer; the other emulates a lead
//! subprocess), and inlining avoids a test-support dependency cycle.

#![allow(dead_code)]

use std::path::Path;

use anyhow::{Context, Result};
use rmcp::model::{CallToolRequestParam, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::ServiceExt;
use serde_json::Value;

pub struct McpClient {
    inner: RunningService<RoleClient, ()>,
}

impl McpClient {
    /// Connect to a pitboss MCP server on `socket` and complete the MCP
    /// initialization handshake.
    pub async fn connect(socket: &Path) -> Result<Self> {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .with_context(|| format!("connect to {}", socket.display()))?;
        let inner = ()
            .serve(stream)
            .await
            .with_context(|| format!("mcp init handshake on {}", socket.display()))?;
        Ok(Self { inner })
    }

    /// Call a tool and return its structured content as JSON. Pitboss
    /// tools always populate `CallToolResult::structured_content` via the
    /// `to_structured_result` helper server-side, so we prefer that.
    /// Falls back to serializing the whole `CallToolResult`.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value> {
        let arguments = match args {
            Value::Null => None,
            Value::Object(map) => Some(map),
            other => {
                anyhow::bail!("call_tool args must be a JSON object or null, got {other}");
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
}
