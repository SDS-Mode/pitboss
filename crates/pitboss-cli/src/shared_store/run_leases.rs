//! Run-global lease registry. Distinct from per-layer /leases/*
//! (which lives in each layer's KvStore). Used for resources that
//! span sub-trees (e.g., operator's filesystem). Any actor in the
//! tree can acquire; leases auto-release on actor termination via
//! explicit hooks in the cancel/reap paths.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify};

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
    /// #153 L6: notify waiters in `acquire_with_wait` whenever a lease
    /// is released or auto-released. Coalesced semantics (single token)
    /// are fine because waiters re-consult the map under the lock on
    /// wake — losing notifications between a release and a slow waiter
    /// just means the *next* release wakes them, which is acceptable
    /// for a small run-global registry.
    release_notify: Arc<Notify>,
}

impl LeaseRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to acquire a lease. Returns Ok(handle) if acquired,
    /// Err(holder) if currently held by another actor.
    ///
    /// **Same-holder semantics (#153 L5):** if `holder` already owns
    /// the lease, this call refreshes `acquired_at` to "now" — i.e. it
    /// acts as an explicit renewal that pushes the deadline out by a
    /// full `ttl`. This is intentional: the registry has no separate
    /// `renew` method, and same-holder reacquire-on-heartbeat is the
    /// only sane outcome for a registry with no shared lock-token.
    /// Callers who want strict no-renew behavior must check
    /// `snapshot()` first.
    pub async fn try_acquire(
        &self,
        key: &str,
        holder: &str,
        ttl: Duration,
    ) -> Result<LeaseHandle, ActorId> {
        let mut map = self.inner.lock().await;
        let now = Instant::now();
        if let Some(existing) = map.get(key) {
            // #153 L5: boundary changed from `<=` to `<` to align with
            // `LeaseRegistry::prune_expired` in leases.rs (`deadline
            // <= now` treats the boundary as expired). With `<=` here,
            // an exactly-at-deadline lease was treated as still held,
            // leaving a one-tick window where waiters got Conflict on
            // a TTL-expired lease.
            if now.duration_since(existing.acquired_at) < existing.ttl && existing.holder != holder
            {
                return Err(existing.holder.clone());
            }
            // else: expired or same-holder renewal — fall through.
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

    /// Try to acquire `key`; if held by another actor, wait up to
    /// `wait` for a release before giving up. Mirrors
    /// `super::SharedStore::lease_acquire` for the run-global registry
    /// so callers don't have to roll their own poll loop. (#153 L6)
    ///
    /// Implementation: the inner `Notify` is poked on every release
    /// path, so waiters wake without polling. Lost wakeups (release
    /// fires before subscribe) are absorbed by Notify's
    /// "permit"-style semantics — the first `notified()` after a
    /// `notify_one` returns immediately. After waking, retries the
    /// non-blocking `try_acquire`; loops until success or deadline.
    pub async fn acquire_with_wait(
        &self,
        key: &str,
        holder: &str,
        ttl: Duration,
        wait: Duration,
    ) -> Result<LeaseHandle, ActorId> {
        // Take a snapshot of the notify handle now so we can subscribe
        // before each retry without re-locking.
        let notify = Arc::clone(&self.release_notify);
        let deadline = Instant::now() + wait;

        // Fast path.
        if let Ok(h) = self.try_acquire(key, holder, ttl).await {
            return Ok(h);
        }

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // Final attempt for the boundary case where TTL just
                // expired but no release fired during our wait.
                return self.try_acquire(key, holder, ttl).await;
            }
            // Subscribe BEFORE retry to avoid a lost-wakeup race with
            // a release that lands between try_acquire and notified.
            let notified = notify.notified();
            tokio::pin!(notified);
            tokio::select! {
                _ = tokio::time::sleep(remaining) => {
                    return self.try_acquire(key, holder, ttl).await;
                }
                _ = &mut notified => {
                    match self.try_acquire(key, holder, ttl).await {
                        Ok(h) => return Ok(h),
                        Err(_) => continue,
                    }
                }
            }
        }
    }

    pub async fn release(&self, key: &str, holder: &str) -> bool {
        let mut map = self.inner.lock().await;
        if map.get(key).is_some_and(|h| h.holder == holder) {
            map.remove(key);
            drop(map);
            // #153 L6: wake any acquire_with_wait waiters.
            self.release_notify.notify_waiters();
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
        let count = to_remove.len();
        drop(map);
        if count > 0 {
            // #153 L6: wake any acquire_with_wait waiters — bulk
            // release can free multiple keys at once.
            self.release_notify.notify_waiters();
        }
        count
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

    /// #153 L5 regression: same-holder reacquire must refresh the
    /// `acquired_at` timestamp (renewal semantics) rather than being
    /// rejected.
    #[tokio::test]
    async fn same_holder_reacquire_renews_lease() {
        let r = LeaseRegistry::new();
        r.try_acquire("foo", "actor-A", Duration::from_millis(50))
            .await
            .unwrap();
        let first = r.snapshot().await[0].acquired_at;
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Same-holder reacquire should succeed (and bump the timestamp).
        let h = r
            .try_acquire("foo", "actor-A", Duration::from_millis(50))
            .await
            .unwrap();
        assert!(
            h.acquired_at > first,
            "renewal must refresh acquired_at; first={:?} new={:?}",
            first,
            h.acquired_at
        );
    }

    /// #153 L6 regression: `acquire_with_wait` must wake on
    /// `release` rather than poll-to-deadline.
    #[tokio::test]
    async fn acquire_with_wait_wakes_on_release() {
        let r = std::sync::Arc::new(LeaseRegistry::new());
        r.try_acquire("foo", "actor-A", Duration::from_secs(30))
            .await
            .unwrap();
        let r2 = r.clone();
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(r2.release("foo", "actor-A").await);
        });
        let start = Instant::now();
        let h = r
            .acquire_with_wait(
                "foo",
                "actor-B",
                Duration::from_secs(30),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        let elapsed = start.elapsed();
        assert_eq!(h.holder, "actor-B");
        assert!(
            elapsed < Duration::from_millis(500),
            "should wake promptly on release, took {:?}",
            elapsed
        );
        releaser.await.unwrap();
    }

    /// #153 L6: `acquire_with_wait` must time out cleanly if no release
    /// happens within `wait`, returning Err(current_holder).
    #[tokio::test]
    async fn acquire_with_wait_times_out_when_held() {
        let r = LeaseRegistry::new();
        r.try_acquire("foo", "actor-A", Duration::from_secs(30))
            .await
            .unwrap();
        let err = r
            .acquire_with_wait(
                "foo",
                "actor-B",
                Duration::from_secs(30),
                Duration::from_millis(50),
            )
            .await
            .unwrap_err();
        assert_eq!(err, "actor-A");
    }

    /// #153 L6: `acquire_with_wait` must wake on bulk
    /// `release_all_held_by`.
    #[tokio::test]
    async fn acquire_with_wait_wakes_on_release_all() {
        let r = std::sync::Arc::new(LeaseRegistry::new());
        r.try_acquire("foo", "actor-A", Duration::from_secs(30))
            .await
            .unwrap();
        let r2 = r.clone();
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            r2.release_all_held_by("actor-A").await;
        });
        let h = r
            .acquire_with_wait(
                "foo",
                "actor-B",
                Duration::from_secs(30),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(h.holder, "actor-B");
        releaser.await.unwrap();
    }
}
