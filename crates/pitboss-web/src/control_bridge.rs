//! Per-run control-socket bridge. Each subscribed run gets ONE
//! `UnixStream` connection to its dispatcher; events are fanned out to N
//! SSE clients via a `tokio::sync::broadcast` channel and outbound
//! control ops are serialized through the shared write-half.
//!
//! ## Lifecycle
//!
//! 1. First `subscribe(run_id)` (or `send_op(run_id, _)`) opens the
//!    socket, sends `Hello`, splits the stream, spawns a reader task,
//!    and registers an `Entry { tx, write }` in the bridge map.
//! 2. Subsequent `subscribe(run_id)` calls return additional receivers
//!    against the same sender — no second connection.
//! 3. `send_op(run_id, op)` serializes the op as a single JSON line and
//!    writes it to the shared write-half under a tokio mutex.
//! 4. Reader task pumps `EventEnvelope`s from the socket into the
//!    broadcast channel until the socket EOFs.
//! 5. On exit, the reader removes the entry from the bridge map; the
//!    next subscribe/send_op will reconnect.
//!
//! ## Single-client constraint
//!
//! The dispatcher accepts at-most-one control client at a time. If a TUI
//! is already connected when the web bridge tries to take the slot, the
//! dispatcher emits `Superseded` to the TUI and binds us. v1 takes the
//! slot opportunistically — the SPA surfaces a banner if WE in turn get
//! superseded by another client (the bridge sees `Superseded` in the
//! event stream, fans it out, and the reader EOFs shortly after).
//!
//! ## Lost-wakeup safety
//!
//! `broadcast::channel` buffers up to `CHANNEL_CAPACITY` events for each
//! receiver, so the gap between `subscribe()` returning and the SSE
//! handler starting to consume cannot lose the dispatcher's initial
//! `Hello`. If a subscriber falls behind by more than the capacity, the
//! channel emits `Lagged(n)` — the SSE handler surfaces this as a typed
//! `lagged` SSE event so the client can resync (re-fetch state).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use pitboss_cli::control::protocol::{ControlEvent, ControlOp, EventEnvelope};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, info, warn};

const CHANNEL_CAPACITY: usize = 256;

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
    write: Arc<Mutex<OwnedWriteHalf>>,
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

    /// Subscribe to a run's control events. Reuses an existing socket
    /// connection if one is already established; otherwise opens a new
    /// one and spawns the reader task.
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
    /// over the event stream — callers that need it should subscribe to
    /// the event stream first.
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

    /// Returns the per-run `Entry`, opening the socket on first use.
    async fn ensure_connected(&self, run_id: &str) -> Result<Entry, BridgeError> {
        let mut map = self.inner.lock().await;
        if let Some(entry) = map.get(run_id) {
            return Ok(entry.clone());
        }

        // Mirror pitboss_cli::runs::resolve_socket_path: prefer the
        // XDG_RUNTIME_DIR socket the dispatcher actually publishes to,
        // and fall back to the in-run-dir path for environments without
        // an XDG runtime dir (the CLI's `pub`-able resolver should
        // ultimately replace this duplication — tracked separately).
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

        // Send the Hello handshake. The dispatcher replies with its own
        // Hello as the first event on the wire — our reader task picks
        // it up after we register the broadcast sender.
        let hello = ControlOp::Hello {
            client_version: format!("pitboss-web/{}", env!("CARGO_PKG_VERSION")),
        };
        let mut hello_line =
            serde_json::to_string(&hello).map_err(|e| BridgeError::Handshake(e.to_string()))?;
        hello_line.push('\n');
        stream.write_all(hello_line.as_bytes()).await?;

        let (read_half, write_half) = stream.into_split();
        let (tx, _initial_rx) = broadcast::channel::<EventEnvelope>(CHANNEL_CAPACITY);
        let entry = Entry {
            tx: tx.clone(),
            write: Arc::new(Mutex::new(write_half)),
        };

        let tx_for_task = tx.clone();
        let run_id_for_task = run_id.to_string();
        let inner_for_task = Arc::clone(&self.inner);
        tokio::spawn(async move {
            reader_loop(read_half, tx_for_task, run_id_for_task, inner_for_task).await;
        });
        map.insert(run_id.to_string(), entry.clone());
        info!(run_id, "control bridge connection opened");
        Ok(entry)
    }
}

async fn reader_loop(
    read_half: tokio::net::unix::OwnedReadHalf,
    tx: broadcast::Sender<EventEnvelope>,
    run_id: String,
    registry: Arc<Mutex<HashMap<String, Entry>>>,
) {
    let mut lines = BufReader::new(read_half).lines();

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                let envelope = match serde_json::from_str::<EventEnvelope>(&line) {
                    Ok(e) => e,
                    Err(_) => match serde_json::from_str::<ControlEvent>(&line) {
                        // Pre-v0.6 dispatchers wire bare ControlEvent
                        // without the EventEnvelope wrapper. Lift to
                        // empty-actor-path envelope for uniformity.
                        Ok(ev) => EventEnvelope {
                            actor_path: Default::default(),
                            event: ev,
                        },
                        Err(e) => {
                            warn!(run_id, error = %e, line = %line, "control event parse failed");
                            continue;
                        }
                    },
                };
                // tx.send returns Err(()) only when there are zero
                // receivers AND zero buffered items. The bridge is
                // designed to outlive subscriber gaps (a control-only
                // POST keeps the entry alive without a receiver), so we
                // tolerate send errors by simply dropping the event —
                // the entry stays registered until reader EOF.
                let _ = tx.send(envelope);
            }
            Ok(None) => {
                debug!(run_id, "control socket closed by server (EOF)");
                break;
            }
            Err(e) => {
                warn!(run_id, error = %e, "control socket read error");
                break;
            }
        }
    }

    // Best-effort cleanup; the next subscribe()/send_op() will
    // re-establish if the dispatcher is back. If the entry has already
    // been replaced by a newer reader (race on `ensure_connected`
    // between two callers), only remove our own.
    let mut map = registry.lock().await;
    if let Some(existing) = map.get(&run_id) {
        if existing.tx.same_channel(&tx) {
            map.remove(&run_id);
        }
    }
    info!(run_id, "control bridge reader exited");
}
