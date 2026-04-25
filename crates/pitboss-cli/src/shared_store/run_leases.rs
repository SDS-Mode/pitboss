//! Run-global lease registry. Distinct from per-layer /leases/*
//! (which lives in each layer's KvStore). Used for resources that
//! span sub-trees (e.g., operator's filesystem). Any actor in the
//! tree can acquire; leases auto-release on actor termination via
//! explicit hooks in the cancel/reap paths.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::dispatch::actor::ActorId;

#[derive(Debug, Clone)]
pub struct LeaseHandle {
    pub key: String,
    pub holder: ActorId,
    pub acquired_at: Instant,
    pub ttl: Duration,
}

#[derive(Debug, Default)]
pub struct LeaseRegistry {
    inner: Mutex<HashMap<String, LeaseHandle>>,
}

impl LeaseRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to acquire a lease. Returns Ok(handle) if acquired,
    /// Err(holder) if currently held by another actor.
    pub async fn try_acquire(
        &self,
        key: &str,
        holder: &str,
        ttl: Duration,
    ) -> Result<LeaseHandle, ActorId> {
        let mut map = self.inner.lock().await;
        // Auto-expire: remove any lease past its TTL before checking
        let now = Instant::now();
        if let Some(existing) = map.get(key) {
            if now.duration_since(existing.acquired_at) <= existing.ttl && existing.holder != holder
            {
                return Err(existing.holder.clone());
            }
            // else: expired, fall through to insert
        }
        let handle = LeaseHandle {
            key: key.to_string(),
            holder: holder.to_string(),
            acquired_at: now,
            ttl,
        };
        map.insert(key.to_string(), handle.clone());
        Ok(handle)
    }

    pub async fn release(&self, key: &str, holder: &str) -> bool {
        let mut map = self.inner.lock().await;
        if map.get(key).is_some_and(|h| h.holder == holder) {
            map.remove(key);
            true
        } else {
            false
        }
    }

    /// Auto-release every lease held by `actor_id`. Called from the
    /// cancel/reap paths when an actor terminates.
    pub async fn release_all_held_by(&self, actor_id: &str) -> usize {
        let mut map = self.inner.lock().await;
        let to_remove: Vec<_> = map
            .iter()
            .filter(|(_, h)| h.holder == actor_id)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &to_remove {
            map.remove(k);
        }
        to_remove.len()
    }

    pub async fn snapshot(&self) -> Vec<LeaseHandle> {
        self.inner.lock().await.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquire_returns_handle_when_free() {
        let r = LeaseRegistry::new();
        let h = r
            .try_acquire("foo", "actor-A", Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(h.key, "foo");
        assert_eq!(h.holder, "actor-A");
    }

    #[tokio::test]
    async fn acquire_returns_err_with_current_holder_when_taken() {
        let r = LeaseRegistry::new();
        r.try_acquire("foo", "actor-A", Duration::from_secs(60))
            .await
            .unwrap();
        let err = r
            .try_acquire("foo", "actor-B", Duration::from_secs(60))
            .await
            .unwrap_err();
        assert_eq!(err, "actor-A");
    }

    #[tokio::test]
    async fn release_succeeds_for_holder_only() {
        let r = LeaseRegistry::new();
        r.try_acquire("foo", "actor-A", Duration::from_secs(60))
            .await
            .unwrap();
        assert!(!r.release("foo", "actor-B").await);
        assert!(r.release("foo", "actor-A").await);
    }

    #[tokio::test]
    async fn release_all_held_by_drops_actor_leases() {
        let r = LeaseRegistry::new();
        r.try_acquire("foo", "actor-A", Duration::from_secs(60))
            .await
            .unwrap();
        r.try_acquire("bar", "actor-A", Duration::from_secs(60))
            .await
            .unwrap();
        r.try_acquire("baz", "actor-B", Duration::from_secs(60))
            .await
            .unwrap();
        let dropped = r.release_all_held_by("actor-A").await;
        assert_eq!(dropped, 2);
        assert_eq!(r.snapshot().await.len(), 1);
    }

    #[tokio::test]
    async fn expired_lease_can_be_reacquired_by_other_actor() {
        let r = LeaseRegistry::new();
        r.try_acquire("foo", "actor-A", Duration::from_millis(10))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let h = r
            .try_acquire("foo", "actor-B", Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(h.holder, "actor-B");
    }
}
