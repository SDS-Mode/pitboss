//! Named distributed-lock leases with TTL + holder identity.
//!
//! Leases are tracked by name. `acquire` is non-blocking by default (the
//! blocking variant with a `wait_secs` argument lands in Task 8). `release`
//! is identity-checked: only the recorded holder can release. A lease
//! auto-expires after its TTL elapses; auto-release on MCP-connection drop
//! is wired in a later task.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::time::Instant;
use uuid::Uuid;

use super::{CallerIdentity, StoreError};

/// A held lease's metadata. Returned from `acquire`, readable via `list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lease {
    pub name: String,
    pub lease_id: String,
    pub holder: String,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Outcome of `SharedStore::lease_acquire`. Shape ties out with the MCP
/// tool's return — `acquired=false` with `None` fields means the caller
/// should retry or give up.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcquireResult {
    pub acquired: bool,
    pub lease_id: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub struct LeaseRegistry {
    inner: Mutex<HashMap<String, (Lease, Instant)>>,
}

impl LeaseRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub async fn acquire(
        &self,
        name: &str,
        ttl: Duration,
        caller: &CallerIdentity,
    ) -> Result<Lease, StoreError> {
        let mut map = self.inner.lock().await;
        Self::prune_expired(&mut map);
        if map.contains_key(name) {
            return Err(StoreError::Conflict);
        }
        let now = Utc::now();
        let lease = Lease {
            name: name.to_string(),
            lease_id: Uuid::now_v7().to_string(),
            holder: caller.id.clone(),
            acquired_at: now,
            expires_at: now
                + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::zero()),
        };
        let deadline = Instant::now() + ttl;
        map.insert(name.to_string(), (lease.clone(), deadline));
        Ok(lease)
    }

    pub async fn release(&self, lease_id: &str, caller: &CallerIdentity) -> Result<(), StoreError> {
        let mut map = self.inner.lock().await;
        Self::prune_expired(&mut map);
        let found = map
            .iter()
            .find(|(_, (l, _))| l.lease_id == lease_id)
            .map(|(name, (l, _))| (name.clone(), l.clone()));
        let Some((name, lease)) = found else {
            return Err(StoreError::Forbidden(
                "lease not held or already released".into(),
            ));
        };
        if lease.holder != caller.id {
            return Err(StoreError::Forbidden("only the holder can release".into()));
        }
        map.remove(&name);
        Ok(())
    }

    pub async fn list(&self) -> Vec<Lease> {
        let mut map = self.inner.lock().await;
        Self::prune_expired(&mut map);
        map.values().map(|(l, _)| l.clone()).collect()
    }

    fn prune_expired(map: &mut HashMap<String, (Lease, Instant)>) {
        let now = Instant::now();
        map.retain(|_, (_, deadline)| *deadline > now);
    }
}

impl Default for LeaseRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_store::ActorRole;

    fn caller(id: &str) -> CallerIdentity {
        CallerIdentity {
            id: id.into(),
            role: ActorRole::Worker,
        }
    }

    #[tokio::test]
    async fn acquire_succeeds_when_free() {
        let reg = LeaseRegistry::new();
        let lease = reg
            .acquire("foo", Duration::from_secs(30), &caller("w1"))
            .await
            .unwrap();
        assert_eq!(lease.name, "foo");
        assert_eq!(lease.holder, "w1");
    }

    #[tokio::test]
    async fn acquire_conflicts_when_held() {
        let reg = LeaseRegistry::new();
        reg.acquire("foo", Duration::from_secs(30), &caller("w1"))
            .await
            .unwrap();
        let err = reg
            .acquire("foo", Duration::from_secs(30), &caller("w2"))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Conflict));
    }

    #[tokio::test]
    async fn release_succeeds_by_holder() {
        let reg = LeaseRegistry::new();
        let lease = reg
            .acquire("foo", Duration::from_secs(30), &caller("w1"))
            .await
            .unwrap();
        reg.release(&lease.lease_id, &caller("w1")).await.unwrap();
        reg.acquire("foo", Duration::from_secs(30), &caller("w2"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn release_rejected_for_non_holder() {
        let reg = LeaseRegistry::new();
        let lease = reg
            .acquire("foo", Duration::from_secs(30), &caller("w1"))
            .await
            .unwrap();
        let err = reg
            .release(&lease.lease_id, &caller("w2"))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Forbidden(_)));
    }

    #[tokio::test]
    async fn lease_auto_expires_after_ttl() {
        let reg = LeaseRegistry::new();
        reg.acquire("foo", Duration::from_millis(30), &caller("w1"))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        reg.acquire("foo", Duration::from_secs(30), &caller("w2"))
            .await
            .unwrap();
    }
}
