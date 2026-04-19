//! Stdio <-> unix-socket bridge. When a claude subprocess launches
//! `pitboss mcp-bridge --actor-id <id> --actor-role <role> <socket>`,
//! this process reads MCP-over-stdio from claude and forwards it to
//! the pitboss MCP server listening on the unix socket.
//!
//! Client-to-server (c2s) direction: parse each JSON-RPC line. For
//! `tools/call` requests, inject `_meta: {actor_id, actor_role}` into
//! `params.arguments` so the dispatcher can identify the caller and
//! enforce namespace authz. Non-tools/call lines pass through unchanged.
//! Malformed JSON passes through byte-for-byte (the dispatcher will
//! reject it with a normal parse error).
//!
//! Server-to-client (s2c) direction: pure byte passthrough. No parsing,
//! no mutation.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::cli::ActorRoleArg;

const ALLOWED_ROLES: &[&str] = &["root_lead", "lead", "sublead", "worker"];

fn role_str(role: ActorRoleArg) -> &'static str {
    match role {
        ActorRoleArg::Lead => "lead",
        ActorRoleArg::Worker => "worker",
    }
}

/// Inject `_meta: {actor_id, actor_role}` into a JSON-RPC request's
/// `params.arguments` if it is a `tools/call` request (matching MCP wire convention).
/// The actor_role must be one of the allowed roles: "root_lead", "lead", "sublead", "worker".
/// For non-`tools/call` requests, the request is left unchanged.
/// Writes to `params.arguments._meta`, not `params._meta`, to match the wire-path behavior.
pub fn inject_meta(request: &mut Value, actor_id: &str, actor_role: &str) {
    if !ALLOWED_ROLES.contains(&actor_role) {
        return; // silently ignore disallowed roles
    }

    if let Value::Object(obj) = request {
        let method = obj.get("method").and_then(|v| v.as_str()).unwrap_or("");
        if method != "tools/call" {
            return; // not a tools/call, leave unchanged
        }

        // tools/call — mutate params.arguments._meta
        let params = obj.entry("params").or_insert(Value::Object(Map::new()));
        if let Value::Object(params_obj) = params {
            let arguments = params_obj
                .entry("arguments")
                .or_insert(Value::Object(Map::new()));
            if let Value::Object(args_obj) = arguments {
                let meta = serde_json::json!({
                    "actor_id": actor_id,
                    "actor_role": actor_role,
                });
                args_obj.insert("_meta".to_string(), meta);
            }
        }
    }
}

/// Parse a single JSON-RPC line and, if it's a `tools/call` request,
/// inject `_meta: {actor_id, actor_role}` into `params.arguments`.
/// Non-`tools/call` requests pass through unchanged. Malformed JSON
/// passes through byte-identical (the dispatcher will reject it).
pub(crate) fn inject_meta_line(line: &[u8], actor_id: &str, actor_role: &str) -> Result<Vec<u8>> {
    let trailing_nl = line.last() == Some(&b'\n');
    let trimmed = if trailing_nl {
        &line[..line.len() - 1]
    } else {
        line
    };

    let mut parsed: Value = match serde_json::from_slice::<Value>(trimmed) {
        Ok(v) => v,
        Err(_) => return Ok(line.to_vec()), // pass through on malformed input
    };

    let Value::Object(_) = parsed else {
        return Ok(line.to_vec());
    };

    // Delegate to inject_meta to handle the actual injection logic
    inject_meta(&mut parsed, actor_id, actor_role);

    let mut out = serde_json::to_vec(&parsed)?;
    if trailing_nl {
        out.push(b'\n');
    }
    Ok(out)
}

pub async fn run_bridge(socket: &Path, actor_id: &str, actor_role: ActorRoleArg) -> Result<i32> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect to pitboss mcp socket at {}", socket.display()))?;
    let (mut sr, mut sw) = stream.split();
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let actor_id = actor_id.to_string();
    let role_s = role_str(actor_role).to_string();

    // c2s: line-parse, inject _meta on tools/call, forward
    let c2s = async {
        let mut reader = BufReader::new(stdin);
        let mut line: Vec<u8> = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let injected = inject_meta_line(&line, &actor_id, &role_s)
                        .unwrap_or_else(|_| line.clone());
                    if sw.write_all(&injected).await.is_err() {
                        break;
                    }
                    if sw.flush().await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    };

    // s2c: byte-level passthrough, no parsing
    let s2c = async {
        let mut buf = vec![0u8; 8192];
        loop {
            match sr.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if stdout.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    if stdout.flush().await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    };

    tokio::select! {
        _ = s2c => {}
        _ = c2s => {}
    }

    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    /// Sanity-check the unix-socket scaffolding the bridge relies on. We spin
    /// up a minimal echo server on a unix socket, connect to it directly, and
    /// verify the bytes round-trip. This does not exercise `run_bridge`
    /// itself (that requires hooking stdin/stdout, which is fiddly in a unit
    /// test) but it proves the transport primitives work as expected.
    #[tokio::test]
    async fn bridge_round_trip_echoes_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket = dir.path().join("echo.sock");

        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 1024];
                let n = stream.read(&mut buf).await.unwrap();
                stream.write_all(&buf[..n]).await.unwrap();
                stream.flush().await.unwrap();
            }
        });

        // Connect directly (not through the bridge) to verify the echo works.
        let mut client = UnixStream::connect(&socket).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        client.flush().await.unwrap();
        let mut buf = [0u8; 5];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");

        // Ensure the server task completes so the test is fully deterministic.
        let _ = server.await;
    }

    #[test]
    fn bridge_injects_meta_into_tool_calls() {
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"kv_get","arguments":{"path":"/ref/k"}}}
"#;
        let out = inject_meta_line(input, "worker-A", "worker").unwrap();
        let out_str = String::from_utf8(out).unwrap();
        assert!(
            out_str.contains(r#""_meta":{"actor_id":"worker-A","actor_role":"worker"}"#),
            "expected _meta injection, got:\n{out_str}"
        );
        assert!(
            out_str.contains(r#""path":"/ref/k""#),
            "original arguments should still be present"
        );
    }

    #[test]
    fn bridge_passes_non_tool_calls_through_unchanged() {
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
"#;
        let out = inject_meta_line(input, "worker-A", "worker").unwrap();
        let out_str = String::from_utf8(out).unwrap();
        assert!(
            !out_str.contains(r#""_meta""#),
            "non-tools/call requests must not carry _meta"
        );
    }

    #[test]
    fn bridge_passes_malformed_json_through_verbatim() {
        let input = b"{not valid json\n";
        let out = inject_meta_line(input, "worker-A", "worker").unwrap();
        assert_eq!(
            out, input,
            "malformed input must pass through byte-identical"
        );
    }

    #[test]
    fn bridge_handles_line_without_trailing_newline() {
        // Last line of a stream may lack a trailing newline; still parse it.
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"kv_get","arguments":{}}}"#;
        let out = inject_meta_line(input, "w", "worker").unwrap();
        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains(r#""_meta""#));
        assert!(!out_str.ends_with('\n'), "must not invent a newline");
    }
}
