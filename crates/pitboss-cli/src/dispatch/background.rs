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

    // Pre-flight: validate the manifest BEFORE forking. A TOML/parse
    // error surfaces here on the parent's stderr where the operator can
    // see it, instead of dying silently inside a detached child whose
    // stderr was previously nulled (#150 M8). The dispatcher will
    // re-validate inside the child too — this is a fail-fast convenience.
    crate::manifest::load::load_manifest(manifest_path, None).with_context(|| {
        format!(
            "validate manifest before background spawn: {}",
            manifest_path.display()
        )
    })?;

    // Canonicalize paths so the JSON announcement and the child both see
    // absolute paths regardless of the parent's cwd. Without this, an
    // orchestrator that captures the announcement and later cd's
    // somewhere else cannot resolve the manifest path. (#150 M2)
    let manifest_path_canonical = std::fs::canonicalize(manifest_path)
        .with_context(|| format!("canonicalize manifest path: {}", manifest_path.display()))?;
    let run_dir_canonical = run_dir_override.as_ref().map(|d| {
        // The runs dir might not exist yet; fall back to the original
        // path if canonicalize fails — the dispatcher creates it.
        std::fs::canonicalize(d).unwrap_or_else(|_| d.clone())
    });

    let exe = std::env::current_exe().context("locate current pitboss binary")?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("dispatch")
        .arg(&manifest_path_canonical)
        .args(["--internal-run-id", &run_id_str]);
    if let Some(ref dir) = run_dir_canonical {
        cmd.arg("--run-dir").arg(dir);
    }

    // Capture child stderr to a per-run log file so dispatch failures
    // produce *some* operator-visible signal — pre-fix, child stderr was
    // nulled and a panic in startup left zero diagnostic. (#150 M3)
    //
    // Logged at `<runs_base>/<run_id>.bg-stderr.log` because the per-run
    // dir doesn't exist yet (the child mints it). This is a sibling
    // location operators can find without knowing the run id ahead of
    // time.
    let stderr_log_path = stderr_log_path_for(run_dir_canonical.as_deref(), &run_id_str);
    let stderr_target = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stderr_log_path)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr_target);

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
        "manifest_path": manifest_path_canonical.to_string_lossy(),
        "started_at": started_at.to_rfc3339(),
        "child_pid": child_pid,
        "bg_stderr_log": stderr_log_path.to_string_lossy(),
    });
    // Use `writeln!` on an explicit stdout handle instead of `println!`:
    // an orchestrator that closes its end of the pipe before reading the
    // announcement would otherwise panic the parent with EPIPE — turning
    // a "child is already running" outcome into a non-zero exit. Swallow
    // EPIPE quietly; the run id is recoverable via `pitboss list
    // --active`. (#150 L13)
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    if let Err(e) = writeln!(out, "{payload}") {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(e).context("write background-dispatch announcement");
        }
        tracing::debug!(error = %e, "background announcement: stdout EPIPE; child still running");
    }
    let _ = out.flush();

    Ok(0)
}

fn stderr_log_path_for(run_dir_override: Option<&Path>, run_id: &str) -> PathBuf {
    let base = run_dir_override
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::runs::runs_base_dir);
    let _ = std::fs::create_dir_all(&base);
    base.join(format!("{run_id}.bg-stderr.log"))
}

/// Parse a `--internal-run-id` argument into a `Uuid`. Used by `main.rs`
/// when forwarding the value into [`crate::dispatch::run_dispatch_inner`]
/// or [`crate::dispatch::hierarchical::run_hierarchical`].
///
/// Requires UUID v7 specifically — pitboss's run-id discriminator
/// includes a millisecond-resolution timestamp the dispatcher (and the
/// run-discovery sort) extract from the leading bytes of v7. Accepting
/// older UUID versions silently breaks downstream consumers that
/// assume the v7 layout. (#150 L14)
pub fn parse_internal_run_id(s: &str) -> Result<Uuid> {
    let parsed =
        Uuid::parse_str(s).with_context(|| format!("--internal-run-id: invalid UUID: {s}"))?;
    if parsed.get_version_num() != 7 {
        anyhow::bail!(
            "--internal-run-id must be a UUID v7 (got v{}); pitboss \
             extracts a timestamp discriminator from the leading bytes \
             of v7 ids and other versions silently break downstream \
             consumers",
            parsed.get_version_num()
        );
    }
    Ok(parsed)
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

    /// #150 L14 regression: non-v7 UUIDs are rejected because pitboss
    /// extracts a timestamp from v7's leading bytes. Use a v4 string
    /// literal — the `v4` cargo feature isn't enabled in pitboss-cli
    /// so we can't `Uuid::new_v4()`, but parsing a v4 string and
    /// asserting it's not v7 gives the same coverage.
    #[test]
    fn parse_internal_run_id_rejects_non_v7_versions() {
        // RFC 4122 example v4 UUID; version nibble is the leading 4 of
        // the third group.
        let v4_str = "550e8400-e29b-41d4-a716-446655440000";
        let parsed = Uuid::parse_str(v4_str).unwrap();
        assert_eq!(parsed.get_version_num(), 4, "test fixture must be v4");
        let err = parse_internal_run_id(v4_str).unwrap_err();
        assert!(err.to_string().contains("UUID v7"));
    }

    /// #150 M3 regression: the stderr-log path lands under the runs
    /// override dir (when supplied) so an orchestrator can find it
    /// without needing the per-run dispatch dir to exist yet.
    #[test]
    fn stderr_log_path_uses_run_dir_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = stderr_log_path_for(Some(tmp.path()), "abc");
        assert!(p.starts_with(tmp.path()));
        assert!(p.to_string_lossy().ends_with("abc.bg-stderr.log"));
    }
}
