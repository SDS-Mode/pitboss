//! Minimal MCP client library used by integration tests to drive the pitboss
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
    actor_id: Option<String>,
    actor_role: Option<String>,
    /// Optional auth token injected as `_meta.token` (issue #145).
    /// Production bridges always supply one; tests can simulate either
    /// the legitimate path (with token) or a forged direct connection
    /// (no token).
    token: Option<String>,
}

impl FakeMcpClient {
    /// Connect to a pitboss MCP server on a unix socket and complete the MCP
    /// initialization handshake. Defaults to root_lead identity for backward
    /// compatibility with tests that don't specify an actor role.
    pub async fn connect(socket: &Path) -> Result<Self> {
        Self::connect_as(socket, "root", "root_lead").await
    }

    /// Connect to a pitboss MCP server with a recorded actor identity. Subsequent
    /// `call_tool` invocations will inject `_meta: {actor_id, actor_role}` into
    /// the request parameters (simulating what `mcp-bridge` does in production).
    pub async fn connect_as(socket: &Path, actor_id: &str, actor_role: &str) -> Result<Self> {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .with_context(|| format!("connect to {}", socket.display()))?;
        let inner = ()
            .serve(stream)
            .await
            .with_context(|| format!("mcp client init handshake on {}", socket.display()))?;
        Ok(Self {
            inner,
            actor_id: Some(actor_id.to_string()),
            actor_role: Some(actor_role.to_string()),
            token: None,
        })
    }

    /// Connect with a recorded actor identity AND a per-actor auth token
    /// (the token the dispatcher minted via `mint_token`). Subsequent
    /// `call_tool` invocations will inject `_meta: {actor_id, actor_role,
    /// token}` so the server can validate the token and bind the
    /// connection's canonical identity. This mirrors what the real
    /// `pitboss mcp-bridge --token` does in production.
    pub async fn connect_with_token(
        socket: &Path,
        actor_id: &str,
        actor_role: &str,
        token: &str,
    ) -> Result<Self> {
        let mut c = Self::connect_as(socket, actor_id, actor_role).await?;
        c.token = Some(token.to_string());
        Ok(c)
    }

    /// Replace the recorded auth token. Use for tests that want to
    /// simulate a connection re-handshaking with a different identity.
    pub fn set_token(&mut self, token: Option<String>) {
        self.token = token;
    }

    /// Call a tool and return the tool's structured content as JSON.
    ///
    /// Pitboss tools always populate `CallToolResult::structured_content` via
    /// `CallToolResult::structured(...)` on the server side, so we prefer that
    /// field. If it's missing (e.g. a tool returned only text content), we
    /// fall back to serializing the full `CallToolResult` as JSON so callers
    /// can still inspect it.
    ///
    /// If this client was created with `connect_as`, injects `_meta` into
    /// the arguments before sending.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value> {
        let mut arguments = match args {
            Value::Null => None,
            Value::Object(map) => Some(map),
            other => {
                return Err(anyhow::anyhow!(
                    "call_tool args must be a JSON object or null, got {}",
                    other
                ));
            }
        };

        // Inject _meta if identity is recorded AND not already present in arguments.
        // This allows tests to explicitly pass _meta (e.g., for shared_store tests
        // that pass specific actor roles) to override the default.
        // Note: also handles the Value::Null case (arguments == None) by inserting
        // an empty map, so tools that take no parameters still receive _meta.
        if let (Some(ref actor_id), Some(ref actor_role)) = (&self.actor_id, &self.actor_role) {
            let args_obj = arguments.get_or_insert_with(serde_json::Map::new);
            if !args_obj.contains_key("_meta") {
                let mut meta = serde_json::Map::new();
                meta.insert(
                    "actor_id".to_string(),
                    serde_json::Value::String(actor_id.clone()),
                );
                meta.insert(
                    "actor_role".to_string(),
                    serde_json::Value::String(actor_role.clone()),
                );
                if let Some(ref t) = self.token {
                    meta.insert("token".to_string(), serde_json::Value::String(t.clone()));
                }
                args_obj.insert("_meta".to_string(), serde_json::Value::Object(meta));
            }
        }

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

    /// List all tools exposed by the server.
    pub async fn list_tools(&mut self) -> Result<Vec<ToolInfo>> {
        let result = self.inner.list_tools(None).await.context("list_tools")?;
        Ok(result
            .tools
            .into_iter()
            .map(|t| ToolInfo {
                name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()),
            })
            .collect())
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

#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    // Integration-level round-trip testing happens in
    // crates/pitboss-cli/tests/hierarchical_flows.rs (Task 24+).
    // This module just validates the crate compiles.
}
