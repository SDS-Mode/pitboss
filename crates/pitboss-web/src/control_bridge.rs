//! Per-run control-socket bridge. Each subscribed run gets ONE
//! `UnixStream` connection to its dispatcher; events are fanned out to N
//! SSE clients via a `tokio::sync::broadcast` channel.
//!
//! ## Lifecycle
//!
//! 1. First `subscribe(run_id)` opens the socket, sends `Hello`, spawns a
//!    reader task, and registers a `broadcast::Sender` in the bridge map.
//! 2. Subsequent `subscribe(run_id)` calls return additional receivers
//!    against the same sender — no second connection.
//! 3. Reader task pumps `EventEnvelope`s from the socket into the
//!    broadcast channel until the socket EOFs or all subscribers leave.
//! 4. On exit, the reader removes the entry from the bridge map; the
//!    next subscribe will reconnect.
//!
//! ## Single-client constraint
//!
//! The dispatcher accepts at-most-one control client at a time. If a TUI
//! is already connected when the web bridge tries to take the slot, the
//! dispatcher emits `Superseded` to the TUI and binds us. v1 takes the
//! slot opportunistically — Phase 3's "Take control" UX makes this
//! explicit.
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
}

#[derive(Clone)]
pub struct ControlBridge {
    runs_dir: Arc<PathBuf>,
    inner: Arc<Mutex<HashMap<String, broadcast::Sender<EventEnvelope>>>>,
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
        let mut map = self.inner.lock().await;
        if let Some(tx) = map.get(run_id) {
            return Ok(tx.subscribe());
        }

        let socket_path = self.runs_dir.join(run_id).join("control.sock");
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

        let (tx, rx) = broadcast::channel::<EventEnvelope>(CHANNEL_CAPACITY);
        let tx_for_task = tx.clone();
        let run_id_for_task = run_id.to_string();
        let inner_for_task = Arc::clone(&self.inner);
        tokio::spawn(async move {
            reader_loop(stream, tx_for_task, run_id_for_task, inner_for_task).await;
        });
        map.insert(run_id.to_string(), tx);
        info!(run_id, "control bridge connection opened");
        Ok(rx)
    }
}

async fn reader_loop(
    stream: UnixStream,
    tx: broadcast::Sender<EventEnvelope>,
    run_id: String,
    registry: Arc<Mutex<HashMap<String, broadcast::Sender<EventEnvelope>>>>,
) {
    // Split for reading; we don't write more after Hello in v1 (control
    // ops land in Phase 3 with a separate POST endpoint that opens its
    // own short-lived connection).
    let (read_half, _write_half) = stream.into_split();
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
                if tx.send(envelope).is_err() {
                    debug!(run_id, "no subscribers; closing control bridge");
                    break;
                }
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

    // Best-effort cleanup; the next subscribe() will re-establish if the
    // dispatcher is back. If the entry has already been replaced by a
    // newer reader (race on `subscribe` between two callers), the
    // remove() is harmless.
    let mut map = registry.lock().await;
    if let Some(existing) = map.get(&run_id) {
        // Only remove if it's our own sender — same_channel ensures we
        // don't yank a fresher sender that races us on the lock.
        if existing.same_channel(&tx) {
            map.remove(&run_id);
        }
    }
    info!(run_id, "control bridge reader exited");
}
