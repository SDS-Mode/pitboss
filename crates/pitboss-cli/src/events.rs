//! `pitboss events <run-id>` — print the persisted control-event
//! stream for a prior run (#259).
//!
//! Reads `<run-dir>/events.jsonl`, which the dispatcher writes when
//! the manifest sets `[run].emit_event_stream = true`. Defaults to a
//! compact one-line-per-envelope render so an operator can scan a
//! sub-lead lifecycle / worker-failure / approval trail without
//! parsing JSON. `--json` passes the file through verbatim (NDJSON)
//! for piping into `jq`.
//!
//! Resolution: the `<run-id>` argument accepts the full UUID or any
//! unique prefix (same convention as `pitboss status`).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::control::protocol::{ControlEvent, EventEnvelope};

/// Entry point for the `events` subcommand.
pub fn run(run_id_prefix: &str, json: bool, run_dir_override: Option<PathBuf>) -> Result<i32> {
    let base = run_dir_override.unwrap_or_else(default_runs_dir);
    let run_dir = crate::runs::resolve_run_dir_by_prefix(&base, run_id_prefix)?;
    let events_path = run_dir.join("events.jsonl");

    let file = match std::fs::File::open(&events_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "no events.jsonl in {}; either the run has not emitted any \
                 envelopes yet or `[run].emit_event_stream` was not set on the \
                 manifest. Re-dispatch with the flag enabled to capture the \
                 control-event stream.",
                run_dir.display(),
            );
        }
        Err(e) => return Err(e.into()),
    };

    let reader = BufReader::new(file);
    let mut stdout = std::io::stdout().lock();

    if json {
        // Passthrough mode: copy the file verbatim. We re-stream
        // through `BufReader::lines` rather than `std::io::copy` so
        // an incomplete trailing line (a partial write caught mid-
        // flush) gets dropped, matching the canonical reader's
        // semantics elsewhere in the codebase.
        for line in reader.lines() {
            let line = line?;
            stdout.write_all(line.as_bytes())?;
            stdout.write_all(b"\n")?;
        }
        return Ok(0);
    }

    let mut parsed = 0usize;
    let mut skipped = 0usize;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<EventEnvelope>(&line) {
            Ok(envelope) => {
                writeln!(stdout, "{}", format_envelope(&envelope))?;
                parsed += 1;
            }
            Err(_) => skipped += 1,
        }
    }

    if skipped > 0 {
        // Don't fail — operators piping into `wc` or grep don't want
        // an exit code change for a partial-write tail. Log to stderr
        // so the diagnostic is still visible.
        eprintln!(
            "pitboss events: skipped {skipped} malformed line(s); rendered {parsed} envelope(s)",
        );
    }
    Ok(0)
}

/// Render one envelope as a single fixed-width line. Format:
///
/// ```text
/// seq=NNNN  <actor_path>  <event_kind>  <detail>
/// ```
///
/// Widths are chosen so a typical hierarchical run with a small
/// number of sub-leads aligns cleanly without forcing word-wrap on
/// an 80-column terminal. Long detail strings (e.g. approval
/// summaries) extend past the right edge — same trade-off `pitboss
/// status` makes for `final_message_preview`.
fn format_envelope(envelope: &EventEnvelope) -> String {
    let actor_path = if envelope.actor_path.is_empty() {
        "/".to_string()
    } else {
        // ActorPath has a `Display` impl that uses `→` as the
        // separator (matches TUI rendering). For a CLI render we
        // prefer Unix-path-style separators so the output looks
        // grep-friendly. Walk the inner Vec directly.
        format!("/{}", envelope.actor_path.0.join("/"))
    };
    let (kind, detail) = describe_event(&envelope.event);
    format!(
        "seq={:>4}  {:<24}  {:<20}  {}",
        envelope.seq, actor_path, kind, detail,
    )
}

/// Per-variant one-liner. Keep this in lockstep with the
/// `#[serde(tag = "event", rename_all = "snake_case")]` discriminator
/// values on `ControlEvent` so the rendered `kind` column matches
/// what an operator sees in the raw JSON.
fn describe_event(event: &ControlEvent) -> (&'static str, String) {
    match event {
        ControlEvent::Hello { .. } => ("hello", String::new()),
        ControlEvent::OpAcked { op, task_id } => (
            "op_acked",
            match task_id {
                Some(t) => format!("op={op} task_id={t}"),
                None => format!("op={op}"),
            },
        ),
        ControlEvent::OpFailed { op, task_id, error } => (
            "op_failed",
            match task_id {
                Some(t) => format!("op={op} task_id={t} error={error}"),
                None => format!("op={op} error={error}"),
            },
        ),
        ControlEvent::OpUnknown { op } => ("op_unknown", format!("op={op}")),
        ControlEvent::OpUnknownState {
            op,
            task_id,
            current_state,
        } => (
            "op_unknown_state",
            format!("op={op} task_id={task_id} state={current_state}"),
        ),
        ControlEvent::ApprovalRequest {
            request_id,
            task_id,
            summary,
            kind,
            ..
        } => (
            "approval_request",
            format!(
                "request_id={request_id} task_id={task_id} kind={kind:?} summary={:?}",
                summary,
            ),
        ),
        ControlEvent::WorkersSnapshot { workers } => (
            "workers_snapshot",
            format!("worker_count={}", workers.len()),
        ),
        ControlEvent::Superseded => ("superseded", String::new()),
        ControlEvent::RunFinished { summary } => (
            "run_finished",
            format!(
                "tasks_total={} tasks_failed={}",
                summary.tasks_total, summary.tasks_failed,
            ),
        ),
        ControlEvent::StoreActivity { counters } => {
            ("store_activity", format!("actor_count={}", counters.len()))
        }
        ControlEvent::SubleadSpawned {
            sublead_id,
            budget_usd,
            max_workers,
            read_down,
            ..
        } => (
            "sublead_spawned",
            format!(
                "sublead_id={sublead_id} budget_usd={} max_workers={} read_down={read_down}",
                budget_usd
                    .map(|v| format!("{v:.2}"))
                    .unwrap_or_else(|| "shared".into()),
                max_workers
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "shared".into()),
            ),
        ),
        ControlEvent::WorkerFailed {
            task_id,
            parent_task_id,
            reason,
        } => (
            "worker_failed",
            match parent_task_id {
                Some(p) => format!("task_id={task_id} parent={p} reason={reason:?}"),
                None => format!("task_id={task_id} reason={reason:?}"),
            },
        ),
        ControlEvent::SubleadTerminated {
            sublead_id,
            spent_usd,
            unspent_usd,
            ..
        } => (
            "sublead_terminated",
            format!(
                "sublead_id={sublead_id} spent_usd={spent_usd:.4} unspent_usd={unspent_usd:.4}"
            ),
        ),
    }
}

fn default_runs_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".local/share/pitboss/runs")
}

/// Test helper exposed so `events::run`-equivalent integration tests
/// don't have to re-implement the resolve+open+render pipeline. Not
/// used by `main.rs`.
#[doc(hidden)]
pub fn render_to_string(events_path: &Path, json: bool) -> Result<String> {
    let file = std::fs::File::open(events_path)?;
    let reader = BufReader::new(file);
    let mut out = String::new();
    if json {
        for line in reader.lines() {
            let line = line?;
            out.push_str(&line);
            out.push('\n');
        }
        return Ok(out);
    }
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<EventEnvelope>(&line) {
            Ok(envelope) => {
                out.push_str(&format_envelope(&envelope));
                out.push('\n');
            }
            Err(_) => continue,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::protocol::{ApprovalKind, ControlEvent};
    use crate::dispatch::actor::ActorPath;
    use pitboss_core::store::FailureReason;
    use tempfile::TempDir;

    fn write_events(dir: &Path, envelopes: &[EventEnvelope]) -> PathBuf {
        let path = dir.join("events.jsonl");
        let mut content = String::new();
        for env in envelopes {
            content.push_str(&serde_json::to_string(env).unwrap());
            content.push('\n');
        }
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn render_formats_envelopes_into_columns() {
        let tmp = TempDir::new().unwrap();
        let path = write_events(
            tmp.path(),
            &[
                EventEnvelope {
                    actor_path: ActorPath::default(),
                    seq: 1,
                    event: ControlEvent::Superseded,
                },
                EventEnvelope {
                    actor_path: ActorPath::default(),
                    seq: 2,
                    event: ControlEvent::SubleadSpawned {
                        sublead_id: "sub-1".into(),
                        budget_usd: Some(0.5),
                        lead_budget_usd: None,
                        max_workers: Some(2),
                        read_down: false,
                    },
                },
                EventEnvelope {
                    actor_path: ActorPath::new(["root", "sub-1"]),
                    seq: 3,
                    event: ControlEvent::WorkerFailed {
                        task_id: "w-1".into(),
                        parent_task_id: Some("sub-1".into()),
                        reason: FailureReason::AuthFailure,
                    },
                },
            ],
        );

        let out = render_to_string(&path, false).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "rendered: {out}");
        assert!(lines[0].contains("seq=   1"), "first: {}", lines[0]);
        assert!(lines[0].contains("superseded"));
        assert!(lines[1].contains("sublead_spawned"));
        assert!(lines[1].contains("sublead_id=sub-1"));
        assert!(lines[1].contains("budget_usd=0.50"));
        assert!(lines[2].contains("worker_failed"));
        assert!(lines[2].contains("/root/sub-1"));
        assert!(lines[2].contains("AuthFailure"));
    }

    #[test]
    fn json_mode_passes_lines_through_verbatim() {
        let tmp = TempDir::new().unwrap();
        let env = EventEnvelope {
            actor_path: ActorPath::default(),
            seq: 7,
            event: ControlEvent::ApprovalRequest {
                request_id: "r-1".into(),
                task_id: "t-1".into(),
                summary: "run cargo test".into(),
                plan: None,
                kind: ApprovalKind::Action,
            },
        };
        let raw = serde_json::to_string(&env).unwrap();
        let path = tmp.path().join("events.jsonl");
        std::fs::write(&path, format!("{raw}\n")).unwrap();

        let out = render_to_string(&path, true).unwrap();
        assert_eq!(out, format!("{raw}\n"));
    }

    #[test]
    fn render_skips_malformed_lines() {
        let tmp = TempDir::new().unwrap();
        let valid = serde_json::to_string(&EventEnvelope {
            actor_path: ActorPath::default(),
            seq: 1,
            event: ControlEvent::Superseded,
        })
        .unwrap();
        let path = tmp.path().join("events.jsonl");
        // Truncated partial write at the head, valid line at the tail.
        std::fs::write(&path, format!("{{\"truncated\":\n{valid}\n")).unwrap();

        let out = render_to_string(&path, false).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("superseded"));
    }

    #[test]
    fn missing_file_gives_operator_friendly_error() {
        let tmp = TempDir::new().unwrap();
        let run_id = uuid::Uuid::now_v7().to_string();
        std::fs::create_dir_all(tmp.path().join(&run_id)).unwrap();
        let err = run(&run_id, false, Some(tmp.path().to_path_buf())).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("emit_event_stream"),
            "error should hint at the manifest flag: {msg}",
        );
    }
}
