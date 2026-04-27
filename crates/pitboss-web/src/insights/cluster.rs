//! Drain-lite cluster mining over a flat `TaskFailureDigest` list.
//! Two-tier clustering:
//!
//! - **Tier 1**: structured `failure_kind` (free, comes from the
//!   dispatcher's typed `FailureReason` enum).
//! - **Tier 2**: `error_template` (canonicalised `error_message`) within
//!   each kind, when an error_message is present.
//!
//! Cluster key is `(failure_kind, error_template_or_kind_only)`.
//! Equality on canonical template — no similarity threshold.

use std::collections::BTreeMap;

use super::digest::{Cluster, TaskFailureDigest};

/// Group failures into clusters and return them sorted by count desc,
/// then by `last_seen` desc (most-recent tiebreaker).
pub fn cluster_failures(failures: &[TaskFailureDigest]) -> Vec<Cluster> {
    // BTreeMap so iteration order is deterministic (the final sort is
    // by count, but stable iteration helps tests + golden files).
    let mut buckets: BTreeMap<(String, Option<String>), Bucket> = BTreeMap::new();

    for f in failures {
        let key = (f.failure_kind.clone(), f.error_template.clone());
        let entry = buckets.entry(key).or_insert_with(|| Bucket {
            kind: f.failure_kind.clone(),
            template: f.error_template.clone(),
            count: 0,
            first_seen: None,
            last_seen: None,
            manifests: Vec::new(),
            task_ids: Vec::new(),
            run_ids: Vec::new(),
            exemplar_message: None,
            exemplar_at: None,
        });
        entry.count += 1;
        push_unique(&mut entry.manifests, &f.manifest_name);
        push_unique(&mut entry.task_ids, &f.task_id);
        push_unique(&mut entry.run_ids, &f.run_id);
        match (entry.first_seen, f.occurred_at) {
            (None, Some(t)) => entry.first_seen = Some(t),
            (Some(a), Some(b)) if b < a => entry.first_seen = Some(b),
            _ => {}
        }
        match (entry.last_seen, f.occurred_at) {
            (None, Some(t)) => entry.last_seen = Some(t),
            (Some(a), Some(b)) if b > a => entry.last_seen = Some(b),
            _ => {}
        }
        // Pick the oldest available message as the exemplar so the
        // operator sees the original failure shape, not the most
        // recent which may have been re-tried/altered.
        if let Some(msg) = &f.error_message {
            match (entry.exemplar_at, f.occurred_at) {
                (None, _) => {
                    entry.exemplar_message = Some(msg.clone());
                    entry.exemplar_at = f.occurred_at;
                }
                (Some(a), Some(b)) if b < a => {
                    entry.exemplar_message = Some(msg.clone());
                    entry.exemplar_at = Some(b);
                }
                _ => {}
            }
        }
    }

    let mut clusters: Vec<Cluster> = buckets
        .into_values()
        .map(|b| Cluster {
            kind: b.kind,
            template: b.template,
            count: b.count,
            first_seen: b.first_seen,
            last_seen: b.last_seen,
            manifests: b.manifests,
            task_ids: b.task_ids,
            run_ids: b.run_ids,
            exemplar_message: b.exemplar_message,
        })
        .collect();

    clusters.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(b.last_seen.cmp(&a.last_seen))
            .then(a.kind.cmp(&b.kind))
    });
    clusters
}

fn push_unique(vec: &mut Vec<String>, val: &str) {
    if !vec.iter().any(|v| v == val) {
        vec.push(val.to_string());
    }
}

struct Bucket {
    kind: String,
    template: Option<String>,
    count: usize,
    first_seen: Option<i64>,
    last_seen: Option<i64>,
    manifests: Vec<String>,
    task_ids: Vec<String>,
    run_ids: Vec<String>,
    exemplar_message: Option<String>,
    exemplar_at: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fail(kind: &str, msg: Option<&str>, run: &str, task: &str, t: i64) -> TaskFailureDigest {
        let template = msg.map(super::super::tokenizer::canonicalize);
        TaskFailureDigest {
            run_id: run.into(),
            manifest_name: "smoke".into(),
            task_id: task.into(),
            parent_task_id: None,
            failure_kind: kind.into(),
            error_message: msg.map(|s| s.into()),
            error_template: template,
            model: None,
            duration_ms: None,
            occurred_at: Some(t),
        }
    }

    #[test]
    fn same_shape_failures_collapse_to_one_cluster() {
        let failures = vec![
            fail(
                "worker_crash",
                Some("exit 137 in /tmp/a.sh"),
                "r1",
                "t1",
                100,
            ),
            fail("worker_crash", Some("exit 1 in /var/x.py"), "r2", "t2", 200),
            fail(
                "worker_crash",
                Some("exit 99 in /opt/y.sh"),
                "r3",
                "t3",
                300,
            ),
        ];
        let clusters = cluster_failures(&failures);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].count, 3);
        assert_eq!(
            clusters[0].template.as_deref(),
            Some("exit <NUM> in <PATH>")
        );
        assert_eq!(clusters[0].run_ids.len(), 3);
        assert_eq!(clusters[0].first_seen, Some(100));
        assert_eq!(clusters[0].last_seen, Some(300));
        // Oldest message is the exemplar.
        assert_eq!(
            clusters[0].exemplar_message.as_deref(),
            Some("exit 137 in /tmp/a.sh")
        );
    }

    #[test]
    fn different_kinds_never_co_cluster() {
        let failures = vec![
            fail("rate_limit", None, "r1", "t1", 100),
            fail("auth_failure", None, "r2", "t2", 200),
        ];
        let clusters = cluster_failures(&failures);
        assert_eq!(clusters.len(), 2);
    }

    #[test]
    fn structured_failures_cluster_by_kind_alone() {
        let failures = vec![
            fail("rate_limit", None, "r1", "t1", 100),
            fail("rate_limit", None, "r2", "t2", 200),
            fail("rate_limit", None, "r3", "t3", 300),
        ];
        let clusters = cluster_failures(&failures);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].count, 3);
        assert!(clusters[0].template.is_none());
    }

    #[test]
    fn count_desc_then_last_seen_desc() {
        let failures = vec![
            fail("rate_limit", None, "r1", "t1", 100),
            fail("auth_failure", None, "r2", "t2", 999),
            fail("rate_limit", None, "r3", "t3", 200),
            fail("rate_limit", None, "r4", "t4", 300),
        ];
        let clusters = cluster_failures(&failures);
        assert_eq!(clusters[0].kind, "rate_limit");
        assert_eq!(clusters[0].count, 3);
        assert_eq!(clusters[1].kind, "auth_failure");
        assert_eq!(clusters[1].count, 1);
    }
}
