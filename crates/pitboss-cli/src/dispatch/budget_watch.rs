//! Live-spend reconciliation for leads and sub-leads.
//!
//! Issue #253 extended `[lead].budget_usd` from a workers-only soft cap
//! into a total-run cap that also counts the lead's and any sub-leads'
//! own token spend, with an optional separate `lead_budget_usd` cap that
//! applies only to orchestration cost. To deliver the kill-switch
//! acceptance criterion ("a lead burning past `budget_usd` triggers run
//! abort"), we cannot wait for terminal subprocess exit to reconcile —
//! a long-running lead typically lives in one subprocess for hours.
//!
//! The parser surfaces per-assistant-turn cumulative usage as
//! [`pitboss_core::parser::Event::AssistantUsage`], the session handle
//! forwards it through its [`UsageObserver`], and this module supplies
//! the observer the dispatcher installs. The observer:
//!
//! 1. Converts cumulative `TokenUsage` for the current subprocess to
//!    USD via [`pitboss_core::prices::cost_usd`].
//! 2. Atomically writes `layer.budget.lead_spent_usd = baseline + cost`, where
//!    `baseline` is the running total committed across prior kill+resume
//!    iterations.
//! 3. Computes the run-wide total (workers + lead + sub-leads across
//!    every layer) and checks both `budget_usd` and the optional
//!    `lead_budget_usd` cap. On breach, it stamps
//!    `layer.budget.abort_reason` so `failure_detection` can name the
//!    overspend and then fires `layer.cancel.terminate()` to kill the
//!    in-flight subprocess.
//!
//! Between iterations the kill+resume loop commits the iteration's
//! final cost into `baseline` via [`commit_iteration`], so the next
//! iteration's observer starts from the right zero.

use std::sync::{Arc, Mutex};

use pitboss_core::parser::TokenUsage;
use pitboss_core::prices;
use pitboss_core::session::UsageObserver;

use crate::dispatch::layer::LayerState;
use crate::dispatch::state::DispatchState;

/// Per-actor (root lead or sub-lead) live-spend tracking state shared
/// between the kill+resume loop and the observer it installs into each
/// iteration's `SessionHandle`.
#[derive(Default)]
pub struct LeadSpendBaseline {
    /// Accumulated cost across prior kill+resume iterations. The
    /// observer writes `layer.budget.lead_spent_usd = baseline + this_iter_cost`
    /// on every assistant-turn usage update.
    pub baseline_usd: Mutex<f64>,
}

impl LeadSpendBaseline {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    // TODO(#253 follow-up): on `pitboss resume`, rehydrate the baseline
    // from the prior run's `summary.jsonl` so a resumed lead doesn't
    // restart with a $0 budget envelope against the original cap. The
    // current implementation mirrors the existing gap for
    // `layer.budget.spent_usd` (workers) — neither is restored on resume — so
    // this parallel gap is "fix both together" rather than "regressed
    // by this PR." Tracked as part of #259 (persistent control event
    // stream replay).
}

/// Snapshot of the run-wide spend, broken down by class. Computed by
/// [`compute_total_spend`] for budget checks and surfaced into status
/// outputs / `summary.json` so operators can see where their cost went.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpendBreakdown {
    /// Sum of `spent_usd` (committed worker cost) across the root
    /// layer and every live + terminated sub-lead layer.
    pub workers_usd: f64,
    /// Root lead's own token cost. Reflects the live observer reading
    /// when a subprocess is in flight, or the committed baseline when
    /// the lead is idle between iterations.
    pub lead_usd: f64,
    /// Sum of every sub-lead's own token cost (live + terminated
    /// sub-trees).
    pub subleads_usd: f64,
}

impl SpendBreakdown {
    #[must_use]
    pub fn total_usd(&self) -> f64 {
        self.workers_usd + self.lead_usd + self.subleads_usd
    }
}

/// Walk every layer in the run (root + live sub-leads + terminated
/// sub-leads) and assemble the current spend totals.
pub async fn compute_total_spend(state: &DispatchState) -> SpendBreakdown {
    let workers_root = *state.root.budget.spent_usd.lock().await;
    let lead_usd = *state
        .root
        .budget
        .lead_spent_usd
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let mut workers_subs = 0.0_f64;
    let mut subleads_usd = 0.0_f64;
    {
        let live = state.subleads.read().await;
        for layer in live.values() {
            workers_subs += *layer.budget.spent_usd.lock().await;
            subleads_usd += *layer
                .budget
                .lead_spent_usd
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
    {
        let terminated = state.terminated_sublead_layers.read().await;
        for layer in terminated.iter() {
            workers_subs += *layer.budget.spent_usd.lock().await;
            subleads_usd += *layer
                .budget
                .lead_spent_usd
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    SpendBreakdown {
        workers_usd: workers_root + workers_subs,
        lead_usd,
        subleads_usd,
    }
}

/// Synchronous variant of [`compute_total_spend`]. Used inside the
/// usage observer, which runs from a non-async context inside the
/// session stream loop. Uses `try_lock` on the tokio mutexes — if a
/// `worker_status` MCP call happens to be reading worker spend at the
/// same instant the contention is benign and the next observer fire
/// gets a fresh reading.
fn compute_total_spend_blocking(state: &DispatchState) -> SpendBreakdown {
    let workers_root = state
        .root
        .budget
        .spent_usd
        .try_lock()
        .map(|g| *g)
        .unwrap_or(0.0);
    let lead_usd = *state
        .root
        .budget
        .lead_spent_usd
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let mut workers_subs = 0.0_f64;
    let mut subleads_usd = 0.0_f64;
    if let Ok(live) = state.subleads.try_read() {
        for layer in live.values() {
            workers_subs += layer.budget.spent_usd.try_lock().map(|g| *g).unwrap_or(0.0);
            subleads_usd += *layer
                .budget
                .lead_spent_usd
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
    if let Ok(terminated) = state.terminated_sublead_layers.try_read() {
        for layer in terminated.iter() {
            workers_subs += layer.budget.spent_usd.try_lock().map(|g| *g).unwrap_or(0.0);
            subleads_usd += *layer
                .budget
                .lead_spent_usd
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    SpendBreakdown {
        workers_usd: workers_root + workers_subs,
        lead_usd,
        subleads_usd,
    }
}

/// Build the usage observer that the kill+resume loop installs on every
/// `SessionHandle` iteration for a given lead or sub-lead.
///
/// `layer` is the lead's own layer (root layer for the root lead; the
/// sub-tree layer for a sub-lead). `state` is the run-global
/// `DispatchState` used to walk every layer when checking the
/// run-wide `budget_usd` cap.
pub fn build_lead_usage_observer(
    layer: Arc<LayerState>,
    state: Arc<DispatchState>,
    model: String,
    actor_id: String,
    baseline: Arc<LeadSpendBaseline>,
    budget_usd: Option<f64>,
    lead_budget_usd: Option<f64>,
) -> UsageObserver {
    Arc::new(move |usage: TokenUsage| {
        let this_iter_cost = prices::cost_usd(&model, &usage).unwrap_or(0.0);
        let baseline_usd = *baseline
            .baseline_usd
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lead_total = baseline_usd + this_iter_cost;

        {
            let mut g = layer
                .budget
                .lead_spent_usd
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *g = lead_total;
        }

        // Don't re-fire abort once already triggered (observer can fire
        // many times in quick succession; cancel() is idempotent but the
        // reason stamp would race).
        if layer
            .budget
            .abort_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
        {
            return;
        }

        let breakdown = compute_total_spend_blocking(&state);

        // Two independent caps. `lead_budget_usd` constrains the
        // orchestration cost (lead + sub-lead self spend); `budget_usd`
        // constrains the run-wide total.
        let lead_self_total = breakdown.lead_usd + breakdown.subleads_usd;
        if let Some(cap) = lead_budget_usd {
            if lead_self_total > cap {
                trip_budget_abort(
                    &layer,
                    format!(
                        "lead+sublead spend {lead_self_total:.4} exceeded \
                         lead_budget_usd cap {cap:.4} (actor: {actor_id})"
                    ),
                );
                return;
            }
        }
        if let Some(cap) = budget_usd {
            let total = breakdown.total_usd();
            if total > cap {
                trip_budget_abort(
                    &layer,
                    format!(
                        "total run spend {total:.4} exceeded budget_usd cap \
                         {cap:.4} (lead actor: {actor_id})"
                    ),
                );
            }
        }
    })
}

fn trip_budget_abort(layer: &LayerState, reason: String) {
    {
        let mut g = layer
            .budget
            .abort_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *g = Some(reason.clone());
    }
    tracing::warn!(reason = %reason, "budget_watch: tripping cancel on lead layer");
    // #475: name the kill site on the cancel token so every actor below
    // this layer surfaces `terminate_reason: BudgetBreach` on its
    // TaskRecord rather than the bare "I got killed somehow" signal.
    layer
        .cancel
        .terminate_with_reason(pitboss_core::store::TerminateReason::BudgetBreach {
            detail: Some(reason),
        });
}

/// After a kill+resume iteration ends, fold its committed cost into
/// the baseline so the next iteration's observer starts from the
/// updated zero. Called by the kill+resume loop with the final per-
/// iteration `TokenUsage` from `SessionOutcome`.
pub fn commit_iteration(baseline: &LeadSpendBaseline, model: &str, iteration_usage: &TokenUsage) {
    let cost = prices::cost_usd(model, iteration_usage).unwrap_or(0.0);
    let mut g = baseline
        .baseline_usd
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *g += cost;
}

#[cfg(test)]
mod tests {
    //! Direct tests for the budget watcher's kill-switch behavior (#253).
    //! These don't drive a full kill+resume subprocess loop — they
    //! instantiate the observer in isolation and fire it with synthetic
    //! cumulative `TokenUsage` values that simulate a lead burning past
    //! its cap mid-turn. The acceptance criterion from #253 is that
    //! observing a `TokenUsage` whose priced cost pushes the run past
    //! `budget_usd` (a) stamps `budget_abort_reason` with a message
    //! that names the overspend and (b) terminates the layer's cancel
    //! token so the in-flight subprocess is killed.

    use super::*;
    use crate::dispatch::state::DispatchState;
    use crate::test_support::state_builder::TestStateBuilder;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Build a minimal `DispatchState` for an Opus root lead with the
    /// supplied caps. Opus pricing is the highest in the table — keeps
    /// the synthetic token counts in the test small enough to read.
    fn mk_state_with_caps(
        budget_usd: Option<f64>,
        lead_budget_usd: Option<f64>,
    ) -> (TempDir, Arc<DispatchState>) {
        let mut b = TestStateBuilder::new()
            .with_lead()
            .lead_model("claude-opus-4-7");
        b = match budget_usd {
            Some(usd) => b.budget(usd),
            None => b.no_budget(),
        };
        if let Some(usd) = lead_budget_usd {
            b = b.lead_budget(usd);
        }
        b.build()
    }

    fn usage(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input,
            output,
            cache_read: 0,
            cache_creation: 0,
        }
    }

    /// Acceptance test (#253): a lead's cumulative usage that prices
    /// past `budget_usd` trips the run abort. The observer stamps
    /// `budget_abort_reason` and fires the layer's cancel token.
    #[tokio::test]
    async fn lead_usage_past_budget_usd_trips_abort_and_reason() {
        // Opus rates (in pitboss-core::prices): $15/M input, $75/M output.
        // 100_000 input + 100_000 output = $1.50 + $7.50 = $9.00 — well
        // past a $1.00 cap.
        let (_dir, state) = mk_state_with_caps(Some(1.00), None);
        let layer = state.root.clone();
        assert!(!layer.cancel.is_terminated());
        assert!(layer.budget.abort_reason.lock().unwrap().is_none());

        let baseline = LeadSpendBaseline::new();
        let obs = build_lead_usage_observer(
            layer.clone(),
            Arc::clone(&state),
            "claude-opus-4-7".into(),
            "lead".into(),
            Arc::clone(&baseline),
            state.root.manifest.budget_usd,
            state.root.manifest.lead_budget_usd,
        );

        // Fire the observer with usage that exceeds the cap.
        obs(usage(100_000, 100_000));

        let reason = layer.budget.abort_reason.lock().unwrap().clone();
        assert!(
            reason.is_some(),
            "expected budget_abort_reason set after overspend"
        );
        let msg = reason.unwrap();
        assert!(
            msg.contains("budget_usd"),
            "abort reason should name the cap: {msg}"
        );
        assert!(
            msg.contains("total run spend"),
            "abort reason should describe what tripped: {msg}"
        );
        assert!(
            layer.cancel.is_terminated(),
            "layer cancel should be terminated after budget abort"
        );
        // #475: the budget watcher names the kill site via
        // `terminate_with_reason(BudgetBreach)` so any downstream
        // TaskRecord built off this cancel token surfaces the audit
        // trail instead of bare Cancelled.
        match layer.cancel.terminate_reason() {
            Some(pitboss_core::store::TerminateReason::BudgetBreach { detail }) => {
                assert!(
                    detail.as_deref().is_some_and(|d| d.contains("budget_usd")),
                    "BudgetBreach detail should name the cap: {detail:?}"
                );
            }
            other => panic!("expected TerminateReason::BudgetBreach, got {other:?}"),
        }
    }

    /// `lead_budget_usd` (the orchestration-only cap) trips
    /// independently of `budget_usd`. The same overspend at a tighter
    /// orchestration cap stamps a reason that names the right cap.
    #[tokio::test]
    async fn lead_budget_usd_trips_independently() {
        // budget_usd is generous; lead_budget_usd is tight.
        let (_dir, state) = mk_state_with_caps(Some(100.0), Some(1.00));
        let layer = state.root.clone();
        let baseline = LeadSpendBaseline::new();
        let obs = build_lead_usage_observer(
            layer.clone(),
            Arc::clone(&state),
            "claude-opus-4-7".into(),
            "lead".into(),
            Arc::clone(&baseline),
            state.root.manifest.budget_usd,
            state.root.manifest.lead_budget_usd,
        );
        obs(usage(100_000, 100_000));
        let reason = layer.budget.abort_reason.lock().unwrap().clone();
        let msg = reason.expect("expected reason set");
        assert!(
            msg.contains("lead_budget_usd"),
            "reason should name the orchestration cap: {msg}"
        );
        assert!(layer.cancel.is_terminated());
    }

    /// Usage well under both caps must not trip the abort.
    #[tokio::test]
    async fn under_budget_observer_is_noop() {
        let (_dir, state) = mk_state_with_caps(Some(100.0), Some(10.0));
        let layer = state.root.clone();
        let baseline = LeadSpendBaseline::new();
        let obs = build_lead_usage_observer(
            layer.clone(),
            Arc::clone(&state),
            "claude-opus-4-7".into(),
            "lead".into(),
            Arc::clone(&baseline),
            state.root.manifest.budget_usd,
            state.root.manifest.lead_budget_usd,
        );
        // 1k+1k tokens at Opus = 1k * $15/M + 1k * $75/M ≈ $0.09 — far
        // below either cap.
        obs(usage(1_000, 1_000));
        assert!(layer.budget.abort_reason.lock().unwrap().is_none());
        assert!(!layer.cancel.is_terminated());
        // lead_spent_usd should reflect the priced cost.
        let live = *layer.budget.lead_spent_usd.lock().unwrap();
        assert!(live > 0.0 && live < 1.0, "live spend out of range: {live}");
    }

    /// `commit_iteration` folds the iteration's committed cost into the
    /// baseline so the next iteration starts from the right zero.
    #[tokio::test]
    async fn commit_iteration_advances_baseline() {
        let baseline = LeadSpendBaseline::new();
        commit_iteration(&baseline, "claude-opus-4-7", &usage(10_000, 10_000));
        let after = *baseline.baseline_usd.lock().unwrap();
        // 10k input * $15/M + 10k output * $75/M = $0.15 + $0.75 = $0.90
        assert!(
            (after - 0.90).abs() < 1e-6,
            "baseline after one iteration should be $0.90, got {after}"
        );
    }
}
