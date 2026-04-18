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
}
