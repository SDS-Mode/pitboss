//! End-to-end test for the cross-run aggregator: build a fixture
//! runs dir on disk, run the aggregator, assert digest shape.
//!
//! Pitboss-web is a bin-only crate, so we mount the `insights` module
//! via `#[path]` (same pattern as `control_bridge_integration.rs`).

use std::path::Path;

use chrono::{TimeZone, Utc};
use pitboss_core::parser::TokenUsage;
use pitboss_core::store::{FailureReason, RunSummary, TaskRecord, TaskStatus};
use uuid::Uuid;

// Mounting the module via `#[path]` brings in its entire item tree;
// this test only uses a small slice, so silence dead-code warnings
// rather than splitting the module just for the test crate.
#[allow(dead_code, unused_imports)]
#[path = "../src/insights/mod.rs"]
mod insights;

use insights::aggregator::{AggregateSet, Filter};
use insights::cluster::cluster_failures;

fn write_summary(runs_dir: &Path, name: Option<&str>, manifest_path: &str, tasks: Vec<TaskRecord>) {
    let run_id = Uuid::now_v7();
    let dir = runs_dir.join(run_id.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let total = tasks.len();
    let failed = tasks
        .iter()
        .filter(|t| !matches!(t.status, TaskStatus::Success))
        .count();
    let summary = RunSummary {
        run_id,
        manifest_path: manifest_path.into(),
        manifest_name: name.map(String::from),
        pitboss_version: "0.test".into(),
        claude_version: None,
        started_at: Utc.with_ymd_and_hms(2026, 4, 27, 10, 0, 0).unwrap(),
        ended_at: Utc.with_ymd_and_hms(2026, 4, 27, 10, 5, 0).unwrap(),
        total_duration_ms: 300_000,
        tasks_total: total,
        tasks_failed: failed,
        was_interrupted: false,
        tasks,
    };
    let bytes = serde_json::to_vec_pretty(&summary).unwrap();
    std::fs::write(dir.join("summary.json"), bytes).unwrap();
}

fn task(id: &str, status: TaskStatus, reason: Option<FailureReason>) -> TaskRecord {
    TaskRecord {
        task_id: id.into(),
        status,
        exit_code: Some(0),
        started_at: Utc.with_ymd_and_hms(2026, 4, 27, 10, 0, 0).unwrap(),
        ended_at: Utc.with_ymd_and_hms(2026, 4, 27, 10, 1, 0).unwrap(),
        duration_ms: 60_000,
        worktree_path: None,
        log_path: "/dev/null".into(),
        token_usage: TokenUsage::default(),
        claude_session_id: None,
        final_message_preview: None,
        final_message: None,
        parent_task_id: None,
        pause_count: 0,
        reprompt_count: 0,
        approvals_requested: 0,
        approvals_approved: 0,
        approvals_rejected: 0,
        model: None,
        failure_reason: reason,
    }
}

#[test]
fn empty_runs_dir_produces_empty_set() {
    let dir = tempfile::tempdir().unwrap();
    let set = AggregateSet::build(dir.path());
    assert!(set.runs.is_empty());
    assert!(set.failures.is_empty());
}

#[test]
fn explicit_name_used_when_present() {
    let dir = tempfile::tempdir().unwrap();
    write_summary(
        dir.path(),
        Some("nightly"),
        "/etc/pitboss/whatever.toml",
        vec![task("ok", TaskStatus::Success, None)],
    );
    let set = AggregateSet::build(dir.path());
    assert_eq!(set.runs.len(), 1);
    assert_eq!(set.runs[0].manifest_name, "nightly");
    assert_eq!(set.runs[0].outcome, "success");
    assert!(set.failures.is_empty());
}

#[test]
fn name_falls_back_to_manifest_filename() {
    let dir = tempfile::tempdir().unwrap();
    write_summary(
        dir.path(),
        None,
        "/abs/path/build-db.toml",
        vec![task("ok", TaskStatus::Success, None)],
    );
    let set = AggregateSet::build(dir.path());
    assert_eq!(set.runs[0].manifest_name, "build-db");
}

#[test]
fn same_shape_failures_collapse_to_one_cluster() {
    let dir = tempfile::tempdir().unwrap();
    for path in ["/tmp/a.sh", "/var/x.py", "/opt/y.sh"] {
        write_summary(
            dir.path(),
            Some("smoke"),
            "/manifest/smoke.toml",
            vec![task(
                "w",
                TaskStatus::Failed,
                Some(FailureReason::Unknown {
                    message: format!("exit 137 in {path}"),
                }),
            )],
        );
    }
    let set = AggregateSet::build(dir.path());
    assert_eq!(set.runs.len(), 3);
    assert_eq!(set.failures.len(), 3);
    let clusters = cluster_failures(&set.failures);
    assert_eq!(clusters.len(), 1);
    assert_eq!(clusters[0].count, 3);
    assert_eq!(
        clusters[0].template.as_deref(),
        Some("exit <NUM> in <PATH>")
    );
}

#[test]
fn rate_limit_and_auth_failure_never_co_cluster() {
    let dir = tempfile::tempdir().unwrap();
    write_summary(
        dir.path(),
        Some("smoke"),
        "/manifest/smoke.toml",
        vec![
            task(
                "a",
                TaskStatus::Failed,
                Some(FailureReason::RateLimit { resets_at: None }),
            ),
            task("b", TaskStatus::Failed, Some(FailureReason::AuthFailure)),
        ],
    );
    let set = AggregateSet::build(dir.path());
    let clusters = cluster_failures(&set.failures);
    assert_eq!(clusters.len(), 2, "Tier-1 isolation must hold");
}

#[test]
fn manifest_filter_drops_unrelated_runs() {
    let dir = tempfile::tempdir().unwrap();
    write_summary(
        dir.path(),
        Some("alpha"),
        "/m/alpha.toml",
        vec![task("ok", TaskStatus::Success, None)],
    );
    write_summary(
        dir.path(),
        Some("beta"),
        "/m/beta.toml",
        vec![task("ok", TaskStatus::Success, None)],
    );
    let set = AggregateSet::build(dir.path());
    assert_eq!(set.runs.len(), 2);
    let only_alpha = set.apply_filter(&Filter {
        manifest: Some("alpha".into()),
        ..Default::default()
    });
    assert_eq!(only_alpha.runs.len(), 1);
    assert_eq!(only_alpha.runs[0].manifest_name, "alpha");
}
