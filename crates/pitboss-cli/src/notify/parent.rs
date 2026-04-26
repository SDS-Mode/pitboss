//! Parent-orchestrator notify hook (issue #133).
//!
//! When a parent process (Discord bot, web dashboard, dispatcher service)
//! wraps pitboss as a sub-component, it needs to know about runs the agent
//! itself spawns from inside its task worktree (`pitboss dispatch
//! child.toml`). Without this hook, the orchestrator's `runs` table, budget
//! gates, and retention sweeps never see those sub-runs.
//!
//! Two env vars implement the contract:
//!
//! - `PITBOSS_PARENT_NOTIFY_URL` — set by the orchestrator before launching
//!   pitboss. When present, every `pitboss dispatch` invocation (top-level
//!   AND any nested call from inside a worktree) builds an ephemeral
//!   webhook sink targeting that URL and emits at run start
//!   ([`PitbossEvent::RunDispatched`]) and run end
//!   ([`PitbossEvent::RunFinished`]). The sink runs alongside any
//!   manifest-declared `[[notification]]` sinks — they don't conflict.
//!
//! - `PITBOSS_RUN_ID` — propagated automatically into every spawned
//!   claude subprocess's env. A nested `pitboss dispatch` from inside that
//!   subprocess inherits the value and reports it as `parent_run_id` on the
//!   `RunDispatched` event so the orchestrator can correlate parent ↔ child.
//!   Top-level dispatches see the var unset and report `parent_run_id =
//!   None`.
//!
//! ## Why bypass the SSRF guard
//!
//! Manifest-declared `[[notification]]` URLs go through
//! [`super::config::validate_webhook_url`] which rejects loopback / private
//! / link-local hosts (defense against a hostile manifest exfiltrating to
//! the host's internal network). The env var path bypasses that guard
//! intentionally: `http://localhost:N` is the canonical orchestrator
//! topology — agent and orchestrator share a host — and an env var can
//! only be set by the operator running pitboss, not by anything the agent
//! produces. Trust scope differs, so the validation rules differ.

use std::sync::Arc;

use anyhow::{Context, Result};

use super::config::NotificationConfig;
use super::sinks::WebhookSink;
use super::{NotificationRouter, NotificationSink, Severity, SinkFilter};

/// Env var name read at dispatch start to pick up the parent orchestrator's
/// notification target.
pub const PARENT_NOTIFY_URL_ENV: &str = "PITBOSS_PARENT_NOTIFY_URL";

/// Env var name pitboss sets in every spawned claude subprocess's env so
/// nested `pitboss dispatch` calls inherit the parent run id and report it
/// on `RunDispatched`.
pub const RUN_ID_ENV: &str = "PITBOSS_RUN_ID";

/// Build a webhook sink from `PITBOSS_PARENT_NOTIFY_URL` if set. Returns
/// `None` when the env var is absent or empty (so a top-level dispatch
/// without a parent orchestrator pays no overhead). The returned sink
/// bypasses the per-request SSRF guard — see the module-level comment for
/// why that's safe.
pub fn build_parent_sink(http: &Arc<reqwest::Client>) -> Option<Arc<dyn NotificationSink>> {
    let url = std::env::var(PARENT_NOTIFY_URL_ENV).ok()?;
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(Arc::new(WebhookSink::new_trusted(
        "parent-notify".to_string(),
        trimmed.to_string(),
        Arc::clone(http),
    )))
}

/// Read `PITBOSS_RUN_ID` from the current process env. Used at dispatch
/// start to populate `RunDispatched.parent_run_id`. `None` for top-level
/// dispatches (env var unset or empty).
pub fn parent_run_id() -> Option<String> {
    std::env::var(RUN_ID_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Set `PITBOSS_RUN_ID` in the current process env so any pitboss-spawned
/// claude subprocess (and any nested `pitboss dispatch` invoked from inside
/// that subprocess) inherits the value. `tokio::process::Command::envs()`
/// adds keys without clearing the inherited env, so a single set here
/// propagates through the whole spawn tree without per-site plumbing.
pub fn set_run_id_env(run_id: &str) {
    std::env::set_var(RUN_ID_ENV, run_id);
}

/// Build a [`NotificationRouter`] combining manifest `[[notification]]`
/// sinks with the optional `PITBOSS_PARENT_NOTIFY_URL`-derived sink.
///
/// Returns `None` when neither source contributes a sink (no manifest
/// notifications declared and the env var is unset) — callers can skip
/// router instantiation entirely in the common case.
///
/// The parent-notify sink is added LAST so its filter (all events,
/// severity_min = Info) doesn't affect ordering of manifest sinks. It
/// receives every event the dispatcher emits.
pub fn build_router(
    manifest_sinks: &[NotificationConfig],
    http: &Arc<reqwest::Client>,
) -> Result<Option<Arc<NotificationRouter>>> {
    let mut sinks: Vec<(Arc<dyn NotificationSink>, SinkFilter)> = manifest_sinks
        .iter()
        .enumerate()
        .map(|(idx, cfg)| {
            let sink = super::sinks::build(cfg, idx, http).context("build notification sink")?;
            let filter = SinkFilter::from(cfg);
            Ok::<_, anyhow::Error>((sink, filter))
        })
        .collect::<Result<_>>()?;

    if let Some(parent_sink) = build_parent_sink(http) {
        // Open filter — operator orchestrators want every signal pitboss can give.
        sinks.push((
            parent_sink,
            SinkFilter {
                events: None,
                severity_min: Severity::Info,
            },
        ));
    }

    if sinks.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Arc::new(NotificationRouter::new(sinks))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All tests that read or write the parent-notify env vars must hold this
    /// mutex for their duration. `cargo test` runs unit tests in parallel
    /// within the same process, and env vars are global — without serialization
    /// these tests trample each other (a `remove_var` in one races a
    /// `set_var` in another). `tokio::sync::Mutex` rather than `std::sync` so
    /// the async tests can hold the guard across `.await` points without
    /// tripping clippy's `await_holding_lock`.
    static ENV_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn alock() -> tokio::sync::MutexGuard<'static, ()> {
        ENV_GUARD.lock().await
    }

    fn lock() -> tokio::sync::MutexGuard<'static, ()> {
        // Sync tests block on the runtime-less mutex via try_lock loop. In
        // practice the guard is rarely contended (these tests are fast).
        loop {
            match ENV_GUARD.try_lock() {
                Ok(g) => return g,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
    }

    #[test]
    fn parent_run_id_returns_none_when_unset() {
        let _g = lock();
        std::env::remove_var(RUN_ID_ENV);
        assert!(parent_run_id().is_none());
    }

    #[test]
    fn parent_run_id_returns_some_when_set() {
        let _g = lock();
        std::env::set_var(RUN_ID_ENV, "  019d0000-aaaa-bbbb-cccc-dddddddddddd  ");
        let v = parent_run_id();
        std::env::remove_var(RUN_ID_ENV);
        assert_eq!(v.as_deref(), Some("019d0000-aaaa-bbbb-cccc-dddddddddddd"));
    }

    #[test]
    fn parent_run_id_returns_none_when_empty() {
        let _g = lock();
        std::env::set_var(RUN_ID_ENV, "   ");
        let v = parent_run_id();
        std::env::remove_var(RUN_ID_ENV);
        assert!(v.is_none());
    }

    #[test]
    fn build_parent_sink_returns_none_when_env_unset() {
        let _g = lock();
        std::env::remove_var(PARENT_NOTIFY_URL_ENV);
        let http = Arc::new(reqwest::Client::new());
        assert!(build_parent_sink(&http).is_none());
    }

    #[tokio::test]
    async fn parent_sink_posts_run_dispatched_to_localhost() {
        let _g = alock().await;
        // End-to-end: env var → trusted webhook sink → POST to a local mock.
        // Verifies that the SSRF bypass actually lets a localhost target work
        // (the manifest path would refuse it at parse time).
        use crate::notify::{NotificationEnvelope, PitbossEvent, Severity};
        use chrono::Utc;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/notify"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&mock)
            .await;

        let url = format!("{}/notify", mock.uri());
        std::env::set_var(PARENT_NOTIFY_URL_ENV, &url);
        let http = Arc::new(reqwest::Client::new());
        let sink = build_parent_sink(&http).expect("env-derived sink");
        std::env::remove_var(PARENT_NOTIFY_URL_ENV);

        let env = NotificationEnvelope::new(
            "child-run-1",
            Severity::Info,
            PitbossEvent::RunDispatched {
                run_id: "child-run-1".into(),
                parent_run_id: Some("parent-run-7".into()),
                manifest_path: "/work/child.toml".into(),
                mode: "flat".into(),
            },
            Utc::now(),
        );
        sink.emit(&env).await.expect("POST should succeed");
    }

    #[tokio::test]
    async fn parent_sink_bypasses_https_only_check() {
        let _g = alock().await;
        // Manifest webhook validation rejects http:// + loopback. The env-var
        // path must accept both, since the canonical orchestrator topology is
        // `http://localhost:N` on the same host.
        use crate::notify::{NotificationEnvelope, PitbossEvent, Severity};
        use chrono::Utc;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock)
            .await;

        // mock.uri() returns http://127.0.0.1:<port> — exactly the case the
        // SSRF guard refuses for manifest URLs.
        std::env::set_var(PARENT_NOTIFY_URL_ENV, mock.uri());
        let http = Arc::new(reqwest::Client::new());
        let sink = build_parent_sink(&http).expect("env-derived sink");
        std::env::remove_var(PARENT_NOTIFY_URL_ENV);

        let env = NotificationEnvelope::new(
            "run-x",
            Severity::Info,
            PitbossEvent::RunFinished {
                run_id: "run-x".into(),
                tasks_total: 1,
                tasks_failed: 0,
                duration_ms: 100,
                spent_usd: 0.0,
            },
            Utc::now(),
        );
        // Without the bypass this would fail with "private / loopback".
        sink.emit(&env).await.expect("loopback POST must succeed");
    }

    #[test]
    fn build_router_returns_none_when_no_sources() {
        let _g = lock();
        std::env::remove_var(PARENT_NOTIFY_URL_ENV);
        let http = Arc::new(reqwest::Client::new());
        let router = build_router(&[], &http).unwrap();
        assert!(router.is_none());
    }

    #[test]
    fn build_router_includes_parent_sink_when_env_set() {
        let _g = lock();
        std::env::set_var(PARENT_NOTIFY_URL_ENV, "http://127.0.0.1:9/x");
        let http = Arc::new(reqwest::Client::new());
        let router = build_router(&[], &http).unwrap();
        std::env::remove_var(PARENT_NOTIFY_URL_ENV);
        assert!(
            router.is_some(),
            "router should be built when only env-var sink contributes"
        );
    }
}
