use tokio::sync::watch;

use crate::store::TerminateReason;

/// Two-phase cancel signal shared across tasks.
#[derive(Clone)]
pub struct CancelToken {
    drain_tx: watch::Sender<bool>,
    drain_rx: watch::Receiver<bool>,
    terminate_tx: watch::Sender<bool>,
    terminate_rx: watch::Receiver<bool>,
    /// Carries the operator-readable reason an in-flight termination was
    /// initiated. Set by `terminate_with_reason()` *before* the boolean
    /// terminate signal flips so any observer racing on terminate has the
    /// reason available by the time they read it. Threaded into
    /// `TaskRecord::terminate_reason` at finalize so the failures dashboard
    /// can distinguish "the dispatcher killed me" from "the subprocess
    /// failed on its own". (#475)
    reason_tx: watch::Sender<Option<TerminateReason>>,
    reason_rx: watch::Receiver<Option<TerminateReason>>,
}

impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        let (drain_tx, drain_rx) = watch::channel(false);
        let (terminate_tx, terminate_rx) = watch::channel(false);
        let (reason_tx, reason_rx) = watch::channel(None);
        Self {
            drain_tx,
            drain_rx,
            terminate_tx,
            terminate_rx,
            reason_tx,
            reason_rx,
        }
    }

    pub fn drain(&self) {
        let _ = self.drain_tx.send(true);
    }

    /// Fire the terminate signal without recording a reason. Prefer
    /// [`Self::terminate_with_reason`] in new code so the kill audit
    /// trail reaches `TaskRecord::terminate_reason`. Retained for the
    /// shutdown-cascade path where the reason is already set on the
    /// parent token and `cascade_to` will propagate it. (#475)
    pub fn terminate(&self) {
        let _ = self.terminate_tx.send(true);
    }

    /// Fire the terminate signal and record the reason. Every dispatcher-
    /// initiated kill path (budget watchdog, operator Ctrl-C, parent
    /// cancel cascade, MCP `terminate_*`, runtime timeout, health check)
    /// should call this instead of [`Self::terminate`] so the operator-
    /// facing audit trail names the kill site. Setting the reason BEFORE
    /// the boolean signal prevents an observer racing on terminate from
    /// reading `None`. (#475)
    pub fn terminate_with_reason(&self, reason: TerminateReason) {
        let _ = self.reason_tx.send(Some(reason));
        let _ = self.terminate_tx.send(true);
    }

    /// Snapshot the currently-recorded terminate reason. `None` when no
    /// reason was ever set (either because the token was terminated via
    /// the legacy `terminate()` path or because no kill has occurred).
    /// Reads from a `watch::Receiver`, so this is lock-free and safe to
    /// call from the finalize hot path. (#475)
    #[must_use]
    pub fn terminate_reason(&self) -> Option<TerminateReason> {
        self.reason_rx.borrow().clone()
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
    /// token is terminated, terminate `child` and propagate any recorded
    /// reason (so a budget-breach kill at the root reaches every sub-tree
    /// actor's `TaskRecord::terminate_reason`). Otherwise if this token
    /// is draining, drain `child`. No-op if neither.
    ///
    /// Terminate dominates drain — when both are set on the parent, the
    /// child receives terminate so the more permissive signal is never
    /// applied while the stricter one is in flight. This matches the
    /// semantics used by the eager-propagation paths in
    /// `pitboss-cli::dispatch` (sub-lead spawn after root cancel; worker
    /// registration in a cancelled sub-tree).
    pub fn cascade_to(&self, child: &CancelToken) {
        if self.is_terminated() {
            if let Some(reason) = self.terminate_reason() {
                child.terminate_with_reason(reason);
            } else {
                child.terminate();
            }
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

    // ── #475: terminate_reason audit trail ──────────────────────────────

    #[test]
    fn terminate_with_reason_records_reason() {
        let t = CancelToken::new();
        assert_eq!(t.terminate_reason(), None);
        t.terminate_with_reason(TerminateReason::BudgetBreach {
            detail: Some("over by $0.50".into()),
        });
        assert!(t.is_terminated());
        match t.terminate_reason() {
            Some(TerminateReason::BudgetBreach { detail }) => {
                assert_eq!(detail.as_deref(), Some("over by $0.50"));
            }
            other => panic!("expected BudgetBreach, got {other:?}"),
        }
    }

    #[test]
    fn legacy_terminate_leaves_reason_none() {
        // Back-compat: code paths that call the legacy `terminate()` must
        // not be coerced into None-vs-some confusion at the consumer.
        let t = CancelToken::new();
        t.terminate();
        assert!(t.is_terminated());
        assert_eq!(t.terminate_reason(), None);
    }

    #[test]
    fn cascade_propagates_reason_to_child() {
        let parent = CancelToken::new();
        let child = CancelToken::new();
        parent.terminate_with_reason(TerminateReason::OperatorCtrlC);
        parent.cascade_to(&child);
        assert!(child.is_terminated());
        assert_eq!(
            child.terminate_reason(),
            Some(TerminateReason::OperatorCtrlC)
        );
    }

    #[test]
    fn cascade_with_no_reason_terminates_child_without_reason() {
        // Legacy path: parent terminated via `terminate()` (no reason).
        // Child still gets terminated, just with no recorded reason.
        let parent = CancelToken::new();
        let child = CancelToken::new();
        parent.terminate();
        parent.cascade_to(&child);
        assert!(child.is_terminated());
        assert_eq!(child.terminate_reason(), None);
    }
}
