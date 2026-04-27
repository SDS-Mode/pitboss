//! Cross-run aggregation, failure clustering, and the read-only digest
//! types served by `/api/insights/*`. Sourced from the on-disk run
//! artefacts (`summary.json`, `summary.jsonl`, `meta.json`) — no
//! database, no schema migration.

pub mod aggregator;
pub mod cache;
pub mod cluster;
pub mod digest;
pub mod tokenizer;

pub use cache::InsightsCache;
pub use cluster::cluster_failures;
pub use digest::{Cluster, ManifestSummary, RunDigest, TaskFailureDigest};
