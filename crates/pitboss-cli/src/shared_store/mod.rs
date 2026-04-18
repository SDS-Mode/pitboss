//! In-memory per-run shared store for hub-mediated coordination between
//! the lead and workers. See
//! `docs/superpowers/specs/2026-04-18-worker-shared-store-design.md`.

pub mod leases;
pub mod tools;
pub use leases::{AcquireResult, Lease, LeaseRegistry};

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;

/// One stored entry, keyed by path in the containing `SharedStore`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub value: Vec<u8>,
    pub version: u64,
    pub written_by: String,
    pub written_at: DateTime<Utc>,
}

/// Internal notification event fired on successful write.
#[derive(Debug, Clone)]
struct NotifyEvent {
    path: PathBuf,
    version: u64,
}

/// Errors returned by store operations. Each maps to a stable MCP error
/// `code` field; see the spec's "Error shape" section.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid argument: {0}")]
    InvalidArg(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("store limit exceeded: {which:?}")]
    LimitExceeded { which: LimitKind },
    #[error("timeout")]
    Timeout,
    #[error("store shutdown")]
    Shutdown,
    #[error("conflict")]
    Conflict,
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

/// Per-run write-size limits on the store. Configurable via
/// `SharedStore::with_limits`; defaults apply in `SharedStore::new`.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_value_bytes: usize,
    pub max_total_bytes: usize,
    pub max_keys: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_value_bytes: 1024 * 1024,      // 1 MiB
            max_total_bytes: 64 * 1024 * 1024, // 64 MiB
            max_keys: 10_000,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LimitKind {
    Value,
    Total,
    Count,
}

/// Who's making the call, as injected by `mcp-bridge --actor-id / --actor-role`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallerIdentity {
    pub id: String,
    pub role: ActorRole,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ActorRole {
    Lead,
    Worker,
}

const LIST_RESULT_CAP: usize = 1000;

fn authorize_write(
    path: &str,
    caller: &CallerIdentity,
    override_flag: bool,
) -> Result<(), StoreError> {
    let namespace = path.split('/').nth(1).unwrap_or("");
    match (namespace, caller.role) {
        ("ref", ActorRole::Lead) => Ok(()),
        ("ref", ActorRole::Worker) => Err(StoreError::Forbidden("/ref is lead-write-only".into())),
        ("peer", _) => {
            let actor_seg = path.split('/').nth(2).unwrap_or("");
            match (actor_seg == caller.id, caller.role, override_flag) {
                (true, _, _) => Ok(()),
                (_, ActorRole::Lead, true) => Ok(()),
                (_, ActorRole::Lead, false) => Err(StoreError::Forbidden(
                    "lead may write /peer/<other>/* only with override=true".into(),
                )),
                (_, ActorRole::Worker, _) => Err(StoreError::Forbidden(
                    "workers may write only their own /peer/<self>/*".into(),
                )),
            }
        }
        ("shared", _) => Ok(()),
        ("leases", _) => Err(StoreError::Forbidden(
            "use lease_acquire, not kv_set on /leases/*".into(),
        )),
        _ => Err(StoreError::Forbidden(format!(
            "unknown namespace: /{namespace}"
        ))),
    }
}

pub struct SharedStore {
    entries: RwLock<HashMap<PathBuf, Entry>>,
    limits: Limits,
    notifier: broadcast::Sender<NotifyEvent>,
    cancel: CancellationToken,
    leases: LeaseRegistry,
    lease_notifier: broadcast::Sender<String>,
}

impl SharedStore {
    pub fn new() -> Self {
        Self::with_limits(Limits::default())
    }

    pub fn with_limits(limits: Limits) -> Self {
        let (notifier, _) = broadcast::channel(256);
        let (lease_notifier, _) = broadcast::channel(64);
        Self {
            entries: RwLock::new(HashMap::new()),
            limits,
            notifier,
            cancel: CancellationToken::new(),
            leases: LeaseRegistry::new(),
            lease_notifier,
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
        if value.len() > self.limits.max_value_bytes {
            return Err(StoreError::LimitExceeded {
                which: LimitKind::Value,
            });
        }
        let key = PathBuf::from(path);
        let mut entries = self.entries.write().await;
        let prev = entries.get(&key);
        let is_new_key = prev.is_none();
        let prev_size = prev.map_or(0, |e| e.value.len());
        let new_size = value.len();
        if is_new_key && entries.len() >= self.limits.max_keys {
            return Err(StoreError::LimitExceeded {
                which: LimitKind::Count,
            });
        }
        let current_total: usize = entries.values().map(|e| e.value.len()).sum();
        let projected_total = current_total - prev_size + new_size;
        if projected_total > self.limits.max_total_bytes {
            return Err(StoreError::LimitExceeded {
                which: LimitKind::Total,
            });
        }
        let version = prev.map_or(1, |e| e.version + 1);
        let notify_key = key.clone();
        entries.insert(
            key,
            Entry {
                value,
                version,
                written_by: written_by.to_string(),
                written_at: Utc::now(),
            },
        );
        drop(entries);
        let _ = self.notifier.send(NotifyEvent {
            path: notify_key,
            version,
        });
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
        if new_value.len() > self.limits.max_value_bytes {
            return Err(StoreError::LimitExceeded {
                which: LimitKind::Value,
            });
        }
        let key = PathBuf::from(path);
        let mut entries = self.entries.write().await;
        let prev = entries.get(&key);
        let current_version = prev.map_or(0, |e| e.version);
        if current_version != expected_version {
            return Ok(CasResult {
                ok: false,
                current_version,
            });
        }
        let is_new_key = prev.is_none();
        let prev_size = prev.map_or(0, |e| e.value.len());
        let new_size = new_value.len();
        if is_new_key && entries.len() >= self.limits.max_keys {
            return Err(StoreError::LimitExceeded {
                which: LimitKind::Count,
            });
        }
        let current_total: usize = entries.values().map(|e| e.value.len()).sum();
        let projected_total = current_total - prev_size + new_size;
        if projected_total > self.limits.max_total_bytes {
            return Err(StoreError::LimitExceeded {
                which: LimitKind::Total,
            });
        }
        let new_version = current_version + 1;
        let notify_key = key.clone();
        entries.insert(
            key,
            Entry {
                value: new_value,
                version: new_version,
                written_by: written_by.to_string(),
                written_at: Utc::now(),
            },
        );
        drop(entries);
        let _ = self.notifier.send(NotifyEvent {
            path: notify_key,
            version: new_version,
        });
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

    pub async fn authorized_set(
        &self,
        path: &str,
        value: Vec<u8>,
        caller: &CallerIdentity,
        override_flag: bool,
    ) -> Result<u64, StoreError> {
        authorize_write(path, caller, override_flag)?;
        self.set(path, value, &caller.id).await
    }

    pub async fn authorized_cas(
        &self,
        path: &str,
        expected_version: u64,
        new_value: Vec<u8>,
        caller: &CallerIdentity,
        override_flag: bool,
    ) -> Result<CasResult, StoreError> {
        authorize_write(path, caller, override_flag)?;
        self.cas(path, expected_version, new_value, &caller.id)
            .await
    }

    pub async fn wait(
        &self,
        path: &str,
        timeout: std::time::Duration,
        min_version: Option<u64>,
    ) -> Result<Entry, StoreError> {
        validate_path(path)?;
        let key = PathBuf::from(path);
        let min = min_version.unwrap_or(1);

        // Subscribe BEFORE the fast-path check to avoid missing a write
        // that lands between our read and our subscribe.
        let mut rx = self.notifier.subscribe();

        if let Some(entry) = self.entries.read().await.get(&key) {
            if entry.version >= min {
                return Ok(entry.clone());
            }
        }

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(StoreError::Timeout);
            }
            tokio::select! {
                _ = self.cancel.cancelled() => return Err(StoreError::Shutdown),
                res = tokio::time::timeout(remaining, rx.recv()) => {
                    match res {
                        Err(_elapsed) => return Err(StoreError::Timeout),
                        Ok(Err(broadcast::error::RecvError::Closed)) => return Err(StoreError::Shutdown),
                        Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                        Ok(Ok(evt)) => {
                            if evt.path == key && evt.version >= min {
                                if let Some(entry) = self.entries.read().await.get(&key) {
                                    if entry.version >= min {
                                        return Ok(entry.clone());
                                    }
                                }
                            }
                            // Otherwise: not our event, loop.
                        }
                    }
                }
            }
        }
    }

    /// Wake all blocking waiters with `StoreError::Shutdown`. Called from
    /// `DispatchState` drop or explicit finalize.
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }

    pub async fn lease_acquire(
        &self,
        name: &str,
        ttl: std::time::Duration,
        wait: Option<std::time::Duration>,
        caller: &CallerIdentity,
    ) -> Result<AcquireResult, StoreError> {
        // Non-blocking attempt first.
        match self.leases.acquire(name, ttl, caller).await {
            Ok(lease) => {
                return Ok(AcquireResult {
                    acquired: true,
                    lease_id: Some(lease.lease_id),
                    expires_at: Some(lease.expires_at),
                });
            }
            Err(StoreError::Conflict) => {}
            Err(e) => return Err(e),
        }

        // If no wait requested, return failure immediately.
        let Some(wait) = wait else {
            return Ok(AcquireResult {
                acquired: false,
                lease_id: None,
                expires_at: None,
            });
        };

        // Subscribe BEFORE retrying so we don't miss a release that lands
        // between our first attempt and the subscribe.
        let mut rx = self.lease_notifier.subscribe();
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(AcquireResult {
                    acquired: false,
                    lease_id: None,
                    expires_at: None,
                });
            }
            tokio::select! {
                _ = self.cancel.cancelled() => return Err(StoreError::Shutdown),
                res = tokio::time::timeout(remaining, rx.recv()) => {
                    match res {
                        Err(_elapsed) => {
                            return Ok(AcquireResult {
                                acquired: false,
                                lease_id: None,
                                expires_at: None,
                            });
                        }
                        Ok(_) => {
                            // Something was released — retry.
                            match self.leases.acquire(name, ttl, caller).await {
                                Ok(lease) => {
                                    return Ok(AcquireResult {
                                        acquired: true,
                                        lease_id: Some(lease.lease_id),
                                        expires_at: Some(lease.expires_at),
                                    });
                                }
                                Err(StoreError::Conflict) => continue,
                                Err(e) => return Err(e),
                            }
                        }
                    }
                }
            }
        }
    }

    pub async fn lease_release(
        &self,
        lease_id: &str,
        caller: &CallerIdentity,
    ) -> Result<(), StoreError> {
        self.leases.release(lease_id, caller).await?;
        let _ = self.lease_notifier.send(lease_id.to_string());
        Ok(())
    }

    /// Release all leases held by `actor_id`. Intended to be called when an
    /// actor's MCP connection drops (lease-on-connection semantics). Wakes
    /// any waiters subscribed via `lease_acquire(wait_secs > 0)`.
    pub async fn release_all_for_actor(&self, actor_id: &str) {
        let released = self.leases.release_all_for_actor(actor_id).await;
        for name in released {
            let _ = self.lease_notifier.send(name);
        }
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
    use std::time::Duration;

    #[tokio::test]
    async fn wait_returns_immediately_if_already_written() {
        let s = SharedStore::new();
        s.set("/ref/k", b"hi".to_vec(), "lead").await.unwrap();
        let entry = s
            .wait("/ref/k", Duration::from_secs(1), None)
            .await
            .unwrap();
        assert_eq!(entry.value, b"hi");
    }

    #[tokio::test]
    async fn wait_blocks_until_key_is_written() {
        let s = std::sync::Arc::new(SharedStore::new());
        let s2 = s.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            s2.set("/ref/k", b"ready".to_vec(), "lead").await.unwrap();
        });
        let entry = s
            .wait("/ref/k", Duration::from_secs(1), None)
            .await
            .unwrap();
        assert_eq!(entry.value, b"ready");
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn wait_respects_min_version() {
        let s = std::sync::Arc::new(SharedStore::new());
        s.set("/ref/k", b"v1".to_vec(), "lead").await.unwrap();
        let s2 = s.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            s2.set("/ref/k", b"v2".to_vec(), "lead").await.unwrap();
        });
        let entry = s
            .wait("/ref/k", Duration::from_secs(1), Some(2))
            .await
            .unwrap();
        assert_eq!(entry.value, b"v2");
        assert_eq!(entry.version, 2);
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn wait_times_out_when_key_never_written() {
        let s = SharedStore::new();
        let err = s
            .wait("/ref/never", Duration::from_millis(50), None)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Timeout));
    }

    #[tokio::test]
    async fn wait_wakes_on_shutdown() {
        let s = std::sync::Arc::new(SharedStore::new());
        let s2 = s.clone();
        let waiter =
            tokio::spawn(async move { s2.wait("/ref/never", Duration::from_secs(10), None).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        s.shutdown();
        let res = waiter.await.unwrap();
        assert!(matches!(res, Err(StoreError::Shutdown)));
    }

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

    fn lead() -> CallerIdentity {
        CallerIdentity {
            id: "lead-A".into(),
            role: ActorRole::Lead,
        }
    }
    fn worker(id: &str) -> CallerIdentity {
        CallerIdentity {
            id: id.into(),
            role: ActorRole::Worker,
        }
    }

    #[tokio::test]
    async fn lead_can_write_ref() {
        let s = SharedStore::new();
        s.authorized_set("/ref/k", b"v".to_vec(), &lead(), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn worker_cannot_write_ref() {
        let s = SharedStore::new();
        let err = s
            .authorized_set("/ref/k", b"v".to_vec(), &worker("w1"), false)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Forbidden(_)));
    }

    #[tokio::test]
    async fn worker_can_write_own_peer() {
        let s = SharedStore::new();
        s.authorized_set("/peer/w1/out", b"v".to_vec(), &worker("w1"), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn worker_cannot_write_other_peer() {
        let s = SharedStore::new();
        let err = s
            .authorized_set("/peer/w2/out", b"v".to_vec(), &worker("w1"), false)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Forbidden(_)));
    }

    #[tokio::test]
    async fn lead_override_can_write_any_peer() {
        let s = SharedStore::new();
        s.authorized_set("/peer/w1/out", b"v".to_vec(), &lead(), true)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn lead_override_rejected_for_worker() {
        let s = SharedStore::new();
        let err = s
            .authorized_set("/peer/w2/out", b"v".to_vec(), &worker("w1"), true)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Forbidden(_)));
    }

    #[tokio::test]
    async fn all_actors_can_write_shared() {
        let s = SharedStore::new();
        s.authorized_set("/shared/k", b"v".to_vec(), &worker("w1"), false)
            .await
            .unwrap();
        s.authorized_set("/shared/k", b"v2".to_vec(), &lead(), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn leases_namespace_rejects_kv_set() {
        let s = SharedStore::new();
        let err = s
            .authorized_set("/leases/foo", b"v".to_vec(), &lead(), false)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Forbidden(_)));
    }

    #[tokio::test]
    async fn unknown_namespace_rejected() {
        let s = SharedStore::new();
        let err = s
            .authorized_set("/other/foo", b"v".to_vec(), &lead(), false)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Forbidden(_)));
    }

    #[tokio::test]
    async fn rejects_oversized_value() {
        let s = SharedStore::new();
        let big = vec![0u8; 1024 * 1024 + 1];
        let err = s.set("/ref/big", big, "lead").await.unwrap_err();
        match err {
            StoreError::LimitExceeded { which } => assert_eq!(which, LimitKind::Value),
            other => panic!("expected LimitExceeded{{ Value }}, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_when_total_size_exceeded() {
        let s = SharedStore::with_limits(Limits {
            max_value_bytes: 1024,
            max_total_bytes: 2048,
            max_keys: 1000,
        });
        s.set("/ref/a", vec![0u8; 1024], "lead").await.unwrap();
        s.set("/ref/b", vec![0u8; 1024], "lead").await.unwrap();
        let err = s.set("/ref/c", vec![0u8; 1], "lead").await.unwrap_err();
        match err {
            StoreError::LimitExceeded { which } => assert_eq!(which, LimitKind::Total),
            other => panic!("expected LimitExceeded{{ Total }}, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_when_key_count_exceeded() {
        let s = SharedStore::with_limits(Limits {
            max_value_bytes: 10,
            max_total_bytes: 10_000,
            max_keys: 3,
        });
        s.set("/ref/a", b"x".to_vec(), "lead").await.unwrap();
        s.set("/ref/b", b"x".to_vec(), "lead").await.unwrap();
        s.set("/ref/c", b"x".to_vec(), "lead").await.unwrap();
        let err = s.set("/ref/d", b"x".to_vec(), "lead").await.unwrap_err();
        match err {
            StoreError::LimitExceeded { which } => assert_eq!(which, LimitKind::Count),
            other => panic!("expected LimitExceeded{{ Count }}, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn overwrite_does_not_count_against_key_limit() {
        let s = SharedStore::with_limits(Limits {
            max_value_bytes: 10,
            max_total_bytes: 10_000,
            max_keys: 2,
        });
        s.set("/ref/a", b"x".to_vec(), "lead").await.unwrap();
        s.set("/ref/a", b"y".to_vec(), "lead").await.unwrap();
    }

    #[tokio::test]
    async fn store_exposes_lease_acquire_release() {
        let s = SharedStore::new();
        let caller = worker("w1");
        let lease = s
            .lease_acquire("job", std::time::Duration::from_secs(30), None, &caller)
            .await
            .unwrap();
        assert!(lease.acquired);
        let lease_id = lease.lease_id.unwrap();
        let res = s
            .lease_acquire(
                "job",
                std::time::Duration::from_secs(30),
                None,
                &worker("w2"),
            )
            .await
            .unwrap();
        assert!(!res.acquired);
        s.lease_release(&lease_id, &caller).await.unwrap();
    }

    #[tokio::test]
    async fn lease_acquire_with_wait_succeeds_when_released() {
        let s = std::sync::Arc::new(SharedStore::new());
        let lease = s
            .lease_acquire(
                "job",
                std::time::Duration::from_secs(30),
                None,
                &worker("w1"),
            )
            .await
            .unwrap();
        let lease_id = lease.lease_id.unwrap();
        let s2 = s.clone();
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            s2.lease_release(&lease_id, &worker("w1")).await.unwrap();
        });
        let res = s
            .lease_acquire(
                "job",
                std::time::Duration::from_secs(30),
                Some(std::time::Duration::from_secs(1)),
                &worker("w2"),
            )
            .await
            .unwrap();
        assert!(res.acquired);
        releaser.await.unwrap();
    }

    #[tokio::test]
    async fn release_all_for_actor_frees_held_leases() {
        let s = SharedStore::new();
        s.lease_acquire("a", std::time::Duration::from_secs(30), None, &worker("w1"))
            .await
            .unwrap();
        s.lease_acquire("b", std::time::Duration::from_secs(30), None, &worker("w1"))
            .await
            .unwrap();
        s.lease_acquire("c", std::time::Duration::from_secs(30), None, &worker("w2"))
            .await
            .unwrap();
        s.release_all_for_actor("w1").await;

        let a = s
            .lease_acquire("a", std::time::Duration::from_secs(30), None, &worker("w3"))
            .await
            .unwrap();
        assert!(a.acquired);
        let b = s
            .lease_acquire("b", std::time::Duration::from_secs(30), None, &worker("w3"))
            .await
            .unwrap();
        assert!(b.acquired);
        // w2 still holds "c"
        let c = s
            .lease_acquire("c", std::time::Duration::from_secs(30), None, &worker("w3"))
            .await
            .unwrap();
        assert!(!c.acquired);
    }
}
