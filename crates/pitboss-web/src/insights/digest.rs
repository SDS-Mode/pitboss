//! Per-run + per-failure digest types served by the insights endpoints.
//! These are flat, JSON-friendly shapes derived from the on-disk
//! `summary.json` / `summary.jsonl` / `meta.json` artefacts. Everything
//! the frontend needs for cross-run charts and tables ships in one of
//! these structs.

use serde::Serialize;

/// One row in the cross-run table. Built per `summary.json` (or partial
/// `summary.jsonl` for in-progress runs).
#[derive(Debug, Clone, Serialize)]
pub struct RunDigest {
    pub run_id: String,
    /// Resolved via the identity cascade in
    /// [`crate::insights::aggregator::resolve_manifest_name`]. Always
    /// populated (falls back to `"<unnamed>"`).
    pub manifest_name: String,
    /// Original manifest path when known, useful for display tooltips.
    pub manifest_path: Option<String>,
    /// `complete` / `running` / `stale` / `aborted` — re-uses the
    /// pitboss-cli classifier so we never have a second taxonomy.
    pub status: String,
    /// `success` / `failed` / `partial` — collapsed business outcome.
    /// Independent of `status`, which is about dispatcher liveness.
    pub outcome: String,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub tasks_total: usize,
    pub tasks_failed: usize,
    /// Distinct `FailureReason::kind` values that appeared in this run.
    /// Empty when the run had no failures.
    pub failure_kinds: Vec<String>,
}

/// One row per failed task. Derived per [`pitboss_core::store::TaskRecord`]
/// with `failure_reason: Some(_)`.
#[derive(Debug, Clone, Serialize)]
pub struct TaskFailureDigest {
    pub run_id: String,
    pub manifest_name: String,
    pub task_id: String,
    pub parent_task_id: Option<String>,
    /// `serde_tag` from `FailureReason` (e.g. `rate_limit`, `worker_crash`).
    /// First-tier cluster key.
    pub failure_kind: String,
    /// Free-text part of the failure (e.g. `WorkerCrash::message`).
    /// `None` for structured-only variants like `RateLimit { resets_at }`.
    pub error_message: Option<String>,
    /// Canonical template from
    /// [`crate::insights::tokenizer::canonicalize`]. `None` when there's
    /// no `error_message` to canonicalize.
    pub error_template: Option<String>,
    pub model: Option<String>,
    pub duration_ms: Option<i64>,
    pub occurred_at: Option<i64>,
}

/// One mined cluster. Built by
/// [`crate::insights::cluster::cluster_failures`].
#[derive(Debug, Clone, Serialize)]
pub struct Cluster {
    /// `(failure_kind, template_or_kind_only)` is the cluster key.
    /// `template` is `None` for structured-only failure kinds where
    /// the kind alone is the canonical grouping.
    pub kind: String,
    pub template: Option<String>,
    pub count: usize,
    pub first_seen: Option<i64>,
    pub last_seen: Option<i64>,
    /// Distinct manifest names hit by this cluster.
    pub manifests: Vec<String>,
    /// Distinct task ids hit by this cluster.
    pub task_ids: Vec<String>,
    /// Distinct run ids in which this cluster appeared.
    pub run_ids: Vec<String>,
    /// One representative untruncated failure message — the oldest
    /// occurrence — so the operator can see what a real instance
    /// looked like before masking.
    pub exemplar_message: Option<String>,
}

/// Per-manifest aggregate. One row per distinct `manifest_name`.
#[derive(Debug, Clone, Serialize)]
pub struct ManifestSummary {
    pub manifest_name: String,
    pub runs_total: usize,
    pub runs_failed: usize,
    pub success_rate: f64,
    pub last_run_at: Option<i64>,
    pub avg_duration_ms: Option<i64>,
    /// Distinct failure kinds seen across this manifest's runs.
    pub failure_kinds: Vec<String>,
}
