//! In-memory per-run shared store for hub-mediated coordination between
//! the lead and workers. See
//! `docs/superpowers/specs/2026-04-18-worker-shared-store-design.md`.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// One stored entry, keyed by path in the containing `SharedStore`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub value: Vec<u8>,
    pub version: u64,
    pub written_by: String,
    pub written_at: DateTime<Utc>,
}

/// Errors returned by store operations. Each maps to a stable MCP error
/// `code` field; see the spec's "Error shape" section.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid argument: {0}")]
    InvalidArg(String),
}

/// Outcome of a compare-and-swap. `ok=true` means the write happened;
/// `current_version` is the version after the op (or unchanged if `ok=false`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CasResult {
    pub ok: bool,
    pub current_version: u64,
}

/// Metadata-only view of an entry, returned by `list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListMetadata {
    pub path: String,
    pub version: u64,
    pub written_by: String,
    pub written_at: DateTime<Utc>,
    pub size_bytes: u64,
}

const LIST_RESULT_CAP: usize = 1000;

pub struct SharedStore {
    entries: RwLock<HashMap<PathBuf, Entry>>,
}

impl SharedStore {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    pub async fn get(&self, path: &str) -> Option<Entry> {
        let key = PathBuf::from(path);
        self.entries.read().await.get(&key).cloned()
    }

    pub async fn set(
        &self,
        path: &str,
        value: Vec<u8>,
        written_by: &str,
    ) -> Result<u64, StoreError> {
        validate_path(path)?;
        let key = PathBuf::from(path);
        let mut entries = self.entries.write().await;
        let version = entries.get(&key).map_or(1, |e| e.version + 1);
        entries.insert(
            key,
            Entry {
                value,
                version,
                written_by: written_by.to_string(),
                written_at: Utc::now(),
            },
        );
        Ok(version)
    }

    pub async fn cas(
        &self,
        path: &str,
        expected_version: u64,
        new_value: Vec<u8>,
        written_by: &str,
    ) -> Result<CasResult, StoreError> {
        validate_path(path)?;
        let key = PathBuf::from(path);
        let mut entries = self.entries.write().await;
        let current_version = entries.get(&key).map_or(0, |e| e.version);
        if current_version != expected_version {
            return Ok(CasResult {
                ok: false,
                current_version,
            });
        }
        let new_version = current_version + 1;
        entries.insert(
            key,
            Entry {
                value: new_value,
                version: new_version,
                written_by: written_by.to_string(),
                written_at: Utc::now(),
            },
        );
        Ok(CasResult {
            ok: true,
            current_version: new_version,
        })
    }

    pub async fn list(&self, pattern: &str) -> Result<Vec<ListMetadata>, StoreError> {
        let pat = glob::Pattern::new(pattern)
            .map_err(|e| StoreError::InvalidArg(format!("bad glob: {e}")))?;
        let opts = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        };
        let entries = self.entries.read().await;
        let mut out: Vec<ListMetadata> = entries
            .iter()
            .filter_map(|(key, entry)| {
                let path = key.to_string_lossy().to_string();
                if pat.matches_with(&path, opts) {
                    Some(ListMetadata {
                        path,
                        version: entry.version,
                        written_by: entry.written_by.clone(),
                        written_at: entry.written_at,
                        size_bytes: entry.value.len() as u64,
                    })
                } else {
                    None
                }
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out.truncate(LIST_RESULT_CAP);
        Ok(out)
    }
}

impl Default for SharedStore {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_path(path: &str) -> Result<(), StoreError> {
    if !path.starts_with('/') {
        return Err(StoreError::InvalidArg("path must be absolute".into()));
    }
    if path.contains("..") {
        return Err(StoreError::InvalidArg("path must not contain `..`".into()));
    }
    if path != "/" && path.split('/').filter(|s| s.is_empty()).count() > 1 {
        // More than one empty segment means "//" somewhere in the path.
        return Err(StoreError::InvalidArg(
            "path must not contain empty segments".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_get_round_trip_bumps_version() {
        let s = SharedStore::new();
        assert!(s.get("/ref/foo").await.is_none());

        let v1 = s.set("/ref/foo", b"hello".to_vec(), "lead").await.unwrap();
        assert_eq!(v1, 1);
        let entry = s.get("/ref/foo").await.unwrap();
        assert_eq!(entry.value, b"hello");
        assert_eq!(entry.version, 1);
        assert_eq!(entry.written_by, "lead");

        let v2 = s.set("/ref/foo", b"world".to_vec(), "lead").await.unwrap();
        assert_eq!(v2, 2);
        let entry = s.get("/ref/foo").await.unwrap();
        assert_eq!(entry.value, b"world");
        assert_eq!(entry.version, 2);
    }

    #[tokio::test]
    async fn set_rejects_non_absolute_path() {
        let s = SharedStore::new();
        let err = s
            .set("relative/foo", b"x".to_vec(), "lead")
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::InvalidArg(_)));
    }

    #[tokio::test]
    async fn set_rejects_double_dot_path() {
        let s = SharedStore::new();
        let err = s
            .set("/ref/../etc/passwd", b"x".to_vec(), "lead")
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::InvalidArg(_)));
    }

    #[tokio::test]
    async fn cas_succeeds_when_version_matches() {
        let s = SharedStore::new();
        s.set("/ref/k", b"v1".to_vec(), "lead").await.unwrap();
        let res = s.cas("/ref/k", 1, b"v2".to_vec(), "lead").await.unwrap();
        assert!(res.ok);
        assert_eq!(res.current_version, 2);
        let entry = s.get("/ref/k").await.unwrap();
        assert_eq!(entry.value, b"v2");
        assert_eq!(entry.version, 2);
    }

    #[tokio::test]
    async fn cas_fails_when_version_mismatches() {
        let s = SharedStore::new();
        s.set("/ref/k", b"v1".to_vec(), "lead").await.unwrap();
        let res = s.cas("/ref/k", 99, b"v2".to_vec(), "lead").await.unwrap();
        assert!(!res.ok);
        assert_eq!(res.current_version, 1);
        let entry = s.get("/ref/k").await.unwrap();
        assert_eq!(entry.value, b"v1");
    }

    #[tokio::test]
    async fn cas_with_expected_zero_creates_if_missing() {
        let s = SharedStore::new();
        let res = s.cas("/ref/k", 0, b"v1".to_vec(), "lead").await.unwrap();
        assert!(res.ok);
        assert_eq!(res.current_version, 1);
    }

    #[tokio::test]
    async fn cas_with_expected_zero_fails_if_present() {
        let s = SharedStore::new();
        s.set("/ref/k", b"v1".to_vec(), "lead").await.unwrap();
        let res = s.cas("/ref/k", 0, b"v2".to_vec(), "lead").await.unwrap();
        assert!(!res.ok);
        assert_eq!(res.current_version, 1);
    }

    #[tokio::test]
    async fn list_matches_single_segment_glob() {
        let s = SharedStore::new();
        s.set("/ref/a", b"".to_vec(), "lead").await.unwrap();
        s.set("/ref/b", b"".to_vec(), "lead").await.unwrap();
        s.set("/ref/nested/c", b"".to_vec(), "lead").await.unwrap();
        let mut paths: Vec<String> = s
            .list("/ref/*")
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.path)
            .collect();
        paths.sort();
        assert_eq!(paths, vec!["/ref/a".to_string(), "/ref/b".to_string()]);
    }

    #[tokio::test]
    async fn list_matches_cross_segment_glob() {
        let s = SharedStore::new();
        s.set("/ref/a", b"".to_vec(), "lead").await.unwrap();
        s.set("/ref/nested/c", b"".to_vec(), "lead").await.unwrap();
        let mut paths: Vec<String> = s
            .list("/ref/**")
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.path)
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            vec!["/ref/a".to_string(), "/ref/nested/c".to_string()]
        );
    }

    #[tokio::test]
    async fn list_returns_metadata_not_values() {
        let s = SharedStore::new();
        s.set("/ref/a", b"hello".to_vec(), "lead").await.unwrap();
        let entries = s.list("/ref/*").await.unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.path, "/ref/a");
        assert_eq!(e.size_bytes, 5);
        assert_eq!(e.version, 1);
        assert_eq!(e.written_by, "lead");
    }

    #[tokio::test]
    async fn list_caps_results_at_1000() {
        let s = SharedStore::new();
        for i in 0..1500 {
            s.set(&format!("/shared/item-{i:05}"), b"x".to_vec(), "lead")
                .await
                .unwrap();
        }
        let entries = s.list("/shared/*").await.unwrap();
        assert_eq!(entries.len(), 1000);
        assert_eq!(entries[0].path, "/shared/item-00000");
        assert_eq!(entries[999].path, "/shared/item-00999");
    }
}
