//! In-memory per-run shared store for hub-mediated coordination between
//! the lead and workers. See
//! `docs/superpowers/specs/2026-04-18-worker-shared-store-design.md`.

pub mod leases;
pub mod run_leases;
pub mod tools;
pub use leases::{AcquireResult, Lease, LeaseRegistry};
pub use run_leases::{LeaseHandle, LeaseRegistry as RunLeaseRegistry};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

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
    /// Lead of the root layer (or depth-1 compat alias; `"root_lead"` also accepted).
    #[serde(alias = "root_lead")]
    Lead,
    /// Sub-lead: leads its own sub-tree layer.
    Sublead,
    /// Worker in any layer.
    Worker,
}

const LIST_RESULT_CAP: usize = 1000;

/// Result of a `kv_list` that preserves truncation information. Callers
/// that see `entries.len() == LIST_RESULT_CAP` cannot otherwise distinguish
/// "exactly cap matches" from "cap-or-more matches, truncated" — and
/// either guess can silently break coordination across a large run.
/// `total_matched` is the real match count prior to truncation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListResult {
    pub entries: Vec<ListMetadata>,
    /// `true` iff `total_matched > entries.len()`.
    pub truncated: bool,
    /// Total matches found before truncation to `LIST_RESULT_CAP`.
    pub total_matched: u64,
}

fn authorize_write(
    path: &str,
    caller: &CallerIdentity,
    override_flag: bool,
) -> Result<(), StoreError> {
    // Sub-leads are the leads of their own layer; treat them as leads for
    // write-authz purposes. The per-layer routing in the MCP server ensures
    // they can only write to their own layer's store.
    let effective_role = match caller.role {
        ActorRole::Sublead => ActorRole::Lead,
        r => r,
    };
    let namespace = path.split('/').nth(1).unwrap_or("");
    match (namespace, effective_role) {
        ("ref", ActorRole::Lead) => Ok(()),
        ("ref", ActorRole::Worker) => Err(StoreError::Forbidden(format!(
            "/ref is lead-write-only (your actor_id={}, role=worker); \
             write to /peer/self/... or /shared/... instead",
            caller.id
        ))),
        ("peer", _) => {
            let actor_seg = path.split('/').nth(2).unwrap_or("");
            match (actor_seg == caller.id, effective_role, override_flag) {
                (true, _, _) => Ok(()),
                (_, ActorRole::Lead, true) => Ok(()),
                (_, ActorRole::Lead, false) => Err(StoreError::Forbidden(format!(
                    "lead (actor_id={}) may write /peer/{}/* only with override=true",
                    caller.id, actor_seg
                ))),
                (_, ActorRole::Worker, _) => Err(StoreError::Forbidden(format!(
                    "worker tried to write /peer/{}/* but your actor_id is {}; \
                     use /peer/self/... (auto-resolves) or /peer/{}/... explicitly",
                    actor_seg, caller.id, caller.id
                ))),
                // Unreachable after substitution above, but needed for exhaustiveness.
                _ => Err(StoreError::Forbidden(format!(
                    "unauthorized write to /peer/{actor_seg}/* by {}",
                    caller.id
                ))),
            }
        }
        ("shared", _) => Ok(()),
        ("leases", _) => Err(StoreError::Forbidden(
            "use lease_acquire, not kv_set on /leases/*".into(),
        )),
        _ => Err(StoreError::Forbidden(format!(
            "unknown namespace: /{namespace} (valid: /ref, /peer/<id>, /shared, /leases)"
        ))),
    }
}

/// Per-actor activity counters surfaced to the TUI so each grid tile can
/// show "kv:N lease:M" — a glanceable indicator of which workers are
/// actively using the shared store vs idle. Counters are monotonically
/// increasing for the lifetime of a run; kv covers get/set/cas/list/wait,
/// lease covers acquire+release. Incremented for both successful and
/// failed calls (authz/limit errors still count as "tried to use the
/// store") so operators can see frustration loops in the numbers.
///
/// **Bump-on-attempt semantics (#153 L9):** counters are bumped at handler
/// entry, *before* authz or execution. The TUI value answers "how many
/// store calls has this actor attempted" — not "how many succeeded" — so
/// a worker spinning on a Forbidden path or mid-op panic still shows as
/// active. Mid-handler panics that abort before the op leave the bump in
/// place by design (one attempt = one bump). Don't restructure to bump
/// after success; that would make stuck workers invisible in the TUI.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ActivityCounters {
    pub kv_ops: u64,
    pub lease_ops: u64,
}

/// Initial capacity for `notifier` (kv writes) — historically 256, raised
/// to 1024 in #153 L2 so a burst on the kv path is less likely to lag a
/// concurrent `wait()`. The Lagged-recovery path in `wait()` re-reads the
/// store before re-subscribing, so over-capacity is safe; this just
/// reduces the rate at which `wait()` falls into the slow path under load.
const KV_NOTIFIER_CAPACITY: usize = 1024;
/// Initial capacity for `lease_notifier`, raised from 64 to 256 in
/// #153 L2 for the same reasons as `KV_NOTIFIER_CAPACITY` — bursts of
/// `lease_release` shouldn't lag concurrent waiters into a slow-path
/// retry. The lease-acquire wait branch handles `Lagged` correctly
/// (consults the map directly) so this is a perf knob, not correctness.
const LEASE_NOTIFIER_CAPACITY: usize = 256;

pub struct SharedStore {
    entries: RwLock<HashMap<PathBuf, Entry>>,
    limits: Limits,
    notifier: broadcast::Sender<NotifyEvent>,
    cancel: CancellationToken,
    leases: LeaseRegistry,
    lease_notifier: broadcast::Sender<String>,
    /// Per-actor activity counters, keyed by `actor_id`. Protected by a
    /// standard RwLock (not the entries lock) so counter bumps don't
    /// contend with reads/writes.
    activity: RwLock<std::collections::HashMap<String, ActivityCounters>>,
    /// Set true by `start_lease_pruner`; consulted by `lease_acquire`
    /// when entering the wait branch so we can warn-once if a caller
    /// would block on TTL expiry without the pruner running. Without the
    /// pruner, a holder that crashes silently (no `release` call) leaves
    /// waiters blocked until *their own* deadline rather than the
    /// holder's. (#153 M1)
    pruner_started: AtomicBool,
    /// Logged-once latch for the "lease_acquire wait without pruner"
    /// warning so we don't spam logs in a tight retry loop.
    pruner_warning_logged: AtomicBool,
}

impl SharedStore {
    pub fn new() -> Self {
        Self::with_limits(Limits::default())
    }

    pub fn with_limits(limits: Limits) -> Self {
        let (notifier, _) = broadcast::channel(KV_NOTIFIER_CAPACITY);
        let (lease_notifier, _) = broadcast::channel(LEASE_NOTIFIER_CAPACITY);
        Self {
            entries: RwLock::new(HashMap::new()),
            limits,
            notifier,
            cancel: CancellationToken::new(),
            leases: LeaseRegistry::new(),
            lease_notifier,
            activity: RwLock::new(std::collections::HashMap::new()),
            pruner_started: AtomicBool::new(false),
            pruner_warning_logged: AtomicBool::new(false),
        }
    }

    /// Increment the kv counter for `actor_id`. Called from each `kv_*`
    /// MCP tool handler — no-op when the actor id is empty (defensive,
    /// shouldn't happen in practice since the bridge stamps `_meta`).
    pub async fn note_kv_op(&self, actor_id: &str) {
        if actor_id.is_empty() {
            return;
        }
        let mut activity = self.activity.write().await;
        activity.entry(actor_id.to_string()).or_default().kv_ops += 1;
    }

    /// Increment the lease counter for `actor_id`. Called from
    /// `lease_acquire` / `lease_release`.
    pub async fn note_lease_op(&self, actor_id: &str) {
        if actor_id.is_empty() {
            return;
        }
        let mut activity = self.activity.write().await;
        activity.entry(actor_id.to_string()).or_default().lease_ops += 1;
    }

    /// Snapshot the per-actor activity map. Used by the control server to
    /// push periodic updates to the TUI.
    pub async fn activity_snapshot(&self) -> std::collections::HashMap<String, ActivityCounters> {
        self.activity.read().await.clone()
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
        Ok(self.list_with_truncation(pattern).await?.entries)
    }

    /// Same match logic as `list`, but surfaces the pre-truncation match
    /// count and a `truncated` flag so callers can detect partial results.
    pub async fn list_with_truncation(&self, pattern: &str) -> Result<ListResult, StoreError> {
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
        let total_matched = out.len() as u64;
        let truncated = out.len() > LIST_RESULT_CAP;
        out.truncate(LIST_RESULT_CAP);
        Ok(ListResult {
            entries: out,
            truncated,
            total_matched,
        })
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
        // #153 L1: `Some(0)` would otherwise match any entry regardless of
        // staleness (`entry.version >= 0` is always true for present
        // entries). MCP callers passing `min_version: 0` almost always
        // mean "I don't care, give me whatever's there or wait for v1" —
        // identical to `None`. Coerce to 1 to make that intent explicit.
        let min = match min_version {
            Some(0) | None => 1,
            Some(n) => n,
        };

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
                        Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                            // A burst of unrelated writes evicted entries from
                            // the channel. The write that satisfies min may
                            // have been one of them — re-read the store
                            // before waiting again, or we'd block until
                            // timeout even when the condition is already met.
                            // #153 L7: log so operators can correlate the
                            // perf cliff with channel saturation.
                            tracing::warn!(
                                target: "pitboss::shared_store",
                                path = %key.display(),
                                skipped = skipped,
                                "kv-notifier Lagged in wait(); fell back to map re-read"
                            );
                            if let Some(entry) = self.entries.read().await.get(&key) {
                                if entry.version >= min {
                                    return Ok(entry.clone());
                                }
                            }
                            continue;
                        }
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

    /// Spawn a background task that periodically calls `prune_expired_now`
    /// and forwards evicted lease names to `lease_notifier`. Covers the
    /// "fully idle" window where no `acquire`/`release`/`list` call runs
    /// between a holder's crash and a waiter's timeout — without this,
    /// TTL-expired leases would not wake waiters until the waiter's own
    /// deadline fires.
    ///
    /// Runs until `shutdown()` is called. Safe to call at most once per
    /// store; subsequent calls spawn additional identical tasks (benign
    /// but wasteful). 500 ms is quick enough that a waiter's wake-up
    /// latency after a silent holder crash is bounded to a single tick.
    pub fn start_lease_pruner(self: &std::sync::Arc<Self>) {
        // #153 M1: latch so `lease_acquire` can detect missing-pruner
        // misconfiguration and warn operators rather than silently
        // blocking waiters until their own deadline.
        self.pruner_started.store(true, Ordering::Relaxed);
        let store = std::sync::Arc::clone(self);
        tokio::spawn(async move {
            const TICK: std::time::Duration = std::time::Duration::from_millis(500);
            loop {
                tokio::select! {
                    () = store.cancel.cancelled() => break,
                    () = tokio::time::sleep(TICK) => {
                        let evicted = store.leases.prune_expired_now().await;
                        if !evicted.is_empty() {
                            store.notify_lease_evictions(&evicted);
                        }
                    }
                }
            }
        });
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
            Ok((lease, evicted)) => {
                self.notify_lease_evictions(&evicted);
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

        // #153 M1: warn-once if the caller is about to block but no
        // background pruner is running. Without the pruner, a holder
        // that crashed silently leaves us waiting until our own
        // deadline rather than the holder's TTL. Logged once per store
        // so a misconfigured embedder hears about it without flooding.
        if !self.pruner_started.load(Ordering::Relaxed)
            && !self.pruner_warning_logged.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                target: "pitboss::shared_store",
                lease_name = name,
                "lease_acquire(wait=...) called but start_lease_pruner() was never called; \
                 silent holder crashes won't wake waiters until their own deadline"
            );
        }

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
                        Ok(Err(broadcast::error::RecvError::Closed)) => {
                            return Err(StoreError::Shutdown);
                        }
                        Ok(Ok(event_name)) => {
                            // #153 M2: per-name filter defeats the
                            // thundering herd. `lease_release` and
                            // `release_all_for_actor` send the lease
                            // *name* (#153 M3), so a release of
                            // unrelated lease "X" no longer wakes every
                            // waiter on lease "Y" into a wasted retry.
                            // Eviction events carry the evicted lease's
                            // name too (see `notify_lease_evictions`).
                            if event_name != name {
                                continue;
                            }
                            match self.leases.acquire(name, ttl, caller).await {
                                Ok((lease, evicted)) => {
                                    self.notify_lease_evictions(&evicted);
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
                        Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                            // #153 L3: Lagged is safe to recover from
                            // because retry consults the actual map
                            // state (not the broadcast stream). If our
                            // target lease was released during the
                            // skipped window, retry-acquire will see
                            // the empty slot and succeed; otherwise it
                            // returns Conflict and we re-subscribe.
                            // The audit's "may miss" concern is moot
                            // for that reason — the broadcast is a
                            // wake-up hint, not the source of truth.
                            // #153 L7: log so operators can correlate
                            // perf with channel saturation.
                            tracing::warn!(
                                target: "pitboss::shared_store",
                                lease_name = name,
                                skipped = skipped,
                                "lease-notifier Lagged in lease_acquire; \
                                 retrying acquire (map state is authoritative)"
                            );
                            match self.leases.acquire(name, ttl, caller).await {
                                Ok((lease, evicted)) => {
                                    self.notify_lease_evictions(&evicted);
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

    fn notify_lease_evictions(&self, names: &[String]) {
        for name in names {
            let _ = self.lease_notifier.send(name.clone());
        }
    }

    pub async fn lease_release(
        &self,
        lease_id: &str,
        caller: &CallerIdentity,
    ) -> Result<(), StoreError> {
        let (released_name, evicted) = self.leases.release(lease_id, caller).await?;
        self.notify_lease_evictions(&evicted);
        // #153 M3: send the lease *name* (not the lease_id UUID).
        // `lease_acquire` filters incoming events by the name it's
        // waiting on; a UUID would never match and waiters would block
        // to timeout instead of retrying.
        let _ = self.lease_notifier.send(released_name);
        Ok(())
    }

    /// Release all leases held by `actor_id`. Intended to be called when an
    /// actor's MCP connection drops (lease-on-connection semantics). Wakes
    /// any waiters subscribed via `lease_acquire(wait_secs > 0)`.
    ///
    /// **#153 L4 — caller contract (LOAD-BEARING):** this method is
    /// authorization-free by design — it is intended to be called from
    /// `mcp/server.rs` connection-cleanup paths where the bridge has
    /// already verified the dropped session's `actor_id`. Do not call it
    /// from MCP tool handlers, control-plane ops, or anywhere reachable
    /// from a worker prompt: a worker could trigger forcible release of
    /// another worker's leases. The audit recommended adding an auth
    /// token; that's deferred until we have a second legitimate caller.
    /// Until then, treat the bridge as the sole caller.
    pub async fn release_all_for_actor(&self, actor_id: &str) {
        let (released, evicted) = self.leases.release_all_for_actor(actor_id).await;
        self.notify_lease_evictions(&evicted);
        for name in released {
            let _ = self.lease_notifier.send(name);
        }
    }

    /// Write a JSON snapshot of all entries + leases to `path`. Values
    /// are base64-encoded (non-UTF-8 safe). Called at finalize time when
    /// `[run] dump_shared_store = true` is set. Pitboss never reads this
    /// back — it's for post-mortem and debugging only.
    ///
    /// **#153 L8 — snapshot consistency:** entries and leases are read
    /// under separate locks (entries lock released before leases lock is
    /// acquired). Concurrent writes between the two reads will appear
    /// in one half of the dump but not the other. Acceptable for
    /// post-mortem use because the dump fires at finalize time when
    /// most workers have already terminated, but **do not rely on this
    /// dump for cross-store invariants** — the entries snapshot may
    /// reference a lease that the leases snapshot already shows as
    /// released, or vice versa.
    pub async fn dump_to_path(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use base64::Engine as _;

        #[derive(Debug, Serialize)]
        struct DumpEntry {
            path: String,
            version: u64,
            written_by: String,
            written_at: DateTime<Utc>,
            value_b64: String,
        }

        #[derive(Debug, Serialize)]
        struct Dump {
            finalized_at: DateTime<Utc>,
            entries: Vec<DumpEntry>,
            leases: Vec<Lease>,
        }

        let entries = self.entries.read().await;
        let dump_entries: Vec<DumpEntry> = entries
            .iter()
            .map(|(k, e)| DumpEntry {
                path: k.to_string_lossy().to_string(),
                version: e.version,
                written_by: e.written_by.clone(),
                written_at: e.written_at,
                value_b64: base64::engine::general_purpose::STANDARD.encode(&e.value),
            })
            .collect();
        drop(entries);
        let (leases, evicted) = self.leases.list().await;
        self.notify_lease_evictions(&evicted);
        let dump = Dump {
            finalized_at: Utc::now(),
            entries: dump_entries,
            leases,
        };
        let bytes = serde_json::to_vec_pretty(&dump)?;
        tokio::fs::write(path, bytes).await?;
        Ok(())
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
    if path.split('/').any(|seg| seg == "..") {
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
        // Message must call out BOTH the target path's actor segment AND
        // the caller's actual actor_id so workers can self-correct without
        // a separate round-trip. Earlier message ("workers may write only
        // their own /peer/<self>/*") left workers guessing what <self>
        // resolves to — they burned turns experimenting with paths.
        let StoreError::Forbidden(msg) = err else {
            panic!("expected Forbidden, got {:?}", err);
        };
        assert!(
            msg.contains("w2") && msg.contains("w1"),
            "message must include target peer id (w2) and caller actor_id (w1): {msg}"
        );
        assert!(
            msg.contains("/peer/self"),
            "message should suggest /peer/self/... alias: {msg}"
        );
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

    #[tokio::test]
    async fn dump_writes_entries_and_leases_as_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("dump.json");
        let s = SharedStore::new();
        s.set("/ref/a", b"hello".to_vec(), "lead").await.unwrap();
        s.lease_acquire(
            "job-1",
            std::time::Duration::from_secs(30),
            None,
            &worker("w1"),
        )
        .await
        .unwrap();
        s.dump_to_path(&path).await.unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"path\": \"/ref/a\""));
        assert!(text.contains("\"value_b64\":"));
        assert!(text.contains("\"name\": \"job-1\""));
    }

    // -----------------------------------------------------------------------
    // Activity counters (v0.4.2+)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn activity_counters_start_empty() {
        let s = SharedStore::new();
        assert!(s.activity_snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn note_kv_op_increments_kv_counter() {
        let s = SharedStore::new();
        s.note_kv_op("worker-A").await;
        s.note_kv_op("worker-A").await;
        s.note_kv_op("worker-B").await;
        let snap = s.activity_snapshot().await;
        assert_eq!(snap.get("worker-A").unwrap().kv_ops, 2);
        assert_eq!(snap.get("worker-A").unwrap().lease_ops, 0);
        assert_eq!(snap.get("worker-B").unwrap().kv_ops, 1);
    }

    #[tokio::test]
    async fn note_lease_op_increments_lease_counter_independently() {
        let s = SharedStore::new();
        s.note_lease_op("worker-A").await;
        s.note_kv_op("worker-A").await;
        let snap = s.activity_snapshot().await;
        let counters = snap.get("worker-A").unwrap();
        assert_eq!(counters.kv_ops, 1);
        assert_eq!(counters.lease_ops, 1);
    }

    /// #153 L1 regression: `min_version=Some(0)` must be coerced to 1 so
    /// `wait` doesn't return immediately for any present entry.
    #[tokio::test]
    async fn wait_with_min_zero_treats_as_one() {
        let s = std::sync::Arc::new(SharedStore::new());
        // Pre-existing entry at version 1 should NOT short-circuit a
        // `min=Some(0)` waiter looking for v1+. (After coercion to 1
        // the present entry satisfies the wait, so this should succeed.)
        s.set("/ref/k", b"v1".to_vec(), "lead").await.unwrap();
        let entry = s
            .wait("/ref/k", Duration::from_millis(50), Some(0))
            .await
            .unwrap();
        assert_eq!(entry.version, 1);

        // Empty store + min=Some(0) must wait, not immediately return —
        // i.e. it behaves like None, not "match anything (including
        // nothing)".
        let s2 = SharedStore::new();
        let err = s2
            .wait("/ref/missing", Duration::from_millis(50), Some(0))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Timeout));
    }

    /// #153 M2 + M3 regression: a release of lease "X" must not cause a
    /// waiter on lease "Y" to wake up and burn a retry. With M3 fix the
    /// notifier carries names; with M2 fix `lease_acquire` filters by
    /// name. The proof is that a waiter on "Y" times out cleanly after
    /// many releases of "X" without spinning.
    #[tokio::test]
    async fn lease_acquire_waiter_filters_by_name() {
        let s = std::sync::Arc::new(SharedStore::new());
        // Hold "Y" so the waiter must actually wait.
        let y = s
            .lease_acquire(
                "Y",
                std::time::Duration::from_secs(30),
                None,
                &worker("holder-Y"),
            )
            .await
            .unwrap();
        assert!(y.acquired);

        // Spawn waiter for "Y".
        let s2 = s.clone();
        let waiter = tokio::spawn(async move {
            s2.lease_acquire(
                "Y",
                std::time::Duration::from_secs(30),
                Some(std::time::Duration::from_millis(150)),
                &worker("waiter"),
            )
            .await
        });

        // Generate noise on unrelated lease "X" — acquire and release
        // many times. Pre-fix this would wake the waiter on every
        // release for nothing.
        for i in 0..10 {
            let id = format!("noise-{i}");
            let res = s
                .lease_acquire("X", std::time::Duration::from_secs(30), None, &worker(&id))
                .await
                .unwrap();
            s.lease_release(&res.lease_id.unwrap(), &worker(&id))
                .await
                .unwrap();
        }

        // Waiter should still be blocked on "Y" and time out cleanly
        // (acquired=false), not get woken into a retry storm.
        let res = waiter.await.unwrap().unwrap();
        assert!(
            !res.acquired,
            "waiter should still be waiting on Y after X churn; got {:?}",
            res
        );
    }

    /// #153 M3 regression: `lease_release` must wake a waiter blocked on
    /// the same lease name. (Pre-fix it sent the lease_id UUID, which
    /// post-M2 would never match the name filter.)
    #[tokio::test]
    async fn lease_release_wakes_same_name_waiter() {
        let s = std::sync::Arc::new(SharedStore::new());
        let held = s
            .lease_acquire(
                "shared-resource",
                std::time::Duration::from_secs(30),
                None,
                &worker("holder"),
            )
            .await
            .unwrap();
        let lease_id = held.lease_id.unwrap();

        let s2 = s.clone();
        let waiter = tokio::spawn(async move {
            s2.lease_acquire(
                "shared-resource",
                std::time::Duration::from_secs(30),
                Some(std::time::Duration::from_secs(2)),
                &worker("waiter"),
            )
            .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        s.lease_release(&lease_id, &worker("holder")).await.unwrap();

        let res = waiter.await.unwrap().unwrap();
        assert!(res.acquired, "waiter should have acquired after release");
    }

    /// #153 M1 regression: pruner-started latch flips on
    /// `start_lease_pruner`.
    #[tokio::test]
    async fn pruner_latch_set_by_start_lease_pruner() {
        let s = std::sync::Arc::new(SharedStore::new());
        assert!(!s.pruner_started.load(std::sync::atomic::Ordering::Relaxed));
        s.start_lease_pruner();
        assert!(s.pruner_started.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn note_ops_ignore_empty_actor_id() {
        // Defensive: the bridge always stamps `_meta` so this shouldn't
        // happen in practice, but an empty actor_id would otherwise
        // create a bogus "" entry in the activity map.
        let s = SharedStore::new();
        s.note_kv_op("").await;
        s.note_lease_op("").await;
        assert!(s.activity_snapshot().await.is_empty());
    }
}
