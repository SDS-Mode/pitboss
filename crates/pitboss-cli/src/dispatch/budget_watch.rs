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
/// [`compute_total_spend`] for budget checks.
///
/// The bookkeeping is asymmetric across the sub-lead lifecycle because
/// the storage layout is asymmetric: `dispatch::sublead::reconcile_terminated_sublead`
/// rolls a terminated sub-tree's `sub.budget.spent_usd` (which itself
/// contains workers + the sublead's own session cost) into
/// `root.budget.spent_usd`, but does NOT roll `sub.budget.lead_spent_usd`
/// into anything. So after reconcile:
///
/// * `root.spent_usd` carries the rolled-up sub-tree totals (workers +
///   sublead-self). Reading the same `sub.spent_usd` again from
///   `terminated_sublead_layers` double-counts it. That was the
///   ~2× budget-trip bug fixed here.
///
/// * `sub.lead_spent_usd` on a terminated sub-layer represents the
///   sublead's own session cost — already inside the `sub.spent_usd`
///   that got rolled into `root.spent_usd`. So adding it to the run-wide
///   total again would TRIPLE-count those dollars. But it's still
///   needed for the `[lead].lead_budget_usd` orchestration cap check
///   (which separately sums root-lead-self + every sub-lead-self), so
///   we keep it in `subleads_usd` and exclude it from [`Self::total_usd`].
///
/// * For LIVE sub-leads (still running), `sub.spent_usd` does not yet
///   include the sublead's own session cost — that's added once at
///   session end by `dispatch::sublead`'s exit handler. The live
///   sublead-self spend lives only in `sub.lead_spent_usd`. We capture
///   it in `live_subleads_self_usd` so [`Self::total_usd`] adds it on
///   top of `workers_usd`, which only has workers for live sub-trees.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpendBreakdown {
    /// Root layer's `spent_usd` (direct root workers + rolled-up
    /// terminated sub-trees) plus each LIVE sub-tree's `spent_usd`
    /// (workers only — sublead-self not yet committed there).
    pub workers_usd: f64,
    /// Root lead's own token cost. Reflects the live observer reading
    /// when a subprocess is in flight, or the committed baseline when
    /// the lead is idle between iterations.
    pub lead_usd: f64,
    /// Sum of every sub-lead's own session cost across LIVE + TERMINATED
    /// sub-trees. Used by the `lead_budget_usd` orchestration cap check.
    /// For terminated sub-trees this duplicates dollars already in
    /// `workers_usd` (because reconcile rolled `sub.spent_usd` — which
    /// includes the sublead's own session — into `root.spent_usd`); see
    /// [`Self::total_usd`] for the de-duplicated run-wide total.
    pub subleads_usd: f64,
    /// Internal: sub-lead self-spend for LIVE sub-trees only. Used by
    /// [`Self::total_usd`] to add live sublead-self spend (which isn't
    /// in `workers_usd` yet) without re-adding the terminated portion
    /// (which is already in `workers_usd` via the reconcile roll-up).
    pub live_subleads_self_usd: f64,
}

impl SpendBreakdown {
    /// Run-wide total spend with no double-counting.
    ///
    /// Equals `root.spent_usd + Σ live.spent_usd + Σ live.lead_spent_usd
    /// + root.lead_spent_usd`. Excludes `subleads_usd`'s terminated
    /// component because those dollars are already in `workers_usd` via
    /// the reconcile roll-up (see [`SpendBreakdown`]'s docstring).
    #[must_use]
    pub fn total_usd(&self) -> f64 {
        self.workers_usd + self.lead_usd + self.live_subleads_self_usd
    }
}

/// Walk every layer in the run (root + live sub-leads + terminated
/// sub-leads) and assemble the current spend totals. See [`SpendBreakdown`]
/// for the asymmetric accumulator semantics that motivate skipping
/// `terminated_sublead_layers` from the worker-cost accumulator while
/// still summing them for the orchestration-cost (`subleads_usd`) bucket.
pub async fn compute_total_spend(state: &DispatchState) -> SpendBreakdown {
    let workers_root = state.root.budget.spent_usd.load();
    let lead_usd = state.root.budget.lead_spent_usd.load();

    let mut workers_subs = 0.0_f64;
    let mut subleads_usd = 0.0_f64;
    let mut live_subleads_self_usd = 0.0_f64;
    {
        let live = state.subleads.read().await;
        for layer in live.values() {
            workers_subs += layer.budget.spent_usd.load();
            let self_spend = layer.budget.lead_spent_usd.load();
            subleads_usd += self_spend;
            live_subleads_self_usd += self_spend;
        }
    }
    {
        // Terminated sub-trees: do NOT add `spent_usd` to workers_subs
        // (already rolled into `workers_root` at reconcile time). DO
        // add `lead_spent_usd` to `subleads_usd` for the orchestration
        // cap check; it's accounted in workers_root via the roll-up
        // but `lead_budget_usd` needs the orchestration-only subtotal.
        let terminated = state.terminated_sublead_layers.read().await;
        for layer in terminated.iter() {
            subleads_usd += layer.budget.lead_spent_usd.load();
        }
    }

    SpendBreakdown {
        workers_usd: workers_root + workers_subs,
        lead_usd,
        subleads_usd,
        live_subleads_self_usd,
    }
}

/// Synchronous variant of [`compute_total_spend`]. Used inside the
/// usage observer, which runs from a non-async context inside the
/// session stream loop. `spent_usd` and `lead_spent_usd` are
/// `BudgetCounter`s (atomic f64) so per-layer reads never block; the
/// remaining `try_read()` on `state.subleads` and
/// `state.terminated_sublead_layers` is a tokio `RwLock` and falls back
/// to a partial sum on contention with a concurrent `register_sublead`
/// / `reconcile_terminated_sublead`. Those writes are rare (one per
/// sub-lead lifecycle event) so the next observer fire gets a fresh
/// reading. (F-ARCH-10)
fn compute_total_spend_blocking(state: &DispatchState) -> SpendBreakdown {
    let workers_root = state.root.budget.spent_usd.load();
    let lead_usd = state.root.budget.lead_spent_usd.load();

    let mut workers_subs = 0.0_f64;
    let mut subleads_usd = 0.0_f64;
    let mut live_subleads_self_usd = 0.0_f64;
    if let Ok(live) = state.subleads.try_read() {
        for layer in live.values() {
            workers_subs += layer.budget.spent_usd.load();
            let self_spend = layer.budget.lead_spent_usd.load();
            subleads_usd += self_spend;
            live_subleads_self_usd += self_spend;
        }
    }
    if let Ok(terminated) = state.terminated_sublead_layers.try_read() {
        // Terminated sub-trees: do NOT add `spent_usd` to workers_subs
        // (already rolled into `workers_root` at reconcile time). See
        // [`SpendBreakdown`] for the asymmetric rationale.
        for layer in terminated.iter() {
            subleads_usd += layer.budget.lead_spent_usd.load();
        }
    }

    SpendBreakdown {
        workers_usd: workers_root + workers_subs,
        lead_usd,
        subleads_usd,
        live_subleads_self_usd,
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

        layer.budget.lead_spent_usd.store(lead_total);

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
        let live = layer.budget.lead_spent_usd.load();
        assert!(live > 0.0 && live < 1.0, "live spend out of range: {live}");
    }

    /// Regression: post-reconcile, `compute_total_spend_blocking` must not
    /// double-count `sub.spent_usd` (already rolled into `root.spent_usd`)
    /// nor triple-count the sublead-self portion within `subleads_usd`.
    ///
    /// Simulates the exact state that tripped a $4 cap at $4.71 in
    /// validation run `019e4b2b-ad23-7eb3-b4b2-ce6eb7c3cdad`: two
    /// reconciled subleads each contributing $0.81 / $0.83 of (workers +
    /// self) to root.spent_usd, with their sub-layer accumulators still
    /// populated in terminated_sublead_layers.
    #[tokio::test]
    async fn terminated_subleads_do_not_double_count_in_total_usd() {
        use crate::dispatch::layer::LayerState;
        use std::sync::Arc;
        // Build a state — caps unused; we drive the accumulators directly.
        let (_dir, state) = mk_state_with_caps(Some(4.0), Some(2.0));

        // Simulate reconcile_terminated_sublead's roll-up: root.spent_usd
        // gets += sub.spent_usd for both subleads. Use the exact figures
        // from the buggy run.
        const SUB_A_SPENT: f64 = 0.81389724; // workers + sublead-self
        const SUB_B_SPENT: f64 = 0.82939324;
        state.root.budget.spent_usd.store(SUB_A_SPENT + SUB_B_SPENT);

        // Build two terminated sub-layer doppelgangers carrying the same
        // figures in their own accumulators (state after sublead.rs:914
        // ran but before reconcile removed them from the live map).
        // Reuse the root layer's Arc (which has its own LayerState) and
        // override the relevant fields — for the cap-arithmetic test we
        // only need the budget accumulators populated.
        let make_term_layer = |spent: f64, lead_spent: f64| -> Arc<LayerState> {
            let layer: Arc<LayerState> = TestStateBuilder::new().with_lead().build_layer().1.into();
            layer.budget.spent_usd.store(spent);
            layer.budget.lead_spent_usd.store(lead_spent);
            layer
        };

        // For each terminated sublead, lead_spent_usd ≈ $0.69 / $0.73
        // (the orchestration-cost portion that's also inside spent_usd).
        const SUB_A_LEAD: f64 = 0.69;
        const SUB_B_LEAD: f64 = 0.73;
        let term_a = make_term_layer(SUB_A_SPENT, SUB_A_LEAD);
        let term_b = make_term_layer(SUB_B_SPENT, SUB_B_LEAD);
        {
            let mut t = state.terminated_sublead_layers.write().await;
            t.push(term_a);
            t.push(term_b);
        }

        // Tiny root lead self-spend, matching the buggy run.
        const ROOT_LEAD_SELF: f64 = 0.004;
        state.root.budget.lead_spent_usd.store(ROOT_LEAD_SELF);

        let breakdown = compute_total_spend_blocking(&state);

        // workers_usd = root.spent_usd (no live subleads).
        let expected_workers_usd = SUB_A_SPENT + SUB_B_SPENT;
        assert!(
            (breakdown.workers_usd - expected_workers_usd).abs() < 1e-9,
            "workers_usd should reflect ONLY root.spent_usd post-reconcile (got {})",
            breakdown.workers_usd
        );

        // subleads_usd keeps the terminated lead_spent_usd for the
        // lead_budget_usd cap check.
        let expected_subleads_usd = SUB_A_LEAD + SUB_B_LEAD;
        assert!(
            (breakdown.subleads_usd - expected_subleads_usd).abs() < 1e-9,
            "subleads_usd should sum terminated lead_spent_usd (got {})",
            breakdown.subleads_usd
        );

        // The key assertion: total_usd MUST NOT include subleads_usd
        // post-reconcile (those dollars are already in workers_usd).
        let expected_total = expected_workers_usd + ROOT_LEAD_SELF;
        assert!(
            (breakdown.total_usd() - expected_total).abs() < 1e-9,
            "total_usd should be workers_usd + lead_usd only (terminated subleads' \
             self-spend already inside workers_usd) — expected {expected_total}, got {}",
            breakdown.total_usd()
        );
        // And it must NOT trip the $4 cap (actual run total was $1.65).
        assert!(
            breakdown.total_usd() < 4.0,
            "total $4.0 cap must not trip on real $1.65 spend; got {}",
            breakdown.total_usd()
        );
    }

    /// Live (in-flight) subleads contribute their `lead_spent_usd` to
    /// `total_usd` via `live_subleads_self_usd` because `sub.spent_usd`
    /// does not yet contain the sublead's own session cost — that's
    /// only added once at session end by `dispatch::sublead`'s exit
    /// handler. Without this term, mid-run total would under-count
    /// every live sublead's orchestration spend.
    #[tokio::test]
    async fn live_sublead_self_spend_counted_in_total_usd() {
        use crate::dispatch::layer::LayerState;
        use std::sync::Arc;
        let (_dir, state) = mk_state_with_caps(Some(4.0), Some(2.0));

        // One live sublead: workers contributed $0.10 to sub.spent_usd
        // (via spawn.rs:835 on worker finish); the sublead is still
        // running so its own session cost (the live observer's writes
        // to sub.lead_spent_usd) is $0.50 — NOT yet rolled into
        // sub.spent_usd.
        let live_layer: Arc<LayerState> =
            TestStateBuilder::new().with_lead().build_layer().1.into();
        live_layer.budget.spent_usd.store(0.10);
        live_layer.budget.lead_spent_usd.store(0.50);
        state
            .subleads
            .write()
            .await
            .insert("live-sub".to_string(), live_layer);

        let breakdown = compute_total_spend_blocking(&state);
        // Expected: workers $0.10 + sublead-self $0.50 + root $0 = $0.60
        assert!(
            (breakdown.total_usd() - 0.60).abs() < 1e-9,
            "live sublead-self must be added to total_usd; got {}",
            breakdown.total_usd()
        );
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

    /// End-to-end pipeline test (F-TEST-3): FakeSpawner emits stream-json
    /// `assistant_usage` lines whose cumulative cost exceeds `budget_usd`,
    /// the parser surfaces `AssistantUsage`, the usage observer prices it,
    /// and the layer's cancel token terminates mid-stream — all without
    /// hand-calling `obs(usage)`. Closes the audit's "no integration test
    /// exercises the full pipeline from stream-json output through
    /// budget-trip to cancel" gap.
    #[tokio::test]
    async fn fakespawner_overspend_trips_cancel_through_full_pipeline() {
        use pitboss_core::process::fake::{FakeScript, FakeSpawner};
        use pitboss_core::process::{ProcessSpawner, SpawnCmd};
        use pitboss_core::session::SessionHandle;
        use std::time::Duration;

        // Tight $0.50 cap; one Opus turn at 50k input + 50k output prices to
        // $0.75 + $3.75 = $4.50, well past the cap.
        let (_dir, state) = mk_state_with_caps(Some(0.50), None);
        let layer = state.root.clone();
        let baseline = LeadSpendBaseline::new();

        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init","session_id":"sess-trip"}"#)
            .assistant_usage("burning tokens", usage(50_000, 50_000))
            .result_event("sess-trip", usage(50_000, 50_000))
            .exit_code(0);
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let cmd = SpawnCmd {
            program: std::path::PathBuf::from("fake-claude"),
            args: vec![],
            cwd: std::path::PathBuf::from("/tmp"),
            env: std::collections::HashMap::new(),
        };
        let observer = build_lead_usage_observer(
            layer.clone(),
            Arc::clone(&state),
            "claude-opus-4-7".into(),
            "lead".into(),
            Arc::clone(&baseline),
            state.root.manifest.budget_usd,
            state.root.manifest.lead_budget_usd,
        );
        let handle = SessionHandle::new("lead", spawner, cmd).with_usage_observer(observer);
        let cancel = layer.cancel.clone();
        let _outcome = handle
            .run_to_completion(cancel, Duration::from_millis(100))
            .await;

        assert!(
            layer.cancel.is_terminated(),
            "cancel token should be terminated after parser-driven budget trip"
        );
        let reason = layer.budget.abort_reason.lock().unwrap().clone();
        let msg = reason.expect("abort reason should be stamped");
        assert!(
            msg.contains("budget_usd"),
            "abort reason should name the cap: {msg}"
        );
    }
}
