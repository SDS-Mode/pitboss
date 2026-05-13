//! Per-run event-stream persistence + seq counter.
//!
//! Implements the writer side of [#259](https://github.com/SDS-Mode/pitboss/issues/259),
//! co-spec'd with [#438](https://github.com/SDS-Mode/pitboss/issues/438)'s
//! unified envelope API (RFC at `book/src/architecture/unified-envelope-api.md`).
//!
//! ## What this owns
//!
//! - A **run-scoped** monotonic seq counter. PR-B of #438 ran a
//!   per-connection counter in [`crate::control::server`] as a stopgap;
//!   this module promotes it to per-run lifetime so seqs persisted in
//!   `events.jsonl` match seqs on the live wire and survive TUI
//!   reconnects.
//! - An append-only `<run-dir>/events.jsonl` file, lazily created on
//!   the first non-`Hello` envelope written when
//!   `[run].emit_event_stream = true`. Absent when the flag is off
//!   (today's back-compat default).
//!
//! ## How persistence runs
//!
//! PR-H of #259 separates the *emission* of an envelope from its
//! *persistence*. Emitters call
//! [`crate::dispatch::layer::LayerState::broadcast_control_event`]
//! which publishes onto the per-run broadcast bus
//! ([`LayerState::events_tx`]). The bus has two subscribers:
//!
//! 1. A persistent task spawned in
//!    [`crate::dispatch::state::DispatchState::new`] that drains the
//!    bus and calls [`EventLog::persist`] for every envelope. This
//!    task is alive for the whole run, so **headless** dispatches
//!    (no TUI, no web bridge) still produce a complete
//!    `events.jsonl`.
//! 2. A per-connection bridge spawned in `control/server.rs` that
//!    forwards the bus into the connected client's outbound mpsc.
//!
//! ## What this does not own
//!
//! - The decision of which events to emit; that stays in the
//!   dispatcher's existing emit sites.
//! - The HTTP endpoint, SPA replay, or `pitboss events` CLI
//!   subcommand from #259's full scope — those are follow-up PRs.
//! - Persisting connection-scoped envelopes (Hello reply, op replies,
//!   queued-approval drain, bridge replay, Superseded sent to a
//!   displaced client, `StoreActivity` ticks). These bypass the bus
//!   on purpose — they're addressed to a specific client, not to
//!   the run — and persist via the inline helpers in
//!   `control/server.rs` whenever they fire. `Hello` itself is
//!   filtered at the [`EventLog::persist`] boundary because it is
//!   pure handshake noise.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::control::protocol::{ControlEvent, EventEnvelope};

/// Run-scoped event-stream log. Cheap to clone via `Arc`.
#[derive(Debug)]
pub struct EventLog {
    /// Monotonic seq, fetched + incremented atomically. Starts at 0;
    /// the first emitted envelope's seq is 1 (post-increment).
    seq: AtomicU64,
    /// Where `events.jsonl` would live. Constructed unconditionally so
    /// tests can introspect; the file at this path is only created
    /// when `enabled` is true and a non-`Hello` envelope is persisted.
    path: PathBuf,
    /// `[run].emit_event_stream` from the resolved manifest. When
    /// false, [`Self::persist`] is a no-op and the file is never
    /// created.
    enabled: bool,
    /// Lazily-opened file handle. `None` until the first
    /// non-`Hello` envelope arrives with `enabled = true`. Held
    /// behind a [`Mutex`] so concurrent writers serialize their
    /// `write_all` calls — `O_APPEND` makes the underlying syscall
    /// atomic for small payloads on POSIX, but per-line buffering
    /// needs the mutex to keep lines whole.
    file: Mutex<Option<File>>,
}

impl EventLog {
    /// Build a new log rooted at `<run_dir>/events.jsonl`. `enabled`
    /// is the resolved `[run].emit_event_stream` flag.
    #[must_use]
    pub fn new(run_dir: &Path, enabled: bool) -> Self {
        Self {
            seq: AtomicU64::new(0),
            path: run_dir.join("events.jsonl"),
            enabled,
            file: Mutex::new(None),
        }
    }

    /// Returns the next seq to assign to an outgoing envelope.
    /// Monotonic per [`EventLog`] instance (i.e. per run); seq=1 is
    /// the first envelope. Reserved sentinel `seq=0` means
    /// "legacy / unknown" — pre-#438 wire format.
    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Whether persistence is on. Wire emitters use this to short-
    /// circuit the envelope clone they'd otherwise pass to [`Self::persist`].
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Path the log will be (or already is) appended to. Useful for
    /// the `pitboss events` CLI and tests.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one envelope to `events.jsonl`. No-op when persistence
    /// is disabled or the envelope is a `Hello` handshake (no
    /// archival value per #259's spec).
    ///
    /// Errors writing are logged but not returned to the caller —
    /// persistence is best-effort, matching the existing
    /// "control socket is best-effort" semantics for the live
    /// broadcast path. Callers proceed regardless.
    pub async fn persist(&self, envelope: &EventEnvelope) {
        if !self.enabled {
            return;
        }
        if matches!(envelope.event, ControlEvent::Hello { .. }) {
            return;
        }

        let mut guard = self.file.lock().await;
        if guard.is_none() {
            *guard = match Self::open_for_append(&self.path).await {
                Ok(f) => Some(f),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %self.path.display(),
                        "events.jsonl: failed to open; persistence disabled for this run",
                    );
                    return;
                }
            };
        }

        let Some(file) = guard.as_mut() else { return };
        let line = match serde_json::to_string(envelope) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "events.jsonl: failed to serialize envelope");
                return;
            }
        };
        if let Err(e) = file.write_all(line.as_bytes()).await {
            tracing::warn!(error = %e, "events.jsonl: write_all failed");
            return;
        }
        if let Err(e) = file.write_all(b"\n").await {
            tracing::warn!(error = %e, "events.jsonl: trailing newline write failed");
            return;
        }
        if let Err(e) = file.flush().await {
            tracing::warn!(error = %e, "events.jsonl: flush failed");
        }
    }

    async fn open_for_append(path: &Path) -> std::io::Result<File> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::protocol::{ControlEvent, EventEnvelope};
    use crate::dispatch::actor::ActorPath;
    use tempfile::TempDir;

    fn env(seq: u64, event: ControlEvent) -> EventEnvelope {
        EventEnvelope {
            actor_path: ActorPath::default(),
            seq,
            event,
        }
    }

    #[test]
    fn seq_is_monotonic_starting_at_one() {
        let tmp = TempDir::new().unwrap();
        let log = EventLog::new(tmp.path(), true);
        assert_eq!(log.next_seq(), 1);
        assert_eq!(log.next_seq(), 2);
        assert_eq!(log.next_seq(), 3);
    }

    #[tokio::test]
    async fn persist_writes_one_line_per_envelope() {
        let tmp = TempDir::new().unwrap();
        let log = EventLog::new(tmp.path(), true);
        log.persist(&env(1, ControlEvent::Superseded)).await;
        log.persist(&env(2, ControlEvent::OpUnknown { op: "x".into() }))
            .await;

        let contents = tokio::fs::read_to_string(log.path()).await.unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"event\":\"superseded\""));
        assert!(lines[0].contains("\"seq\":1"));
        assert!(lines[1].contains("\"op_unknown\""));
        assert!(lines[1].contains("\"seq\":2"));
    }

    #[tokio::test]
    async fn persist_skips_hello_envelopes() {
        let tmp = TempDir::new().unwrap();
        let log = EventLog::new(tmp.path(), true);
        log.persist(&env(
            1,
            ControlEvent::Hello {
                server_version: "0.13.0".into(),
                run_id: "r".into(),
                run_kind: "flat".into(),
                workers: vec![],
                policy_rules: vec![],
            },
        ))
        .await;
        log.persist(&env(2, ControlEvent::Superseded)).await;

        let contents = tokio::fs::read_to_string(log.path()).await.unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1, "Hello must not be archived");
        assert!(lines[0].contains("\"event\":\"superseded\""));
    }

    #[tokio::test]
    async fn persist_noops_when_disabled() {
        let tmp = TempDir::new().unwrap();
        let log = EventLog::new(tmp.path(), false);
        log.persist(&env(1, ControlEvent::Superseded)).await;
        assert!(
            !log.path().exists(),
            "file must not be created when emit_event_stream is off",
        );
    }

    /// Pre-existing `events.jsonl` (resume scenario) must be appended
    /// to, not truncated.
    #[tokio::test]
    async fn persist_appends_to_existing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("events.jsonl");
        tokio::fs::write(&path, "{\"prior\":\"row\"}\n")
            .await
            .unwrap();

        let log = EventLog::new(tmp.path(), true);
        log.persist(&env(7, ControlEvent::Superseded)).await;

        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"prior\":\"row\""));
        assert!(lines[1].contains("\"seq\":7"));
    }
}
