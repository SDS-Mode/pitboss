//! Minimal MCP client for fake-claude's MCP-client mode.
//!
//! Connects to a pitboss MCP server over a unix socket, performs the
//! init handshake, and exposes `call_tool`. Intentionally near-duplicate
//! of `fake-mcp-client/src/lib.rs` — the two crates have different roles
//! (one drives the server as a test peer; the other emulates a lead
//! subprocess), and inlining avoids a test-support dependency cycle.
//!
//! Two connection modes:
//! - [`McpClient::connect`] — opens the pitboss unix socket directly.
//!   Fast for test setups that don't care about the bridge layer.
//! - [`McpClient::connect_via_bridge`] — spawns
//!   `<pitboss> mcp-bridge <socket> --actor-id <id> --actor-role <role>`
//!   and speaks stdio JSON-RPC to it. Exercises the `_meta` injection
//!   path that a real claude subprocess uses in production.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rmcp::model::{CallToolRequestParam, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use serde_json::Value;
use tokio::process::Command;

pub struct McpClient {
    inner: RunningService<RoleClient, ()>,
}

impl McpClient {
    /// Connect to a pitboss MCP server on `socket` and complete the MCP
    /// initialization handshake. Direct unix-socket mode — skips the
    /// bridge's `_meta` injection.
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

    /// Connect via a spawned `pitboss mcp-bridge` subprocess. Mirrors
    /// how a real claude subprocess talks to pitboss: the bridge reads
    /// JSON-RPC from stdin, injects `_meta: {actor_id, actor_role}` on
    /// every `tools/call`, and forwards to the unix socket.
    ///
    /// `pitboss_bin` must point at a `pitboss` binary that exposes the
    /// `mcp-bridge` subcommand.
    pub async fn connect_via_bridge(
        pitboss_bin: &Path,
        socket: &Path,
        actor_id: &str,
        actor_role: &str,
    ) -> Result<Self> {
        let mut cmd = Command::new(pitboss_bin);
        cmd.arg("mcp-bridge")
            .arg(socket)
            .arg("--actor-id")
            .arg(actor_id)
            .arg("--actor-role")
            .arg(actor_role);
        let transport = TokioChildProcess::new(cmd).with_context(|| {
            format!(
                "spawn mcp-bridge via {} (sock={}, actor={}/{})",
                pitboss_bin.display(),
                socket.display(),
                actor_id,
                actor_role,
            )
        })?;
        let inner = ()
            .serve(transport)
            .await
            .with_context(|| format!("mcp init handshake via bridge on {}", socket.display()))?;
        Ok(Self { inner })
    }

    /// Helper: read the trio of bridge env vars and pick the right
    /// connection mode. Returns `None` if no MCP is configured at all,
    /// the direct-socket path if only `PITBOSS_FAKE_MCP_SOCKET` is set,
    /// and the bridge path if all three bridge vars are set.
    pub async fn connect_from_env(socket: &Path) -> Result<Self> {
        let bridge_cmd = std::env::var_os("PITBOSS_FAKE_MCP_BRIDGE_CMD");
        if let Some(cmd) = bridge_cmd {
            let actor_id = std::env::var("PITBOSS_FAKE_ACTOR_ID")
                .context("PITBOSS_FAKE_MCP_BRIDGE_CMD set but PITBOSS_FAKE_ACTOR_ID missing")?;
            let actor_role = std::env::var("PITBOSS_FAKE_ACTOR_ROLE")
                .context("PITBOSS_FAKE_MCP_BRIDGE_CMD set but PITBOSS_FAKE_ACTOR_ROLE missing")?;
            return Self::connect_via_bridge(&PathBuf::from(cmd), socket, &actor_id, &actor_role)
                .await;
        }
        Self::connect(socket).await
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
