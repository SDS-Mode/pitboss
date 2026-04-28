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
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::cli::ActorRoleArg;

/// Bounded drain window for server→client responses after the c2s
/// path has signalled EOF (`sw.shutdown()`). 5s is generous for a
/// well-behaved MCP server to flush in-flight responses; we cap it
/// so a buggy server can't keep the bridge subprocess alive
/// indefinitely.
const S2C_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

const ALLOWED_ROLES: &[&str] = &["root_lead", "lead", "sublead", "worker"];

/// Hard cap on a single JSON-RPC line from the child's stdout. A buggy or
/// hostile subprocess that never emits `\n` would otherwise grow the read
/// buffer until pitboss OOMs. 4 MiB is well above any legitimate MCP message.
const MAX_C2S_LINE_BYTES: usize = 4 * 1024 * 1024;

fn role_str(role: ActorRoleArg) -> &'static str {
    match role {
        ActorRoleArg::Lead => "lead",
        ActorRoleArg::RootLead => "root_lead",
        ActorRoleArg::Sublead => "sublead",
        ActorRoleArg::Worker => "worker",
    }
}

/// Inject `_meta: {actor_id, actor_role[, token]}` into a JSON-RPC request's
/// `params.arguments` if it is a `tools/call` request (matching MCP wire convention).
/// The actor_role must be one of the allowed roles: "root_lead", "lead", "sublead", "worker".
/// For non-`tools/call` requests, the request is left unchanged.
/// Writes to `params.arguments._meta`, not `params._meta`, to match the wire-path behavior.
///
/// When `token` is `Some`, it is added as `_meta.token`. The server validates
/// the token against `DispatchState::actor_tokens` and binds the connection's
/// canonical identity from the lookup result — so even if the wire `actor_id`
/// / `actor_role` are forged by a direct (non-bridge) socket connection, authz
/// uses the bound identity. Closes #145.
pub fn inject_meta(request: &mut Value, actor_id: &str, actor_role: &str, token: Option<&str>) {
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
                let mut meta = serde_json::Map::new();
                meta.insert("actor_id".into(), Value::String(actor_id.to_string()));
                meta.insert("actor_role".into(), Value::String(actor_role.to_string()));
                if let Some(t) = token {
                    meta.insert("token".into(), Value::String(t.to_string()));
                }
                args_obj.insert("_meta".to_string(), Value::Object(meta));
            }
        }
    }
}

/// Parse a single JSON-RPC line and, if it's a `tools/call` request,
/// inject `_meta: {actor_id, actor_role[, token]}` into `params.arguments`.
/// Non-`tools/call` requests pass through unchanged. Malformed JSON
/// passes through byte-identical (the dispatcher will reject it).
pub(crate) fn inject_meta_line(
    line: &[u8],
    actor_id: &str,
    actor_role: &str,
    token: Option<&str>,
) -> Result<Vec<u8>> {
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
    inject_meta(&mut parsed, actor_id, actor_role, token);

    let mut out = serde_json::to_vec(&parsed)?;
    if trailing_nl {
        out.push(b'\n');
    }
    Ok(out)
}

pub async fn run_bridge(
    socket: &Path,
    actor_id: &str,
    actor_role: ActorRoleArg,
    token: Option<&str>,
) -> Result<i32> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect to pitboss mcp socket at {}", socket.display()))?;
    let (sr, sw) = stream.into_split();
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    run_bridge_io(
        sr,
        sw,
        stdin,
        stdout,
        actor_id.to_string(),
        role_str(actor_role).to_string(),
        token.map(str::to_string),
    )
    .await
}

/// IO-generic bridge body. Extracted so tests can drive the c2s/s2c
/// pair against `tokio::io::duplex` instead of needing real stdio +
/// a unix socket. Behaviour is identical to the production
/// `run_bridge` flow against a `UnixStream`. (#151 L1)
///
/// Half-close semantics: when c2s observes EOF on stdin (or hits its
/// per-line cap), it explicitly shuts down the socket write half so
/// the server sees end-of-input and finishes responding to in-flight
/// requests. The select! arm that fires on c2s completion then
/// awaits s2c with a bounded timeout to drain those final responses
/// to stdout — without that drain, the previous bare select! dropped
/// in-flight server→client bytes the moment c2s won. (#151 L1)
async fn run_bridge_io<SR, SW, IN, OUT>(
    mut sr: SR,
    mut sw: SW,
    mut stdin: IN,
    mut stdout: OUT,
    actor_id: String,
    role_s: String,
    token_s: Option<String>,
) -> Result<i32>
where
    SR: AsyncRead + Unpin,
    SW: AsyncWrite + Unpin,
    IN: AsyncRead + Unpin,
    OUT: AsyncWrite + Unpin,
{
    // c2s: line-parse, inject _meta on tools/call, forward.
    // Chunked read with an explicit per-line cap so a child that never emits
    // `\n` can't OOM the host. We read straight from stdin — the manual line
    // accumulator means a BufReader wrapper would just add a second copy.
    //
    // SECURITY: Do NOT log `token_s` or include it in error/diagnostic
    // messages. The token is the only thing standing between a same-UID
    // attacker who can read mcp-config.json and an attacker who can also
    // forge identity. Tracing it would land it in the run's log file.
    let c2s = async {
        let mut line: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        'outer: loop {
            match stdin.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    for &b in &chunk[..n] {
                        line.push(b);
                        if b == b'\n' {
                            let injected =
                                inject_meta_line(&line, &actor_id, &role_s, token_s.as_deref())
                                    .unwrap_or_else(|_| line.clone());
                            if sw.write_all(&injected).await.is_err() {
                                break 'outer;
                            }
                            if sw.flush().await.is_err() {
                                break 'outer;
                            }
                            line.clear();
                        } else if line.len() > MAX_C2S_LINE_BYTES {
                            tracing::error!(
                                len = line.len(),
                                cap = MAX_C2S_LINE_BYTES,
                                "mcp-bridge: c2s line exceeds cap, closing bridge",
                            );
                            break 'outer;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        // Flush the final line if the stream ended without a trailing newline
        // and it's within the cap.
        if !line.is_empty() && line.len() <= MAX_C2S_LINE_BYTES {
            let injected = inject_meta_line(&line, &actor_id, &role_s, token_s.as_deref())
                .unwrap_or_else(|_| line.clone());
            let _ = sw.write_all(&injected).await;
            let _ = sw.flush().await;
        }
        // #151 L1: half-close our socket write half so the server
        // observes end-of-input. Without this the server would keep
        // its read half open until the bridge process exited, and
        // the bare select! below dropped any in-flight server→client
        // responses the instant c2s won.
        let _ = sw.shutdown().await;
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

    tokio::pin!(c2s);
    tokio::pin!(s2c);
    tokio::select! {
        () = &mut c2s => {
            // c2s shut down sw; drain remaining server→client bytes
            // until s2c sees EOF (or the bounded timeout fires).
            let _ = tokio::time::timeout(S2C_DRAIN_TIMEOUT, &mut s2c).await;
        }
        () = &mut s2c => {
            // Server closed the socket — no more responses possible.
            // Don't bother draining stdin: there's no one to deliver
            // those bytes to.
        }
    }

    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    #[test]
    fn role_str_covers_every_actor_role_variant() {
        // Regression: CLI previously had only Lead + Worker. Sub-lead mcp
        // configs write `--actor-role sublead`; clap rejected the argv,
        // mcp-bridge never started, and claude reported `pitboss: failed`.
        // Silent depth-2 break since v0.6. These four strings are load-
        // bearing: they must match the server-side ALLOWED_ROLES list.
        assert_eq!(role_str(ActorRoleArg::Lead), "lead");
        assert_eq!(role_str(ActorRoleArg::RootLead), "root_lead");
        assert_eq!(role_str(ActorRoleArg::Sublead), "sublead");
        assert_eq!(role_str(ActorRoleArg::Worker), "worker");
        for role in [
            ActorRoleArg::Lead,
            ActorRoleArg::RootLead,
            ActorRoleArg::Sublead,
            ActorRoleArg::Worker,
        ] {
            assert!(
                ALLOWED_ROLES.contains(&role_str(role)),
                "role {:?} serializes to {:?} which is not in ALLOWED_ROLES \
                 {ALLOWED_ROLES:?}",
                role,
                role_str(role)
            );
        }
    }

    #[test]
    fn clap_accepts_sublead_and_root_lead_role_tokens() {
        // Token-level check: the mcp-bridge subcommand must accept every
        // string that pitboss itself writes into mcp-config files. Before
        // this fix, `pitboss mcp-bridge --actor-role sublead ...` failed
        // with `invalid value 'sublead' for '--actor-role'` before even
        // attempting to connect the socket.
        use clap::ValueEnum;
        for token in ["lead", "root_lead", "sublead", "worker"] {
            assert!(
                ActorRoleArg::from_str(token, false).is_ok(),
                "clap rejected actor-role token {token:?}"
            );
        }
    }

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
        let out = inject_meta_line(input, "worker-A", "worker", None).unwrap();
        let out_str = String::from_utf8(out).unwrap();
        assert!(
            out_str.contains(r#""actor_id":"worker-A""#),
            "expected actor_id injection, got:\n{out_str}"
        );
        assert!(
            out_str.contains(r#""actor_role":"worker""#),
            "expected actor_role injection, got:\n{out_str}"
        );
        assert!(
            out_str.contains(r#""path":"/ref/k""#),
            "original arguments should still be present"
        );
    }

    #[test]
    fn bridge_injects_token_when_provided() {
        // Issue #145: --token argv adds _meta.token, which the server uses
        // to bind connection identity (rejecting forged actor_role on direct
        // socket connections).
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"kv_get","arguments":{}}}
"#;
        let out = inject_meta_line(input, "worker-A", "worker", Some("tok-1234")).unwrap();
        let out_str = String::from_utf8(out).unwrap();
        assert!(
            out_str.contains(r#""token":"tok-1234""#),
            "expected token injection, got:\n{out_str}"
        );
    }

    #[test]
    fn bridge_omits_token_field_when_not_provided() {
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"kv_get","arguments":{}}}
"#;
        let out = inject_meta_line(input, "worker-A", "worker", None).unwrap();
        let out_str = String::from_utf8(out).unwrap();
        assert!(
            !out_str.contains(r#""token""#),
            "token field must be absent when no token supplied, got:\n{out_str}"
        );
    }

    #[test]
    fn bridge_passes_non_tool_calls_through_unchanged() {
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
"#;
        let out = inject_meta_line(input, "worker-A", "worker", None).unwrap();
        let out_str = String::from_utf8(out).unwrap();
        assert!(
            !out_str.contains(r#""_meta""#),
            "non-tools/call requests must not carry _meta"
        );
    }

    #[test]
    fn bridge_passes_malformed_json_through_verbatim() {
        let input = b"{not valid json\n";
        let out = inject_meta_line(input, "worker-A", "worker", None).unwrap();
        assert_eq!(
            out, input,
            "malformed input must pass through byte-identical"
        );
    }

    #[test]
    fn bridge_handles_line_without_trailing_newline() {
        // Last line of a stream may lack a trailing newline; still parse it.
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"kv_get","arguments":{}}}"#;
        let out = inject_meta_line(input, "w", "worker", None).unwrap();
        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains(r#""_meta""#));
        assert!(!out_str.ends_with('\n'), "must not invent a newline");
    }

    /// #151 L1 regression: when the c2s side EOFs first, the bridge
    /// must drain in-flight server→client bytes to stdout instead of
    /// dropping them. Pre-fix the bare `tokio::select!` exited the
    /// instant c2s completed, taking s2c down with it mid-write —
    /// any response queued on the socket but not yet copied to
    /// stdout was lost.
    ///
    /// We drive `run_bridge_io` directly with a real `UnixStream`
    /// pair (so EOF propagates the way it does in production) and
    /// `tokio::io::duplex` stand-ins for the bridge's stdio. The
    /// "server" pre-loads a JSON-RPC response onto the socket and
    /// then drops its end, so the bridge's s2c side sees the
    /// response followed by a clean EOF; "stdin" is closed
    /// immediately so c2s EOFs first.
    #[tokio::test]
    async fn bridge_drains_in_flight_s2c_when_c2s_eofs_first() {
        // Real unix-socket pair so EOF propagates correctly when
        // the test's "server" side is dropped.
        let (server_stream, bridge_stream) = UnixStream::pair().unwrap();
        let (sr, sw) = bridge_stream.into_split();
        let (mut server_read, mut server_write) = server_stream.into_split();

        // Stdin: writer side closed immediately so the bridge sees
        // EOF on its first read.
        let (stdin_writer, stdin_reader) = tokio::io::duplex(64);
        drop(stdin_writer);

        // Stdout: writer is what the bridge writes through; reader
        // is the test's view of bytes that landed on stdout.
        let (stdout_writer, mut stdout_reader) = tokio::io::duplex(8192);

        // Pre-load a "server response" onto the socket. The bridge's
        // s2c loop should pick this up and forward it to stdout
        // even though c2s is about to end immediately.
        let response = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"ok\"}\n";
        server_write.write_all(response).await.unwrap();
        server_write.flush().await.unwrap();
        // Drop the server's write half *and* the read half so the
        // bridge's s2c sees EOF after consuming the response. With
        // the half-close fix in place, the bridge half-closes sw on
        // c2s EOF — the test asserts that propagates by reading
        // server_read to EOF below.
        drop(server_write);

        let bridge = tokio::spawn(async move {
            run_bridge_io(
                sr,
                sw,
                stdin_reader,
                stdout_writer,
                "worker-A".to_string(),
                "worker".to_string(),
                None,
            )
            .await
        });

        // Reading server_read to EOF asserts two things at once:
        // (1) the bridge half-closed sw on c2s EOF (so the server
        // side sees EOF here at all), and (2) the await terminates
        // promptly rather than blocking until S2C_DRAIN_TIMEOUT.
        let mut sink = Vec::new();
        server_read.read_to_end(&mut sink).await.unwrap();
        assert!(
            sink.is_empty(),
            "server received no client bytes (stdin was empty)"
        );

        let exit = bridge.await.unwrap().unwrap();
        assert_eq!(exit, 0);

        // After the bridge returned, stdout_writer was dropped so
        // the reader can drain to EOF.
        let mut got = Vec::new();
        stdout_reader.read_to_end(&mut got).await.unwrap();
        assert_eq!(
            got, response,
            "in-flight server→client response must reach stdout when c2s EOFs first"
        );
    }
}
