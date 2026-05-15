//! Per-run control-socket bridge.
//!
//! After PR-Q of #438 each run carries two dispatcher connections:
//!
//! - **Writer connection** (writer-mode Hello — `ClientMode::Writer`,
//!   elided on the wire for byte-identical legacy bytes). Owns the
//!   single `OwnedWriteHalf` that `send_op` writes through. A drain
//!   task reads-and-discards from the writer's read half so the
//!   kernel buffer doesn't backpressure dispatcher fan-out — we don't
//!   need its envelope stream because the subscriber arm covers SSE.
//!
//! - **Subscriber-mode unified-API pump.** Drives
//!   `pitboss_core::stream::open_run_stream(StreamMode::ReplayThenLive)`,
//!   filters to `RunStreamPayload::Event(_)` items, and pushes the
//!   inner envelope into the per-run `broadcast::Sender<EventEnvelope>`
//!   that SSE clients subscribe to. PR-P's subscriber-mode handshake
//!   guarantees this connection doesn't displace the writer.
//!
//! Op replies (`OpAcked`, `OpFailed`, `OpUnknownState`,
//! `WorkersSnapshot`) reach SSE clients via the dispatcher's broadcast
//! bus rather than via the writer's read half — PR-Q changed the
//! dispatcher's read loop to route them through `events_tx`.
//!
//! ## Lifecycle
//!
//! 1. First `subscribe(run_id)` or `send_op(run_id, _)` opens the
//!    writer socket, spawns the writer-drain task, spawns the unified-API
//!    pump, and registers an `Entry { tx, write }` in the bridge map.
//! 2. Subsequent `subscribe(run_id)` calls return additional receivers
//!    against the same sender — no second connection pair.
//! 3. `send_op(run_id, op)` serializes the op as one JSON line and
//!    writes it to the shared write-half under a tokio mutex.
//! 4. The unified-API pump exits when the dispatcher EOFs the
//!    subscriber socket or `open_run_stream` ends; on exit the entry is
//!    removed from the map so the next call reconnects.
//!
//! ## Lost-wakeup safety
//!
//! `broadcast::channel` buffers up to `CHANNEL_CAPACITY` events for
//! each receiver, so the gap between `subscribe()` returning and the
//! SSE handler starting to consume cannot lose the dispatcher's
//! initial `Hello`. If a subscriber falls behind by more than the
//! capacity, the channel emits `Lagged(n)` — the SSE handler surfaces
//! this as a typed `lagged` SSE event so the client can resync.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt;
use pitboss_cli::control::protocol::{ControlOp, EventEnvelope};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixStream};
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, info, warn};

const CHANNEL_CAPACITY: usize = 256;

/// Boxed-trait-object writer half. The bridge connects to either a
/// `UnixStream` (host-mode runs) or a `TcpStream` (container-dispatch
/// runs that publish a control_tcp_addr in meta.json — #474). The split
/// write halves are different concrete types, so they're boxed at the
/// boundary into a single `AsyncWrite + Unpin + Send` trait object.
type BoxedWriter = Box<dyn AsyncWrite + Unpin + Send>;

/// Boxed-trait-object reader half. See `BoxedWriter`.
type BoxedReader = Box<dyn AsyncRead + Unpin + Send>;

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("control socket not found")]
    NotFound,
    #[error("control socket exists but no listener (dispatcher exited)")]
    Dead,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("handshake: {0}")]
    Handshake(String),
    #[error("op rejected: {0}")]
    Rejected(String),
}

/// Per-run state: the broadcast sender for events going OUT to SSE
/// clients, and the shared write-half for ops coming IN from REST clients.
#[derive(Clone)]
struct Entry {
    tx: broadcast::Sender<EventEnvelope>,
    write: Arc<Mutex<BoxedWriter>>,
}

#[derive(Clone)]
pub struct ControlBridge {
    runs_dir: Arc<PathBuf>,
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

impl ControlBridge {
    pub fn new(runs_dir: PathBuf) -> Self {
        Self {
            runs_dir: Arc::new(runs_dir),
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Subscribe to a run's control events. Reuses an existing
    /// connection pair if one is already established; otherwise opens
    /// a new one and spawns the writer-drain + unified-API pump tasks.
    pub async fn subscribe(
        &self,
        run_id: &str,
    ) -> Result<broadcast::Receiver<EventEnvelope>, BridgeError> {
        let entry = self.ensure_connected(run_id).await?;
        Ok(entry.tx.subscribe())
    }

    /// Send a single `ControlOp` to the dispatcher. Auto-connects the
    /// bridge if no SSE subscriber has opened it yet. The op is
    /// serialized as one JSON line and written under the per-run
    /// write-half mutex, so concurrent `send_op` calls cannot interleave
    /// bytes on the socket.
    ///
    /// Returns `Ok(())` once the bytes are flushed to the socket. The
    /// dispatcher's `OpAcked` / `OpFailed` reply is delivered out-of-band
    /// over the event stream (PR-Q of #438 routes op replies through the
    /// broadcast bus, so subscribers see them too) — callers that need
    /// the reply should subscribe to the event stream first.
    pub async fn send_op(&self, run_id: &str, op: &ControlOp) -> Result<(), BridgeError> {
        // Block server-only handshake variant: clients must not impersonate
        // the dispatcher's Hello; the bridge sends its own client Hello
        // automatically when establishing the socket.
        if matches!(op, ControlOp::Hello { .. }) {
            return Err(BridgeError::Rejected(
                "client cannot send hello; bridge handles handshake".into(),
            ));
        }

        let entry = self.ensure_connected(run_id).await?;
        let mut line =
            serde_json::to_string(op).map_err(|e| BridgeError::Handshake(e.to_string()))?;
        line.push('\n');
        let mut guard = entry.write.lock().await;
        guard.write_all(line.as_bytes()).await?;
        guard.flush().await?;
        Ok(())
    }

    /// Returns the per-run `Entry`, opening the writer connection and
    /// spawning the writer-drain + unified-API pump tasks on first use.
    async fn ensure_connected(&self, run_id: &str) -> Result<Entry, BridgeError> {
        let mut map = self.inner.lock().await;
        if let Some(entry) = map.get(run_id) {
            return Ok(entry.clone());
        }

        // Transport selection (#474):
        //
        // 1. If meta.json carries `control_tcp_addr`, dial TCP. This is
        //    populated by `pitboss container-dispatch` on platforms
        //    where the in-container AF_UNIX socket isn't visible from
        //    the host (macOS+Podman virtiofs).
        // 2. Otherwise, resolve the AF_UNIX socket path (XDG runtime
        //    dir first, then runs-dir fallback) and dial UnixStream.
        //
        // PR-Q preserves the pre-existing 404-on-cold-runs contract for
        // the AF_UNIX path: if the socket doesn't exist, return
        // `NotFound` rather than silently letting
        // `open_run_stream(ReplayThenLive)` replay disk events.jsonl
        // over the SSE feed — that's the dedicated Replay tab's job via
        // `/api/runs/:id/events-jsonl`.
        let tcp_addr = read_control_tcp_addr(&self.runs_dir, run_id);
        let (writer_read_half, writer_write_half): (BoxedReader, BoxedWriter) = match &tcp_addr {
            Some(addr) => {
                let mut stream = match TcpStream::connect(addr).await {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                        return Err(BridgeError::Dead);
                    }
                    Err(e) => return Err(BridgeError::Io(e)),
                };
                let hello = ControlOp::Hello {
                    client_version: format!("pitboss-web/{}", env!("CARGO_PKG_VERSION")),
                    mode: pitboss_cli::control::protocol::ClientMode::default(),
                };
                let mut hello_line = serde_json::to_string(&hello)
                    .map_err(|e| BridgeError::Handshake(e.to_string()))?;
                hello_line.push('\n');
                stream.write_all(hello_line.as_bytes()).await?;
                let (rh, wh) = stream.into_split();
                (Box::new(rh), Box::new(wh))
            }
            None => {
                let socket_path = std::env::var_os("XDG_RUNTIME_DIR")
                    .map(|x| {
                        std::path::PathBuf::from(x)
                            .join("pitboss")
                            .join(format!("{run_id}.control.sock"))
                    })
                    .filter(|p| p.exists())
                    .unwrap_or_else(|| self.runs_dir.join(run_id).join("control.sock"));
                if !socket_path.exists() {
                    return Err(BridgeError::NotFound);
                }
                let mut stream = match UnixStream::connect(&socket_path).await {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                        return Err(BridgeError::Dead);
                    }
                    Err(e) => return Err(BridgeError::Io(e)),
                };
                let hello = ControlOp::Hello {
                    client_version: format!("pitboss-web/{}", env!("CARGO_PKG_VERSION")),
                    mode: pitboss_cli::control::protocol::ClientMode::default(),
                };
                let mut hello_line = serde_json::to_string(&hello)
                    .map_err(|e| BridgeError::Handshake(e.to_string()))?;
                hello_line.push('\n');
                stream.write_all(hello_line.as_bytes()).await?;
                let (rh, wh) = stream.into_split();
                (Box::new(rh), Box::new(wh))
            }
        };

        let (tx, _initial_rx) = broadcast::channel::<EventEnvelope>(CHANNEL_CAPACITY);
        let entry = Entry {
            tx: tx.clone(),
            write: Arc::new(Mutex::new(writer_write_half)),
        };

        // 2. Writer-drain task: read-and-discard envelopes the dispatcher
        //    sends to this writer connection (its own Hello, broadcast
        //    fan-out, etc.). The subscriber arm covers SSE delivery; the
        //    drain just keeps the kernel buffer from filling up and
        //    backpressuring dispatcher writes.
        let run_id_drain = run_id.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(writer_read_half).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(_)) => {} // discard
                    Ok(None) => {
                        debug!(run_id = %run_id_drain, "writer-side drain task: socket EOF");
                        break;
                    }
                    Err(e) => {
                        debug!(run_id = %run_id_drain, error = %e, "writer-side drain task: read error");
                        break;
                    }
                }
            }
        });

        // 3. Unified-API pump: drive `open_run_stream(ReplayThenLive)`
        //    against the per-run dir, filter to `Event` payloads, and
        //    forward into the broadcast tx. The unified API opens its own
        //    subscriber-mode UnixStream internally (PR-P), so this is a
        //    separate, non-displacing connection.
        let run_dir = self.runs_dir.join(run_id);
        let tx_for_pump = tx.clone();
        let run_id_for_pump = run_id.to_string();
        let inner_for_pump = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let stream = pitboss_core::stream::open_run_stream(
                &run_dir,
                pitboss_core::stream::StreamMode::ReplayThenLive,
            );
            tokio::pin!(stream);
            while let Some(item) = stream.next().await {
                if let pitboss_core::stream::RunStreamPayload::Event(env) = item.payload {
                    // `pitboss_core::control_protocol::EventEnvelope`
                    // holds the inner event as `serde_json::Value`; the
                    // SSE handler downstream expects
                    // `pitboss_cli::control::protocol::EventEnvelope`
                    // with the typed `ControlEvent`. Re-parse via JSON
                    // — one alloc per envelope, negligible at SSE rates.
                    let json = match serde_json::to_string(&*env) {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(run_id = %run_id_for_pump, error = %e,
                                  "control bridge: serialize-then-reparse failed");
                            continue;
                        }
                    };
                    let typed: EventEnvelope = match serde_json::from_str(&json) {
                        Ok(e) => e,
                        Err(e) => {
                            warn!(run_id = %run_id_for_pump, error = %e, line = %json,
                                  "control bridge: typed envelope reparse failed");
                            continue;
                        }
                    };
                    // `tx.send` returns Err only when zero subscribers AND
                    // zero buffered slots. We tolerate that — a
                    // control-only POST can keep the entry alive without
                    // an SSE subscriber.
                    let _ = tx_for_pump.send(typed);
                }
            }
            // Stream EOF — either disk replay ran out on a finalized
            // run, or the live socket dropped. Either way, the entry is
            // stale; remove so the next subscribe() / send_op() opens a
            // fresh pair. Race-safe against `ensure_connected`
            // re-registering: only remove if it's still OUR tx.
            let mut map = inner_for_pump.lock().await;
            if let Some(existing) = map.get(&run_id_for_pump) {
                if existing.tx.same_channel(&tx_for_pump) {
                    map.remove(&run_id_for_pump);
                }
            }
            info!(run_id = %run_id_for_pump, "control bridge unified-API pump exited");
        });

        map.insert(run_id.to_string(), entry.clone());
        info!(
            run_id,
            transport = if tcp_addr.is_some() { "tcp" } else { "unix" },
            "control bridge connection opened (writer + subscriber pump)"
        );
        Ok(entry)
    }
}

/// Read `meta.json` for a run and return the optional `control_tcp_addr`.
/// Returns `None` when meta.json doesn't exist, can't be parsed, or
/// doesn't carry the field (host-mode runs, or container-dispatch runs
/// from before #474). The bridge falls back to AF_UNIX in those cases.
fn read_control_tcp_addr(runs_dir: &std::path::Path, run_id: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct MetaTcpOnly {
        #[serde(default)]
        control_tcp_addr: Option<String>,
    }
    let meta_path = runs_dir.join(run_id).join("meta.json");
    let bytes = std::fs::read(&meta_path).ok()?;
    serde_json::from_slice::<MetaTcpOnly>(&bytes)
        .ok()
        .and_then(|m| m.control_tcp_addr)
}
