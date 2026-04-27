//! 30-second LRU cache around [`AggregateSet::build`]. Re-runs the
//! filesystem walk only when:
//!
//! - the cached entry is older than [`CACHE_TTL`], OR
//! - the runs-dir mtime has changed since the cache was filled.
//!
//! Single-instance (one cache per [`AppState`]). Holds the unfiltered
//! aggregate; per-request filtering is done in
//! [`super::aggregator::AggregateSet::apply_filter`] which is cheap.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use super::aggregator::AggregateSet;

const CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct Entry {
    set: AggregateSet,
    cached_at: Instant,
    runs_dir_mtime: Option<SystemTime>,
}

/// Process-global aggregator cache. Created once per [`AppState`] (the
/// state holds it inside an `Arc`).
#[derive(Debug, Default)]
pub struct InsightsCache {
    entry: Mutex<Option<Entry>>,
}

impl InsightsCache {
    pub fn new() -> Self {
        Self {
            entry: Mutex::new(None),
        }
    }

    /// Return the aggregate for `runs_dir`. Rebuilds when stale.
    /// Cloning the `AggregateSet` keeps the lock window short — the
    /// vectors are small (one entry per run, one per failure) and the
    /// alternative (handing out an `Arc<AggregateSet>`) would force
    /// every consumer to deal with `Arc` semantics for filtering.
    pub fn get(&self, runs_dir: &Path) -> AggregateSet {
        let dir_mtime = runs_dir_mtime(runs_dir);
        let now = Instant::now();
        {
            let guard = self.entry.lock().expect("insights cache poisoned");
            if let Some(entry) = guard.as_ref() {
                let fresh_age = now.duration_since(entry.cached_at) < CACHE_TTL;
                let same_mtime = entry.runs_dir_mtime == dir_mtime;
                if fresh_age && same_mtime {
                    return entry.set.clone();
                }
            }
        }
        let set = AggregateSet::build(runs_dir);
        let mut guard = self.entry.lock().expect("insights cache poisoned");
        *guard = Some(Entry {
            set: set.clone(),
            cached_at: Instant::now(),
            runs_dir_mtime: dir_mtime,
        });
        set
    }

    #[allow(dead_code)] // Wired up when the SSE invalidator lands.
    pub fn invalidate(&self) {
        if let Ok(mut g) = self.entry.lock() {
            *g = None;
        }
    }
}

fn runs_dir_mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

#[allow(dead_code)]
fn ensure_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<InsightsCache>();
    let _ = PathBuf::new();
}
