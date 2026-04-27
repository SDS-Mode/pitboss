//! Read-only `/api/insights/*` endpoints. All endpoints share the
//! aggregator cache held on [`AppState`]; per-request filters narrow a
//! cloned [`AggregateSet`].

use std::collections::BTreeMap;

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::insights::{
    aggregator::Filter, cluster_failures, Cluster, ManifestSummary, RunDigest, TaskFailureDigest,
};
use crate::state::AppState;

/// Common filter knobs accepted by every insights endpoint. Empty
/// strings are treated as None so the SPA can pass `?manifest=&kind=`
/// without manually pruning empties from its query string.
#[derive(Debug, Deserialize, Default)]
pub struct InsightsQuery {
    pub manifest: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub status: Option<String>,
    pub kind: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub min_count: Option<usize>,
}

impl InsightsQuery {
    fn to_filter(&self) -> Filter {
        Filter {
            manifest: nonempty(&self.manifest),
            since: self.since,
            until: self.until,
            status: nonempty(&self.status),
            kind: nonempty(&self.kind),
        }
    }
}

fn nonempty(s: &Option<String>) -> Option<String> {
    s.as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `GET /api/insights/runs` — paginated [`RunDigest`] list.
pub async fn runs(
    State(state): State<AppState>,
    Query(q): Query<InsightsQuery>,
) -> Json<RunsResponse> {
    let set = state.insights_cache().get(state.runs_dir());
    let filtered = set.apply_filter(&q.to_filter());
    let total = filtered.runs.len();
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(usize::MAX);
    let runs: Vec<RunDigest> = filtered.runs.into_iter().skip(offset).take(limit).collect();
    Json(RunsResponse { runs, total })
}

#[derive(Debug, Serialize)]
pub struct RunsResponse {
    pub runs: Vec<RunDigest>,
    pub total: usize,
}

/// `GET /api/insights/failures` — flat [`TaskFailureDigest`] list.
pub async fn failures(
    State(state): State<AppState>,
    Query(q): Query<InsightsQuery>,
) -> Json<FailuresResponse> {
    let set = state.insights_cache().get(state.runs_dir());
    let filtered = set.apply_filter(&q.to_filter());
    let total = filtered.failures.len();
    let mut sorted = filtered.failures;
    sorted.sort_by_key(|f| std::cmp::Reverse(f.occurred_at));
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(usize::MAX);
    let failures: Vec<TaskFailureDigest> = sorted.into_iter().skip(offset).take(limit).collect();
    Json(FailuresResponse { failures, total })
}

#[derive(Debug, Serialize)]
pub struct FailuresResponse {
    pub failures: Vec<TaskFailureDigest>,
    pub total: usize,
}

/// `GET /api/insights/clusters` — mined clusters from
/// [`crate::insights::cluster_failures`].
pub async fn clusters(
    State(state): State<AppState>,
    Query(q): Query<InsightsQuery>,
) -> Json<ClustersResponse> {
    let set = state.insights_cache().get(state.runs_dir());
    let filtered = set.apply_filter(&q.to_filter());
    let mut clusters = cluster_failures(&filtered.failures);
    if let Some(min) = q.min_count {
        clusters.retain(|c| c.count >= min);
    }
    let total = clusters.len();
    Json(ClustersResponse { clusters, total })
}

#[derive(Debug, Serialize)]
pub struct ClustersResponse {
    pub clusters: Vec<Cluster>,
    pub total: usize,
}

/// `GET /api/insights/manifests` — per-manifest aggregates.
pub async fn manifests(
    State(state): State<AppState>,
    Query(q): Query<InsightsQuery>,
) -> Json<ManifestsResponse> {
    let set = state.insights_cache().get(state.runs_dir());
    // For the manifest summary, ignore the `manifest=` filter (it would
    // collapse to a single row); honor `since` so operators can scope
    // health to a window.
    let filter = Filter {
        manifest: None,
        since: q.since,
        until: q.until,
        status: None,
        kind: None,
    };
    let filtered = set.apply_filter(&filter);
    let summaries = build_manifest_summaries(&filtered.runs);
    let total = summaries.len();
    Json(ManifestsResponse {
        manifests: summaries,
        total,
    })
}

#[derive(Debug, Serialize)]
pub struct ManifestsResponse {
    pub manifests: Vec<ManifestSummary>,
    pub total: usize,
}

fn build_manifest_summaries(runs: &[RunDigest]) -> Vec<ManifestSummary> {
    let mut by_name: BTreeMap<&str, Acc> = BTreeMap::new();
    for r in runs {
        let entry = by_name.entry(r.manifest_name.as_str()).or_default();
        entry.runs_total += 1;
        if r.tasks_failed > 0 {
            entry.runs_failed += 1;
        }
        if let Some(d) = r.duration_ms {
            entry.duration_sum += d;
            entry.duration_count += 1;
        }
        if let Some(t) = r.started_at {
            entry.last_run_at = entry.last_run_at.max(Some(t));
        }
        for k in &r.failure_kinds {
            if !entry.failure_kinds.iter().any(|kk| kk == k) {
                entry.failure_kinds.push(k.clone());
            }
        }
    }
    let mut out: Vec<ManifestSummary> = by_name
        .into_iter()
        .map(|(name, a)| ManifestSummary {
            manifest_name: name.to_string(),
            runs_total: a.runs_total,
            runs_failed: a.runs_failed,
            success_rate: if a.runs_total == 0 {
                0.0
            } else {
                (a.runs_total - a.runs_failed) as f64 / a.runs_total as f64
            },
            last_run_at: a.last_run_at,
            avg_duration_ms: if a.duration_count == 0 {
                None
            } else {
                Some(a.duration_sum / a.duration_count as i64)
            },
            failure_kinds: a.failure_kinds,
        })
        .collect();
    out.sort_by_key(|m| std::cmp::Reverse(m.last_run_at));
    out
}

#[derive(Default)]
struct Acc {
    runs_total: usize,
    runs_failed: usize,
    duration_sum: i64,
    duration_count: usize,
    last_run_at: Option<i64>,
    failure_kinds: Vec<String>,
}
