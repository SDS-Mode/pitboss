//! `pitboss tree <manifest>` — pre-flight visualisation + cost gate.
//!
//! Static walk over a resolved manifest that prints the dispatch
//! topology — root lead, depth-2 controls, sub-lead defaults, or the
//! flat-mode task list — alongside every per-actor knob the operator
//! is implicitly committing to (model, effort, timeout, worker pool,
//! per-actor budget). Aggregates the worst-case budget envelope so
//! the operator can sanity-check a manifest before firing a $20+ run.
//!
//! `--check <USD>` turns the same walk into a hard gate: exits
//! non-zero if the aggregate exceeds the threshold OR if a required
//! cap (e.g. `max_sublead_budget_usd` when `allow_subleads = true`)
//! is unbounded. Designed for CI use — drop it in front of a `pitboss
//! dispatch` step in a workflow file and the pipeline fails loudly
//! before any spend lands.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};

use crate::manifest::load::load_manifest_from_str;
use crate::manifest::resolve::{ResolvedLead, ResolvedManifest, ResolvedSubleadDefaults};

/// Worst-case aggregate budget envelope for a manifest.
///
/// "Worst case" = assume every sub-lead carves its own pool
/// (`read_down = false`) and the lead spawns up to `max_subleads`
/// of them. `read_down = true` sub-leads share the root's pool, but
/// that's a per-spawn runtime decision; a manifest can't promise it
/// statically, so the gate is conservative.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetEnvelope {
    /// Root lead's `budget_usd`. `None` when the lead has no cap.
    pub root_lead_usd: Option<f64>,
    /// Per-sub-lead envelope multiplied by `max_subleads`. `None`
    /// when sub-leads are disabled OR when the manifest declares
    /// `allow_subleads = true` without both `max_subleads` and
    /// `max_sublead_budget_usd` set.
    pub subleads: Option<SubleadBudget>,
    /// Sum of `root_lead_usd` and `subleads.total_usd`. `None` when
    /// either leg is unbounded — partial sums would mislead the
    /// operator into thinking the cap is lower than it actually is.
    pub total_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubleadBudget {
    pub max_subleads: u32,
    pub per_sublead_usd: f64,
    pub total_usd: f64,
}

impl BudgetEnvelope {
    pub fn from_lead(lead: &ResolvedLead) -> Self {
        let root_lead_usd = lead.budget_usd();
        let subleads = if lead.allow_subleads {
            match (lead.max_subleads, lead.max_sublead_budget_usd) {
                (Some(n), Some(per)) => Some(SubleadBudget {
                    max_subleads: n,
                    per_sublead_usd: per,
                    #[allow(clippy::cast_precision_loss)]
                    total_usd: per * n as f64,
                }),
                _ => None,
            }
        } else {
            // Sub-leads disabled — they contribute zero, not "unknown".
            Some(SubleadBudget {
                max_subleads: 0,
                per_sublead_usd: 0.0,
                total_usd: 0.0,
            })
        };

        let total_usd = match (root_lead_usd, &subleads) {
            (Some(r), Some(s)) => Some(r + s.total_usd),
            _ => None,
        };

        Self {
            root_lead_usd,
            subleads,
            total_usd,
        }
    }
}

/// Fields that aren't serde-exposed on `ResolvedLead` but live on the
/// underlying `ResolvedManifest`. We surface them via this trait so
/// `BudgetEnvelope::from_lead` doesn't need a `&ResolvedManifest`
/// reference.
trait ResolvedLeadBudget {
    fn budget_usd(&self) -> Option<f64>;
}

impl ResolvedLeadBudget for ResolvedLead {
    fn budget_usd(&self) -> Option<f64> {
        // ResolvedLead doesn't carry budget directly; the dispatcher
        // surfaces it on `ResolvedManifest::budget_usd`. Tree calls
        // `BudgetEnvelope::from_resolved_manifest` instead, but this
        // hook keeps the per-lead extension point open for the future
        // sub-lead-as-lead variant the runtime synthesizes.
        None
    }
}

impl BudgetEnvelope {
    /// Build the envelope from the top-level resolved manifest. The
    /// hierarchical-mode budget surfaces on `ResolvedManifest` (mirrored
    /// from `[lead].budget_usd`), not on `ResolvedLead`, so this is the
    /// entry point most callers want.
    pub fn from_resolved_manifest(m: &ResolvedManifest) -> Option<Self> {
        let lead = m.lead.as_ref()?;
        let root_lead_usd = m.budget_usd;
        let subleads = if lead.allow_subleads {
            match (lead.max_subleads, lead.max_sublead_budget_usd) {
                (Some(n), Some(per)) => Some(SubleadBudget {
                    max_subleads: n,
                    per_sublead_usd: per,
                    #[allow(clippy::cast_precision_loss)]
                    total_usd: per * n as f64,
                }),
                _ => None,
            }
        } else {
            Some(SubleadBudget {
                max_subleads: 0,
                per_sublead_usd: 0.0,
                total_usd: 0.0,
            })
        };
        let total_usd = match (root_lead_usd, &subleads) {
            (Some(r), Some(s)) => Some(r + s.total_usd),
            _ => None,
        };
        Some(Self {
            root_lead_usd,
            subleads,
            total_usd,
        })
    }
}

/// Run the `pitboss tree` subcommand. Returns the exit code so `main`
/// can pass it straight to `std::process::exit` and so tests can assert
/// on the gate behavior.
///
/// Exit codes:
/// * 0 — printed the tree; if `--check` was passed, the budget is
///   within the threshold.
/// * 1 — `--check` failed: aggregate budget over threshold OR
///   unbounded leg in a `--check`-gated manifest.
/// * 2 — manifest could not be loaded.
pub fn run(manifest_path: &Path, check_threshold_usd: Option<f64>) -> i32 {
    let resolved = match load_for_tree(manifest_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pitboss tree: load {}: {e:#}", manifest_path.display());
            return 2;
        }
    };
    let report = render(&resolved);
    print!("{report}");

    let Some(threshold) = check_threshold_usd else {
        return 0;
    };

    match check(&resolved, threshold) {
        CheckOutcome::Ok { total } => {
            eprintln!(
                "pitboss tree --check: OK — aggregate ${total:.2} ≤ threshold ${threshold:.2}"
            );
            0
        }
        CheckOutcome::Over { total } => {
            eprintln!(
                "pitboss tree --check: FAIL — aggregate ${total:.2} > threshold ${threshold:.2}"
            );
            1
        }
        CheckOutcome::Unbounded { reason } => {
            eprintln!("pitboss tree --check: FAIL — {reason}");
            1
        }
        CheckOutcome::FlatMode => {
            eprintln!(
                "pitboss tree --check: SKIP — flat-mode manifest has no budget envelope to check"
            );
            0
        }
    }
}

/// Outcome of a `--check <USD>` comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckOutcome {
    Ok { total: f64 },
    Over { total: f64 },
    Unbounded { reason: String },
    FlatMode,
}

/// Compare the aggregate envelope against `threshold_usd`. Pure
/// function so tests can drive every branch.
pub fn check(m: &ResolvedManifest, threshold_usd: f64) -> CheckOutcome {
    let Some(env) = BudgetEnvelope::from_resolved_manifest(m) else {
        return CheckOutcome::FlatMode;
    };
    let Some(total) = env.total_usd else {
        return CheckOutcome::Unbounded {
            reason: unbounded_reason(&env),
        };
    };
    if total > threshold_usd {
        CheckOutcome::Over { total }
    } else {
        CheckOutcome::Ok { total }
    }
}

fn unbounded_reason(env: &BudgetEnvelope) -> String {
    match (env.root_lead_usd, &env.subleads) {
        (None, _) => "[lead].budget_usd is unset (root pool is unbounded)".to_string(),
        (Some(_), None) => "allow_subleads=true requires both [lead].max_subleads and \
             [lead].max_sublead_budget_usd to compute the aggregate"
            .to_string(),
        _ => "aggregate envelope is unbounded".to_string(),
    }
}

/// Render the dispatch tree as a human-readable string. Pure function
/// — same input always produces identical output.
pub fn render(m: &ResolvedManifest) -> String {
    let mut out = String::new();
    if let Some(lead) = &m.lead {
        render_hierarchical(&mut out, m, lead);
    } else {
        render_flat(&mut out, m);
    }
    out
}

fn render_hierarchical(out: &mut String, m: &ResolvedManifest, lead: &ResolvedLead) {
    let _ = writeln!(out, "Hierarchical mode");
    let _ = writeln!(out);
    let _ = writeln!(out, "  lead {:?}", lead.id);
    let _ = writeln!(out, "    model:    {}", lead.model);
    let _ = writeln!(out, "    effort:   {:?}", lead.effort);
    let _ = writeln!(out, "    timeout:  {}", format_secs(lead.timeout_secs));
    let _ = writeln!(
        out,
        "    workers:  max={}  budget={}  lead_timeout={}",
        m.max_workers
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".to_string()),
        format_usd(m.budget_usd),
        m.lead_timeout_secs
            .map(format_secs)
            .unwrap_or_else(|| "—".to_string()),
    );

    if lead.allow_subleads {
        let _ = writeln!(
            out,
            "    subleads: enabled  max={}  total_workers_cap={}",
            lead.max_subleads
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".to_string()),
            lead.max_total_workers
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".to_string()),
        );
        if let Some(per) = lead.max_sublead_budget_usd {
            let _ = writeln!(out, "    sublead budget cap: {}", format_usd(Some(per)));
        }
        if let Some(sd) = &lead.sublead_defaults {
            render_sublead_defaults(out, sd);
        }
    } else {
        let _ = writeln!(out, "    subleads: disabled");
    }

    let _ = writeln!(out);
    render_envelope(out, m);
}

fn render_sublead_defaults(out: &mut String, sd: &ResolvedSubleadDefaults) {
    let _ = writeln!(out, "    [sublead_defaults]");
    let _ = writeln!(
        out,
        "      provider={}  budget={}  max_workers={}  lead_timeout={}  read_down={}",
        sd.provider,
        format_usd(sd.budget_usd),
        sd.max_workers
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".to_string()),
        sd.lead_timeout_secs
            .map(format_secs)
            .unwrap_or_else(|| "—".to_string()),
        sd.read_down,
    );
}

fn render_envelope(out: &mut String, m: &ResolvedManifest) {
    let Some(env) = BudgetEnvelope::from_resolved_manifest(m) else {
        return;
    };

    out.push_str("Aggregate budget envelope (worst case):\n");
    let _ = writeln!(
        out,
        "  root lead pool:                       {}",
        format_usd(env.root_lead_usd),
    );
    if let Some(s) = &env.subleads {
        if s.max_subleads == 0 {
            out.push_str("  sub-leads:                            disabled\n");
        } else {
            let _ = writeln!(
                out,
                "  sub-leads ({} × ${:.2}, no read_down):  ${:.2}",
                s.max_subleads, s.per_sublead_usd, s.total_usd,
            );
        }
    } else {
        out.push_str("  sub-leads:                            unbounded (missing caps)\n");
    }
    out.push_str("  ────────────────────────────────────────────\n");
    match env.total_usd {
        Some(t) => {
            let _ = writeln!(out, "  total:                                ${t:.2}");
        }
        None => {
            out.push_str(
                "  total:                                unbounded — see warnings above\n",
            );
        }
    }
}

fn render_flat(out: &mut String, m: &ResolvedManifest) {
    let _ = writeln!(
        out,
        "Flat mode ({} task{}, max_parallel_tasks={})",
        m.tasks.len(),
        if m.tasks.len() == 1 { "" } else { "s" },
        m.max_parallel_tasks,
    );
    let _ = writeln!(out);
    for t in &m.tasks {
        let _ = writeln!(out, "  task {:?}", t.id);
        let _ = writeln!(out, "    model:    {}", t.model);
        let _ = writeln!(out, "    effort:   {:?}", t.effort);
        let _ = writeln!(out, "    timeout:  {}", format_secs(t.timeout_secs));
    }
    let _ = writeln!(out);
    out.push_str("No budget envelope configured (flat-mode runs have no per-run cap).\n");
}

fn format_usd(amount: Option<f64>) -> String {
    match amount {
        Some(n) => format!("${n:.2}"),
        None => "—".to_string(),
    }
}

fn format_secs(secs: u64) -> String {
    if secs >= 3600 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h{m}m")
        }
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// Load + resolve a manifest without any directory / git work-tree
/// validation. Tree is a pre-flight visualisation tool — it must
/// accept manifests with placeholder paths (e.g. fresh `pitboss init`
/// output) and runtime-only constraints. The schema parse + the
/// resolver are the only checks we want here.
pub fn load_for_tree(path: &Path) -> Result<ResolvedManifest> {
    let src = std::fs::read_to_string(path)
        .with_context(|| format!("read manifest {}", path.display()))?;
    load_manifest_from_str(&src).with_context(|| format!("parse manifest {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::load::load_manifest_from_str;

    const HIERARCHICAL_WITH_SUBLEADS: &str = r#"
[lead]
id        = "coordinator"
directory = "/tmp"
prompt    = "do the thing"

max_workers       = 8
budget_usd        = 20.00
lead_timeout_secs = 7200

allow_subleads         = true
max_subleads           = 3
max_sublead_budget_usd = 5.00
max_total_workers      = 16

[sublead_defaults]
budget_usd        = 5.00
max_workers       = 4
lead_timeout_secs = 3600
read_down         = false
"#;

    const HIERARCHICAL_NO_SUBLEADS: &str = r#"
[lead]
id        = "coordinator"
directory = "/tmp"
prompt    = "do the thing"

max_workers       = 4
budget_usd        = 5.00
lead_timeout_secs = 1800
"#;

    const FLAT: &str = r#"
[run]
max_parallel_tasks = 4

[[task]]
id        = "t1"
directory = "/tmp"
prompt    = "first"

[[task]]
id        = "t2"
directory = "/tmp"
prompt    = "second"
"#;

    const HIERARCHICAL_UNBOUNDED_SUBLEADS: &str = r#"
[lead]
id        = "coordinator"
directory = "/tmp"
prompt    = "do the thing"

max_workers       = 8
budget_usd        = 20.00
lead_timeout_secs = 7200

allow_subleads = true
"#;

    const HIERARCHICAL_NO_BUDGET: &str = r#"
[lead]
id        = "coordinator"
directory = "/tmp"
prompt    = "do the thing"
"#;

    fn load(s: &str) -> ResolvedManifest {
        load_manifest_from_str(s).expect("manifest should resolve")
    }

    // ── BudgetEnvelope ──────────────────────────────────────────────────

    #[test]
    fn envelope_with_subleads_aggregates_correctly() {
        let m = load(HIERARCHICAL_WITH_SUBLEADS);
        let env = BudgetEnvelope::from_resolved_manifest(&m).unwrap();
        assert_eq!(env.root_lead_usd, Some(20.00));
        let sub = env.subleads.unwrap();
        assert_eq!(sub.max_subleads, 3);
        assert_eq!(sub.per_sublead_usd, 5.00);
        assert_eq!(sub.total_usd, 15.00);
        assert_eq!(env.total_usd, Some(35.00));
    }

    #[test]
    fn envelope_no_subleads_is_root_only() {
        let m = load(HIERARCHICAL_NO_SUBLEADS);
        let env = BudgetEnvelope::from_resolved_manifest(&m).unwrap();
        assert_eq!(env.root_lead_usd, Some(5.00));
        let sub = env.subleads.unwrap();
        assert_eq!(sub.total_usd, 0.0);
        assert_eq!(env.total_usd, Some(5.00));
    }

    #[test]
    fn envelope_subleads_without_caps_is_unbounded() {
        let m = load(HIERARCHICAL_UNBOUNDED_SUBLEADS);
        let env = BudgetEnvelope::from_resolved_manifest(&m).unwrap();
        assert!(env.subleads.is_none(), "missing caps should yield None");
        assert!(env.total_usd.is_none());
    }

    #[test]
    fn envelope_no_root_budget_is_unbounded_total() {
        let m = load(HIERARCHICAL_NO_BUDGET);
        let env = BudgetEnvelope::from_resolved_manifest(&m).unwrap();
        assert!(env.root_lead_usd.is_none());
        assert!(env.total_usd.is_none());
    }

    #[test]
    fn envelope_flat_mode_is_none() {
        let m = load(FLAT);
        assert!(BudgetEnvelope::from_resolved_manifest(&m).is_none());
    }

    // ── check() ──────────────────────────────────────────────────────────

    #[test]
    fn check_within_threshold_passes() {
        let m = load(HIERARCHICAL_WITH_SUBLEADS);
        match check(&m, 50.00) {
            CheckOutcome::Ok { total } => assert_eq!(total, 35.00),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn check_over_threshold_fails() {
        let m = load(HIERARCHICAL_WITH_SUBLEADS);
        match check(&m, 30.00) {
            CheckOutcome::Over { total } => assert_eq!(total, 35.00),
            other => panic!("expected Over, got {other:?}"),
        }
    }

    #[test]
    fn check_at_threshold_passes() {
        // Boundary: threshold == total. Allowed (≤, not <).
        let m = load(HIERARCHICAL_WITH_SUBLEADS);
        match check(&m, 35.00) {
            CheckOutcome::Ok { total } => assert_eq!(total, 35.00),
            other => panic!("expected Ok at boundary, got {other:?}"),
        }
    }

    #[test]
    fn check_unbounded_subleads_fails_with_reason() {
        let m = load(HIERARCHICAL_UNBOUNDED_SUBLEADS);
        let outcome = check(&m, 1000.00);
        match outcome {
            CheckOutcome::Unbounded { reason } => {
                assert!(
                    reason.contains("max_subleads") && reason.contains("max_sublead_budget_usd"),
                    "reason should name both missing caps: {reason}"
                );
            }
            other => panic!("expected Unbounded, got {other:?}"),
        }
    }

    #[test]
    fn check_no_root_budget_is_unbounded() {
        let m = load(HIERARCHICAL_NO_BUDGET);
        let outcome = check(&m, 1000.00);
        match outcome {
            CheckOutcome::Unbounded { reason } => {
                assert!(reason.contains("budget_usd is unset"), "got: {reason}");
            }
            other => panic!("expected Unbounded, got {other:?}"),
        }
    }

    #[test]
    fn check_flat_mode_skips() {
        let m = load(FLAT);
        assert_eq!(check(&m, 0.0), CheckOutcome::FlatMode);
    }

    // ── render() ─────────────────────────────────────────────────────────

    #[test]
    fn render_hierarchical_includes_lead_id_and_envelope() {
        let m = load(HIERARCHICAL_WITH_SUBLEADS);
        let s = render(&m);
        assert!(s.contains("Hierarchical mode"));
        assert!(s.contains("\"coordinator\""));
        assert!(s.contains("budget=$20.00"));
        assert!(s.contains("Aggregate budget envelope"));
        assert!(s.contains("$35.00"), "total should appear; got:\n{s}");
        assert!(s.contains("read_down=false"));
    }

    #[test]
    fn render_flat_includes_each_task() {
        let m = load(FLAT);
        let s = render(&m);
        assert!(s.contains("Flat mode (2 tasks"));
        assert!(s.contains("\"t1\""));
        assert!(s.contains("\"t2\""));
        assert!(s.contains("No budget envelope"));
    }

    #[test]
    fn render_subleads_disabled_says_so() {
        let m = load(HIERARCHICAL_NO_SUBLEADS);
        let s = render(&m);
        assert!(s.contains("subleads: disabled"));
        assert!(
            !s.contains("[sublead_defaults]"),
            "should not render the [sublead_defaults] section when subleads are off"
        );
    }

    #[test]
    fn render_unbounded_envelope_calls_it_out() {
        let m = load(HIERARCHICAL_UNBOUNDED_SUBLEADS);
        let s = render(&m);
        assert!(s.contains("unbounded"));
    }

    // ── format_secs ──────────────────────────────────────────────────────

    #[test]
    fn format_secs_picks_appropriate_unit() {
        assert_eq!(format_secs(45), "45s");
        assert_eq!(format_secs(120), "2m");
        assert_eq!(format_secs(3600), "1h");
        assert_eq!(format_secs(7200), "2h");
        assert_eq!(format_secs(7260), "2h1m");
    }
}
