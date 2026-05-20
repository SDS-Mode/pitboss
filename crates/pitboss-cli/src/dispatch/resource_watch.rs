//! Per-actor resource sampler + memory-pressure tracker (#553).
//!
//! One [`ResourceWatcher`] is spawned per dispatch process — root
//! lead, sub-leads, and workers all funnel into the same sampler. The
//! watcher reads `/proc/<pid>/{status,stat}` for every pid registered
//! in `layer.workers.pids`, sums RSS to compute `total_rss`, derives
//! container headroom from `/sys/fs/cgroup/memory.{max,current}` (or
//! falls back to `/proc/meminfo` `MemTotal` for flat host dispatch),
//! and broadcasts a `ControlEvent::ResourceSample` on every tick. The
//! [`PressureTracker`] folds the latest sample into a hysteretic
//! state machine and fires `ControlEvent::ResourcePressure` on
//! warn / error / clear transitions.
//!
//! ## Why dispatcher-level, not per-connection
//!
//! `StoreActivity` in `control/server.rs:564` runs *per-connection*
//! and only persists samples while a control-bridge client is
//! attached. Resource sampling has to persist regardless — operators
//! looking at a finalized run want to know whether it came close to
//! its memory ceiling even if the TUI was never opened. The watcher
//! piggy-backs on the same broadcast bus + `events.jsonl` subscriber
//! that the persistent task in `dispatch/state.rs:507` already
//! drains, so headless dispatches get the full time series.
//!
//! ## Where the watcher reads from
//!
//! | Mode                    | Sampler runs           | Reads from                                | Headroom denominator             |
//! |-------------------------|------------------------|-------------------------------------------|----------------------------------|
//! | Flat (linux)            | host                   | `/proc/<pid>`                             | `/proc/meminfo` `MemTotal`       |
//! | Flat (macOS)            | host                   | n/a — emits "darwin unsupported" + exits  | n/a                              |
//! | Container               | inside podman VM       | in-container `/proc` + cgroup files       | `/sys/fs/cgroup/memory.max`      |
//! | Container + host web UI | inside podman VM       | same as Container                         | same                             |
//!
//! There is no two-source merge — `pitboss-web` reads the in-container
//! samples through the existing control-bridge transport (`/api/runs/
//! <id>/events` SSE) and the persisted `events.jsonl`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pitboss_core::store::record::ResourceHighWater;

use crate::control::protocol::{ControlEvent, EventEnvelope, PressureLevel, ResourceSampleEntry};
use crate::dispatch::actor::ActorPath;
use crate::dispatch::layer::LayerState;
use crate::dispatch::state::DispatchState;

/// Hysteresis thresholds for the pressure tracker (#553). Picked to
/// stay below typical container OOM-kill points (~95%) with enough
/// headroom for the warn signal to actually be useful — at 5 s
/// cadence, a warn at 70% gives ~1-2 minutes of lead time before the
/// kernel reaper fires in the OOM scenarios we've seen.
const PRESSURE_WARN_FRAC: f32 = 0.70;
const PRESSURE_ERROR_FRAC: f32 = 0.90;
const PRESSURE_CLEAR_FRAC: f32 = 0.60;
/// Number of consecutive sub-clear samples required before emitting
/// `Clear`. Two is the smallest value that visibly debounces a run
/// hovering near the warn threshold; bigger values would just delay
/// the banner dismissal.
const PRESSURE_CLEAR_DEBOUNCE_SAMPLES: u32 = 2;

/// Dispatcher-level resource sampler handle. Cheap to clone — the
/// watcher task owns the periodic tokio task; this struct only
/// carries the shared accumulator readback path.
pub struct ResourceWatcher {
    high_water: Arc<Mutex<HighWaterAccum>>,
    cadence_secs: u64,
}

impl ResourceWatcher {
    /// Spawn the watcher task. Returns immediately; the task exits
    /// when `state.root.cancel.await_terminate()` fires (run is
    /// cancelled or finalized). When `cadence_secs == 0`, no task is
    /// spawned and the returned watcher carries an empty accumulator
    /// — finalize sees `sample_count == 0` and writes
    /// `resource_high_water: None` into `summary.json`.
    ///
    /// On hosts without `/proc` (darwin) the spawned task logs one
    /// INFO line and exits cleanly. Inside podman / linux containers
    /// the same code path picks up the in-container `/proc` and
    /// `/sys/fs/cgroup` mounts naturally.
    pub fn spawn(state: Arc<DispatchState>, cadence_secs: u64) -> Arc<Self> {
        let high_water = Arc::new(Mutex::new(HighWaterAccum::with_cadence(cadence_secs)));
        let watcher = Arc::new(Self {
            high_water: Arc::clone(&high_water),
            cadence_secs,
        });
        if cadence_secs == 0 {
            return watcher;
        }
        if !proc_filesystem_available() {
            tracing::info!(
                "resource_watch: /proc unavailable (likely darwin host); \
                 per-actor resource sampling disabled. Inside container-dispatch \
                 the watcher runs in the linux VM so macOS hosts still get \
                 full pressure visibility through `pitboss container-dispatch`."
            );
            return watcher;
        }
        tokio::spawn(run_sampler(
            Arc::clone(&state),
            high_water,
            Duration::from_secs(cadence_secs),
        ));
        watcher
    }

    /// Read the in-memory roll-up at finalize time. Cheap copy. The
    /// caller (finalize site in `runner.rs` / `hierarchical.rs`)
    /// stuffs this into `RunSummary.resource_high_water` only when
    /// `sample_count > 0` so untouched / disabled runs serialise
    /// `resource_high_water: None`.
    pub fn high_water_snapshot(&self) -> ResourceHighWater {
        // `std::sync::Mutex` here: the critical section is a tiny
        // clone-out, never holds across `.await`, and finalize runs
        // off the hot path. Poison is impossible in practice (the
        // sampler task never panics — `/proc` read errors are
        // tolerated), but if it ever does we degrade to "no
        // high-water data" rather than crashing finalize.
        let g = match self.high_water.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut hw = (*g).clone().into_summary();
        hw.sample_cadence_secs = self.cadence_secs;
        hw
    }
}

async fn run_sampler(
    state: Arc<DispatchState>,
    high_water: Arc<Mutex<HighWaterAccum>>,
    cadence: Duration,
) {
    // tokio::time::interval fires the first tick immediately; using
    // `interval_at(now + cadence, …)` would swallow the first cadence,
    // which on short worker-spawn-storm OOMs (peak within 10-20 s) meant
    // the motivating incident's peak landed before any sample was taken.
    // (#553 follow-up to PR #579 R3 finding F5)
    let mut interval = tokio::time::interval(cadence);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut tracker = PressureTracker::default();

    loop {
        tokio::select! {
            _ = state.root.cancel.await_terminate() => {
                tracing::debug!("resource_watch: cancel fired, exiting sampler");
                break;
            }
            _ = interval.tick() => {}
        }

        let samples = collect_samples(&state).await;
        let cgroup = read_cgroup_memory().await;
        let host_total = if cgroup.is_none() {
            read_meminfo_total().await
        } else {
            None
        };

        let total_rss = samples.iter().map(|s| s.rss_bytes).sum::<u64>();
        let denominator = cgroup
            .as_ref()
            .map(|c| c.max_bytes)
            .or(host_total)
            .unwrap_or(0);
        {
            let mut g = match high_water.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            g.observe(
                total_rss,
                denominator,
                cgroup.as_ref().map(|c| c.max_bytes),
                &samples,
            );
        }

        // Build + broadcast sample envelope first; pressure transitions
        // (which use the same denominator) follow immediately after so
        // the order on the wire is "facts, then judgement."
        let sample_event = ControlEvent::ResourceSample {
            samples: samples.clone(),
            cgroup_memory_current_bytes: cgroup.as_ref().map(|c| c.current_bytes),
            cgroup_memory_max_bytes: cgroup.as_ref().map(|c| c.max_bytes),
            host_mem_total_bytes: host_total,
        };
        broadcast(&state.root, sample_event).await;

        if denominator > 0 {
            if let Some(transition) = tracker.observe(total_rss, denominator) {
                let message = pressure_message(transition.level, total_rss, denominator);
                broadcast(
                    &state.root,
                    ControlEvent::ResourcePressure {
                        level: transition.level,
                        total_rss_bytes: total_rss,
                        available_bytes: denominator,
                        message,
                    },
                )
                .await;
            }
        }
    }
}

async fn broadcast(layer: &LayerState, event: ControlEvent) {
    let envelope = EventEnvelope {
        actor_path: ActorPath::default(),
        seq: 0, // assigned inside broadcast_control_event via event_log.next_seq()
        event,
    };
    layer.broadcast_control_event(envelope).await;
}

/// Snapshot every layer's `workers.pids` map and read /proc for each
/// live pid. Reaped processes (pid==0 or `/proc/<pid>` already gone)
/// are dropped silently — they're either between spawn and exec, or
/// just finished and not yet removed from the registry. The latter
/// case shows up as one missing sample then the entry vanishes, which
/// is exactly the "actor finished" signal consumers want.
async fn collect_samples(state: &DispatchState) -> Vec<ResourceSampleEntry> {
    let mut pid_map: Vec<(String, u32)> = Vec::new();
    {
        let pids = state.root.workers.pids.read().await;
        for (id, slot) in pids.iter() {
            let pid = slot.load(std::sync::atomic::Ordering::Acquire);
            pid_map.push((id.clone(), pid));
        }
    }
    {
        let subleads = state.subleads.read().await;
        for layer in subleads.values() {
            let pids = layer.workers.pids.read().await;
            for (id, slot) in pids.iter() {
                let pid = slot.load(std::sync::atomic::Ordering::Acquire);
                pid_map.push((id.clone(), pid));
            }
        }
    }

    let mut out = Vec::with_capacity(pid_map.len());
    for (actor_id, pid) in pid_map {
        if pid == 0 {
            // pid slot registered but child hasn't published yet; emit a
            // skeleton entry so consumers see "this actor was alive at
            // sample time, just hadn't published RSS yet." Drops to zero
            // values that skip-serialise via `is_zero_u64`.
            out.push(ResourceSampleEntry {
                actor_id,
                pid: 0,
                rss_bytes: 0,
                vsz_bytes: 0,
                cpu_jiffies: 0,
            });
            continue;
        }
        let Some(stat) = read_proc_pid(pid).await else {
            // Process reaped between the pid-map snapshot and the
            // /proc read. Skip — the actor's TaskRecord will surface
            // the terminal state.
            continue;
        };
        out.push(ResourceSampleEntry {
            actor_id,
            pid,
            rss_bytes: stat.rss_bytes,
            vsz_bytes: stat.vsz_bytes,
            cpu_jiffies: stat.cpu_jiffies,
        });
    }
    out
}

struct ProcStat {
    rss_bytes: u64,
    vsz_bytes: u64,
    cpu_jiffies: u64,
}

async fn read_proc_pid(pid: u32) -> Option<ProcStat> {
    let status_path = format!("/proc/{pid}/status");
    let stat_path = format!("/proc/{pid}/stat");
    let status = tokio::fs::read_to_string(&status_path).await.ok()?;
    let stat = tokio::fs::read_to_string(&stat_path).await.ok()?;
    let (rss_bytes, vsz_bytes) = parse_status_rss_vsz(&status);
    let cpu_jiffies = parse_stat_cpu_jiffies(&stat);
    Some(ProcStat {
        rss_bytes,
        vsz_bytes,
        cpu_jiffies,
    })
}

/// Parse `VmRSS` and `VmSize` (in kB) out of `/proc/<pid>/status`,
/// returning bytes. Missing fields default to 0 — kernel threads or
/// processes that don't expose Vm* lines (rare for userspace claude
/// subprocesses but cheap to tolerate).
fn parse_status_rss_vsz(status: &str) -> (u64, u64) {
    let mut rss_kb: u64 = 0;
    let mut vsz_kb: u64 = 0;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            rss_kb = parse_kb_number(rest);
        } else if let Some(rest) = line.strip_prefix("VmSize:") {
            vsz_kb = parse_kb_number(rest);
        }
    }
    (rss_kb.saturating_mul(1024), vsz_kb.saturating_mul(1024))
}

fn parse_kb_number(s: &str) -> u64 {
    s.split_ascii_whitespace()
        .next()
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Parse `utime + stime` (fields 14 + 15 in `/proc/<pid>/stat`,
/// 1-indexed). The comm field at index 2 can contain spaces and
/// parentheses, so we find the last `)` and split fields from there.
fn parse_stat_cpu_jiffies(stat: &str) -> u64 {
    let close = match stat.rfind(')') {
        Some(i) => i,
        None => return 0,
    };
    let tail = &stat[close + 1..];
    // After ')' the fields are: state utime_idx14 stime_idx15 ...
    // tail starts with ' STATE FIELDS...', so split_whitespace yields:
    //   0: state
    //   1: ppid
    //   2: pgrp
    //   3: session
    //   ...
    //   11: utime  (man proc: position 14 → 14 - 3 = 11 after removing pid+comm+state? actually we already skipped past pid+comm; state is the first token)
    // Per `man 5 proc` field numbering: 1=pid, 2=comm, 3=state, 14=utime, 15=stime.
    // After skipping past `)` we are at field 3 onwards → utime is tail-index 11, stime is index 12.
    let parts: Vec<&str> = tail.split_ascii_whitespace().collect();
    let utime = parts
        .get(11)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let stime = parts
        .get(12)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    utime.saturating_add(stime)
}

struct CgroupMemory {
    max_bytes: u64,
    current_bytes: u64,
}

async fn read_cgroup_memory() -> Option<CgroupMemory> {
    if let Some(cg) = read_cgroup_v2_files().await {
        return Some(cg);
    }
    // cgroupv1 fallback: older docker, RHEL-7, K8s pre-1.25 without
    // systemd-cgroup migration. Without this, the caller falls back to
    // /proc/meminfo MemTotal — but MemTotal is NOT pidns-namespaced and
    // reports the host's total RAM inside a v1 container, silently
    // making the pressure thresholds never fire on a constrained
    // cgroup. (#553 follow-up to PR #579 R3 finding F1)
    read_cgroup_v1_files().await
}

async fn read_cgroup_v2_files() -> Option<CgroupMemory> {
    let max_raw = tokio::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .await
        .ok()?;
    let cur_raw = tokio::fs::read_to_string("/sys/fs/cgroup/memory.current")
        .await
        .ok()?;
    parse_cgroup_v2(&max_raw, &cur_raw)
}

fn parse_cgroup_v2(max_raw: &str, cur_raw: &str) -> Option<CgroupMemory> {
    let max_trim = max_raw.trim();
    if max_trim == "max" {
        // Unlimited cgroup (host root cgroup, or no limit configured).
        // Treated as "no headroom denominator" so flat-host fallback
        // takes over.
        return None;
    }
    let max_bytes = max_trim.parse::<u64>().ok()?;
    let current_bytes = cur_raw.trim().parse::<u64>().ok()?;
    Some(CgroupMemory {
        max_bytes,
        current_bytes,
    })
}

async fn read_cgroup_v1_files() -> Option<CgroupMemory> {
    let max_raw = tokio::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
        .await
        .ok()?;
    let cur_raw = tokio::fs::read_to_string("/sys/fs/cgroup/memory/memory.usage_in_bytes")
        .await
        .ok()?;
    parse_cgroup_v1(&max_raw, &cur_raw)
}

fn parse_cgroup_v1(max_raw: &str, cur_raw: &str) -> Option<CgroupMemory> {
    // cgroupv1 "unlimited" sentinel: the kernel writes PAGE_COUNTER_MAX,
    // typically 9223372036854771712 (i64::MAX rounded down to PAGE_SIZE).
    // Treat anything within one page (4 KiB) of i64::MAX as unlimited.
    const V1_UNLIMITED_THRESHOLD: u64 = (i64::MAX as u64) - 4096;
    let max_bytes = max_raw.trim().parse::<u64>().ok()?;
    if max_bytes >= V1_UNLIMITED_THRESHOLD {
        return None;
    }
    let current_bytes = cur_raw.trim().parse::<u64>().ok()?;
    Some(CgroupMemory {
        max_bytes,
        current_bytes,
    })
}

async fn read_meminfo_total() -> Option<u64> {
    let raw = tokio::fs::read_to_string("/proc/meminfo").await.ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb = parse_kb_number(rest);
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

fn proc_filesystem_available() -> bool {
    Path::new("/proc/self/stat").exists()
}

// ---- pressure tracker --------------------------------------------------

#[derive(Default)]
struct PressureTracker {
    state: PressureLevel,
    consecutive_clear_samples: u32,
}

struct PressureTransition {
    level: PressureLevel,
}

impl PressureTracker {
    /// Observe a new sample. Returns `Some(transition)` only when the
    /// hysteretic state machine flips state — callers emit a
    /// `ResourcePressure` envelope on `Some`, nothing on `None`.
    fn observe(&mut self, total_rss: u64, available: u64) -> Option<PressureTransition> {
        if available == 0 {
            return None;
        }
        let frac = total_rss as f32 / available as f32;
        let new = if frac >= PRESSURE_ERROR_FRAC {
            PressureLevel::Error
        } else if frac >= PRESSURE_WARN_FRAC {
            PressureLevel::Warn
        } else {
            // Sub-warn band. We don't immediately drop to Clear — only
            // after a debounce window below the clear floor.
            if frac < PRESSURE_CLEAR_FRAC {
                self.consecutive_clear_samples = self.consecutive_clear_samples.saturating_add(1);
            } else {
                // In the dead-band (clear..warn) we hold the previous
                // state and reset the debounce counter — staying e.g.
                // at Warn is fine until we drop firmly below clear.
                self.consecutive_clear_samples = 0;
            }
            if matches!(self.state, PressureLevel::Clear) {
                self.consecutive_clear_samples = 0;
                return None;
            }
            if self.consecutive_clear_samples >= PRESSURE_CLEAR_DEBOUNCE_SAMPLES {
                self.consecutive_clear_samples = 0;
                PressureLevel::Clear
            } else {
                return None;
            }
        };
        if new == self.state {
            // Re-entering the warn/error band from inside it (or
            // staying clear): no transition, no envelope.
            if !matches!(new, PressureLevel::Clear) {
                self.consecutive_clear_samples = 0;
            }
            return None;
        }
        self.state = new;
        if !matches!(new, PressureLevel::Clear) {
            self.consecutive_clear_samples = 0;
        }
        Some(PressureTransition { level: new })
    }
}

fn pressure_message(level: PressureLevel, total_rss: u64, available: u64) -> String {
    let pct = if available == 0 {
        0.0
    } else {
        100.0 * (total_rss as f32 / available as f32)
    };
    let total_gb = bytes_to_gb(total_rss);
    let avail_gb = bytes_to_gb(available);
    match level {
        PressureLevel::Warn => format!(
            "Container at {pct:.0}% of available memory ({total_gb:.2} / {avail_gb:.2} GB). \
             Consider lowering [lead].max_workers."
        ),
        PressureLevel::Error => format!(
            "Container at {pct:.0}% of available memory ({total_gb:.2} / {avail_gb:.2} GB). \
             OOM-kill imminent. Raise the cgroup ceiling \
             (`podman machine set --memory 4096`) or reduce worker fan-out."
        ),
        PressureLevel::Clear => format!("Memory pressure cleared ({pct:.0}% of {avail_gb:.2} GB)."),
    }
}

fn bytes_to_gb(b: u64) -> f32 {
    (b as f64 / (1024.0 * 1024.0 * 1024.0)) as f32
}

// ---- high-water accumulator --------------------------------------------

#[derive(Clone, Default)]
struct HighWaterAccum {
    total_rss_bytes_max: u64,
    cgroup_memory_max_bytes: Option<u64>,
    peak_utilization_pct: Option<f32>,
    rss_bytes_max_by_actor: BTreeMap<String, u64>,
    sample_count: u64,
    sample_cadence_secs: u64,
}

impl HighWaterAccum {
    fn with_cadence(secs: u64) -> Self {
        Self {
            sample_cadence_secs: secs,
            ..Self::default()
        }
    }

    fn observe(
        &mut self,
        total_rss: u64,
        denominator: u64,
        cgroup_max: Option<u64>,
        samples: &[ResourceSampleEntry],
    ) {
        self.sample_count = self.sample_count.saturating_add(1);
        if total_rss > self.total_rss_bytes_max {
            self.total_rss_bytes_max = total_rss;
            if denominator > 0 {
                self.peak_utilization_pct = Some(total_rss as f32 / denominator as f32);
            }
        }
        if cgroup_max.is_some() {
            self.cgroup_memory_max_bytes = cgroup_max;
        }
        for s in samples {
            let slot = self
                .rss_bytes_max_by_actor
                .entry(s.actor_id.clone())
                .or_insert(0);
            if s.rss_bytes > *slot {
                *slot = s.rss_bytes;
            }
        }
    }

    fn into_summary(self) -> ResourceHighWater {
        ResourceHighWater {
            total_rss_bytes_max: self.total_rss_bytes_max,
            cgroup_memory_max_bytes: self.cgroup_memory_max_bytes,
            peak_utilization_pct: self.peak_utilization_pct,
            rss_bytes_max_by_actor: self.rss_bytes_max_by_actor,
            sample_count: self.sample_count,
            sample_cadence_secs: self.sample_cadence_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cgroup_v2_parses_limited() {
        let cg = parse_cgroup_v2("2147483648\n", "1073741824\n").unwrap();
        assert_eq!(cg.max_bytes, 2_147_483_648);
        assert_eq!(cg.current_bytes, 1_073_741_824);
    }

    #[test]
    fn cgroup_v2_max_sentinel_returns_none() {
        assert!(parse_cgroup_v2("max\n", "12345\n").is_none());
    }

    #[test]
    fn cgroup_v1_parses_limited() {
        let cg = parse_cgroup_v1("2147483648\n", "1073741824\n").unwrap();
        assert_eq!(cg.max_bytes, 2_147_483_648);
        assert_eq!(cg.current_bytes, 1_073_741_824);
    }

    #[test]
    fn cgroup_v1_unlimited_sentinel_returns_none() {
        // Kernel-written "unlimited" on cgroupv1 — PAGE_COUNTER_MAX
        // (i64::MAX rounded down to PAGE_SIZE). Verifies the fallback
        // doesn't accidentally treat the unlimited container as
        // having a tiny denominator.
        assert!(parse_cgroup_v1("9223372036854771712\n", "12345").is_none());
    }

    #[test]
    fn parses_vm_rss_and_vm_size_from_status() {
        let status = "Name:\tclaude\n\
                      State:\tS (sleeping)\n\
                      VmPeak:\t  2048000 kB\n\
                      VmSize:\t  1048576 kB\n\
                      VmRSS:\t   204800 kB\n";
        let (rss, vsz) = parse_status_rss_vsz(status);
        assert_eq!(rss, 204_800 * 1024);
        assert_eq!(vsz, 1_048_576 * 1024);
    }

    #[test]
    fn status_without_vm_lines_is_zero() {
        let (rss, vsz) = parse_status_rss_vsz("Name:\tinit\nState:\tS\n");
        assert_eq!(rss, 0);
        assert_eq!(vsz, 0);
    }

    #[test]
    fn parses_cpu_jiffies_from_stat_with_paren_in_comm() {
        // Fake stat line where comm contains a parenthesis and space.
        // After the last ')', fields are (1-indexed from `man 5 proc`):
        //   3:state 4:ppid 5:pgrp ... 14:utime 15:stime ...
        // We need utime + stime → values 100 + 50 = 150.
        let stat = "12345 (cl (a)ude) S 1 12345 12345 0 -1 0 0 0 0 0 \
                    100 50 0 0 20 0 1 0 0 0 0 0";
        assert_eq!(parse_stat_cpu_jiffies(stat), 150);
    }

    #[test]
    fn stat_without_close_paren_returns_zero() {
        assert_eq!(parse_stat_cpu_jiffies("garbage with no paren"), 0);
    }

    #[test]
    fn pressure_tracker_warn_then_error_then_clear() {
        let mut t = PressureTracker::default();
        // 50% → no transition (still Clear)
        assert!(t.observe(50, 100).is_none());
        // 75% → Warn
        let trans = t.observe(75, 100).expect("warn transition");
        assert_eq!(trans.level, PressureLevel::Warn);
        // 76% → no re-trigger (already Warn)
        assert!(t.observe(76, 100).is_none());
        // 92% → Error
        let trans = t.observe(92, 100).expect("error transition");
        assert_eq!(trans.level, PressureLevel::Error);
        // 50% → still Error (debounce in progress, only 1 sample below)
        assert!(t.observe(50, 100).is_none());
        // 50% again → Clear (2 consecutive samples below 60%)
        let trans = t.observe(50, 100).expect("clear transition");
        assert_eq!(trans.level, PressureLevel::Clear);
    }

    #[test]
    fn pressure_tracker_does_not_flap_in_dead_band() {
        // 65% sits in the (clear, warn) dead band. Starting Clear and
        // hovering there must never trigger Warn (we need to cross
        // 70%) nor Clear (already Clear).
        let mut t = PressureTracker::default();
        for _ in 0..10 {
            assert!(t.observe(65, 100).is_none());
        }
    }

    #[test]
    fn pressure_tracker_clear_requires_two_consecutive_below() {
        let mut t = PressureTracker::default();
        // Bump to Warn
        let _ = t.observe(75, 100).expect("warn");
        // One sub-clear sample
        assert!(t.observe(50, 100).is_none());
        // Bounce back above clear before second clear sample
        assert!(t.observe(65, 100).is_none());
        // First clear sample re-arms; need another consecutive
        assert!(t.observe(50, 100).is_none());
        let trans = t.observe(50, 100).expect("clear after second");
        assert_eq!(trans.level, PressureLevel::Clear);
    }

    #[test]
    fn high_water_accum_tracks_peak_and_per_actor() {
        let mut h = HighWaterAccum::with_cadence(5);
        let s1 = vec![
            ResourceSampleEntry {
                actor_id: "lead".into(),
                pid: 1,
                rss_bytes: 100,
                vsz_bytes: 0,
                cpu_jiffies: 0,
            },
            ResourceSampleEntry {
                actor_id: "w1".into(),
                pid: 2,
                rss_bytes: 200,
                vsz_bytes: 0,
                cpu_jiffies: 0,
            },
        ];
        h.observe(300, 1000, Some(1000), &s1);
        let s2 = vec![ResourceSampleEntry {
            actor_id: "w1".into(),
            pid: 2,
            rss_bytes: 500,
            vsz_bytes: 0,
            cpu_jiffies: 0,
        }];
        h.observe(500, 1000, Some(1000), &s2);
        let sum = h.into_summary();
        assert_eq!(sum.total_rss_bytes_max, 500);
        assert_eq!(sum.peak_utilization_pct, Some(0.5));
        assert_eq!(sum.cgroup_memory_max_bytes, Some(1000));
        assert_eq!(sum.rss_bytes_max_by_actor.get("lead"), Some(&100));
        assert_eq!(sum.rss_bytes_max_by_actor.get("w1"), Some(&500));
        assert_eq!(sum.sample_count, 2);
        assert_eq!(sum.sample_cadence_secs, 5);
    }

    #[test]
    fn watcher_with_zero_cadence_does_not_panic_on_snapshot() {
        // Mirrors the disabled-cadence path: spawn never schedules a
        // task, snapshot returns zero counts.
        let h = HighWaterAccum::with_cadence(0).into_summary();
        assert_eq!(h.sample_count, 0);
        assert_eq!(h.sample_cadence_secs, 0);
    }
}
