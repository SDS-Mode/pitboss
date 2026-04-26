//! `pitboss dispatch --background` — detached, non-blocking dispatch entry.
//!
//! ## Why this module exists
//!
//! Orchestrators that wrap pitboss (Discord bots, web dashboards, CI scripts)
//! need to dispatch a manifest and return to availability immediately rather
//! than block on completion. Issue #133-C frames the use case: a Discord bot
//! that calls `pitboss dispatch <manifest>` from inside a slash-command
//! handler ties up its event loop for the entire run; with `--background`
//! the bot fires the dispatch, gets a `run_id` back, and the run grinds in
//! the background while the bot stays interactive.
//!
//! ## Mechanism
//!
//! The flag is mode-agnostic: it works for both flat-mode (`[[task]]`) and
//! hierarchical (`[lead]`) manifests. The decision of whether a lead claude
//! runs is a manifest authoring concern, kept orthogonal to the
//! attached-vs-detached lifecycle concern this flag controls.
//!
//! Implementation:
//! 1. Parent pre-mints the `run_id` (UUID v7, same scheme the dispatcher
//!    uses internally).
//! 2. Parent re-spawns itself with the standard `dispatch` subcommand plus
//!    the hidden `--internal-run-id <uuid>` flag, which tells the child
//!    dispatcher to honor the pre-minted id instead of generating its own
//!    — so the `run_id` the parent prints matches what lands in
//!    `summary.json` on completion.
//! 3. Child stdio is nulled and `setsid()` is called via `pre_exec` so the
//!    child becomes its own session leader, fully detached from the parent's
//!    controlling terminal. Ctrl-C in the parent's TTY won't reach the
//!    child.
//! 4. Parent prints `{run_id, manifest_path, started_at, child_pid}` JSON
//!    to stdout and exits 0. The child runs the standard dispatch path.
//!
//! When the parent exits, the child is reparented to PID 1 (init/systemd),
//! which auto-reaps it on completion. No explicit double-fork is needed
//! on Linux.
//!
//! ## Composes with PR #137 lifecycle
//!
//! Background dispatch pairs naturally with `[lifecycle].notify` and
//! `[lifecycle].survive_parent`: orchestrators that detach via
//! `--background` and want completion notifications declare a webhook in
//! the manifest, then `RunFinished` lands at the orchestrator's URL when
//! the detached run finishes. Run-id correlation is automatic — the
//! orchestrator stored the `run_id` it got back from the parent, and the
//! webhook payload carries the same id.
//!
//! ## Why not double-fork
//!
//! Classical Unix daemonization uses double-fork to prevent the daemon from
//! re-acquiring a controlling terminal. We don't need that guarantee here:
//! the child is a `pitboss dispatch` process that never `open()`s a tty
//! device. `setsid()` alone (which puts the child in a new session with no
//! controlling tty) is sufficient and keeps the spawn logic
//! straightforward.

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::json;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use uuid::Uuid;

/// Spawn a detached dispatch child and return immediately. Prints a JSON
/// line `{run_id, manifest_path, started_at, child_pid}` to stdout on
/// success.
///
/// Returns the parent's exit code (0 on successful spawn, non-zero on
/// spawn failure). The child's success/failure is observed out-of-band
/// via `pitboss list --active`, `pitboss status <run_id>`, or the
/// manifest's `[lifecycle].notify` webhook.
pub fn run_background(manifest_path: &Path, run_dir_override: Option<PathBuf>) -> Result<i32> {
    let run_id = Uuid::now_v7();
    let run_id_str = run_id.to_string();
    let started_at = Utc::now();

    let exe = std::env::current_exe().context("locate current pitboss binary")?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("dispatch")
        .arg(manifest_path)
        .args(["--internal-run-id", &run_id_str]);
    if let Some(ref dir) = run_dir_override {
        cmd.arg("--run-dir").arg(dir);
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: `pre_exec` runs in the forked child between fork() and
    // exec(). `setsid()` is async-signal-safe per POSIX, which is the only
    // restriction in this hook (no allocations, no locks, no Rust-stdlib
    // calls that touch shared state).
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("spawn detached dispatcher (binary: {})", exe.display()))?;
    let child_pid = child.id();
    // Drop the Child handle without waiting. On parent exit the child is
    // reparented to PID 1 which auto-reaps it. We keep no reference so
    // the parent can exit immediately after printing the announcement.
    drop(child);

    let payload = json!({
        "run_id": run_id_str,
        "manifest_path": manifest_path.to_string_lossy(),
        "started_at": started_at.to_rfc3339(),
        "child_pid": child_pid,
    });
    println!("{payload}");

    Ok(0)
}

/// Parse a `--internal-run-id` argument into a `Uuid`. Used by `main.rs`
/// when forwarding the value into [`crate::dispatch::run_dispatch_inner`]
/// or [`crate::dispatch::hierarchical::run_hierarchical`].
pub fn parse_internal_run_id(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).with_context(|| format!("--internal-run-id: invalid UUID: {s}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_internal_run_id_accepts_uuid_v7() {
        let v7 = Uuid::now_v7();
        let parsed = parse_internal_run_id(&v7.to_string()).expect("valid UUID parses");
        assert_eq!(parsed, v7);
    }

    #[test]
    fn parse_internal_run_id_rejects_garbage() {
        assert!(parse_internal_run_id("not-a-uuid").is_err());
        assert!(parse_internal_run_id("").is_err());
    }
}
