use tokio::sync::watch;

/// Two-phase cancel signal shared across tasks.
#[derive(Clone)]
pub struct CancelToken {
    drain_tx: watch::Sender<bool>,
    drain_rx: watch::Receiver<bool>,
    terminate_tx: watch::Sender<bool>,
    terminate_rx: watch::Receiver<bool>,
}

impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        let (drain_tx, drain_rx) = watch::channel(false);
        let (terminate_tx, terminate_rx) = watch::channel(false);
        Self {
            drain_tx,
            drain_rx,
            terminate_tx,
            terminate_rx,
        }
    }

    pub fn drain(&self) {
        let _ = self.drain_tx.send(true);
    }

    pub fn terminate(&self) {
        let _ = self.terminate_tx.send(true);
    }

    #[must_use]
    pub fn is_draining(&self) -> bool {
        *self.drain_rx.borrow()
    }

    #[must_use]
    pub fn is_terminated(&self) -> bool {
        *self.terminate_rx.borrow()
    }

    /// Async wait for drain signal. Returns immediately if already draining.
    pub async fn await_drain(&self) {
        let mut rx = self.drain_rx.clone();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }

    /// Async wait for terminate signal.
    pub async fn await_terminate(&self) {
        let mut rx = self.terminate_rx.clone();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }

    /// Propagate this token's in-flight cancel state to `child`. If this
    /// token is terminated, terminate `child`. Otherwise if this token is
    /// draining, drain `child`. No-op if neither.
    ///
    /// Terminate dominates drain — when both are set on the parent, the
    /// child receives terminate so the more permissive signal is never
    /// applied while the stricter one is in flight. This matches the
    /// semantics used by the eager-propagation paths in
    /// `pitboss-cli::dispatch` (sub-lead spawn after root cancel; worker
    /// registration in a cancelled sub-tree).
    pub fn cascade_to(&self, child: &CancelToken) {
        if self.is_terminated() {
            child.terminate();
        } else if self.is_draining() {
            child.drain();
        }
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn drain_signal_fires() {
        let t = CancelToken::new();
        assert!(!t.is_draining());
        let handle = {
            let t = t.clone();
            tokio::spawn(async move { t.await_drain().await })
        };
        tokio::time::advance(Duration::from_millis(10)).await;
        t.drain();
        handle.await.unwrap();
        assert!(t.is_draining());
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn terminate_is_independent_of_drain() {
        let t = CancelToken::new();
        t.terminate();
        assert!(t.is_terminated());
        assert!(!t.is_draining());
    }

    #[test]
    fn cascade_to_pristine_parent_is_noop() {
        let parent = CancelToken::new();
        let child = CancelToken::new();
        parent.cascade_to(&child);
        assert!(!child.is_draining());
        assert!(!child.is_terminated());
    }

    #[test]
    fn cascade_to_drains_child_when_parent_drained() {
        let parent = CancelToken::new();
        let child = CancelToken::new();
        parent.drain();
        parent.cascade_to(&child);
        assert!(child.is_draining());
        assert!(!child.is_terminated());
    }

    #[test]
    fn cascade_to_terminates_child_when_parent_terminated() {
        let parent = CancelToken::new();
        let child = CancelToken::new();
        parent.terminate();
        parent.cascade_to(&child);
        assert!(child.is_terminated());
        // Whether `is_draining` is also true depends on whether
        // terminate implicitly sets drain; we deliberately only assert
        // terminate here because that's the dominant signal.
    }

    #[test]
    fn cascade_to_terminate_dominates_drain() {
        let parent = CancelToken::new();
        let child = CancelToken::new();
        parent.drain();
        parent.terminate();
        parent.cascade_to(&child);
        assert!(child.is_terminated());
        // Crucially, drain is NOT applied to the child when terminate
        // is also set on the parent — the stricter signal wins, so we
        // never tell the child "you may finish current work" while the
        // parent has already said "stop now".
        assert!(!child.is_draining());
    }
}
