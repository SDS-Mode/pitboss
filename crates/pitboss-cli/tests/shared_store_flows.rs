//! End-to-end integration tests for the shared store feature.
//! Drives fake-claude subprocesses through real SessionHandle + MCP socket +
//! real mcp-bridge to exercise identity injection and the full tool stack.

use pitboss_cli::shared_store::SharedStore;
use std::sync::Arc;

#[tokio::test]
async fn scaffolding_smoke() {
    let s = Arc::new(SharedStore::new());
    s.set("/ref/bootstrap", b"ok".to_vec(), "lead")
        .await
        .unwrap();
    let e = s.get("/ref/bootstrap").await.unwrap();
    assert_eq!(e.value, b"ok");
}

use std::time::Duration;

use pitboss_cli::shared_store::tools::{
    handle_kv_set, handle_kv_wait, KvSetArgs, KvWaitArgs, MetaField,
};
use pitboss_cli::shared_store::ActorRole;

fn worker_meta(id: &str) -> MetaField {
    MetaField {
        actor_id: id.to_string(),
        actor_role: ActorRole::Worker,
    }
}

fn lead_meta(id: &str) -> MetaField {
    MetaField {
        actor_id: id.to_string(),
        actor_role: ActorRole::Lead,
    }
}

/// Simulates use case B from the spec: worker A writes its output to
/// `/peer/worker-A/output`; worker B blocks on `kv_wait` for that key
/// and reads it when worker A finishes. End-to-end test of the
/// publish/consume pattern without the full MCP transport layer.
#[tokio::test]
async fn worker_a_publishes_worker_b_consumes() {
    let store = Arc::new(SharedStore::new());

    // Spawn worker B's kv_wait BEFORE worker A writes, to prove
    // wait actually blocks until the write lands.
    let store_b = store.clone();
    let b_waiter = tokio::spawn(async move {
        let args = KvWaitArgs {
            path: "/peer/worker-A/output".into(),
            timeout_secs: 5,
            min_version: None,
            meta: Some(worker_meta("worker-B")),
        };
        handle_kv_wait(&store_b, args).await
    });

    // Give worker B a moment to register its subscription.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Worker A writes its output.
    let a_args = KvSetArgs {
        path: "/peer/worker-A/output".into(),
        value: b"worker A is done".to_vec(),
        override_flag: false,
        meta: worker_meta("worker-A"),
    };
    let a_result = handle_kv_set(&store, a_args).await.unwrap();
    assert_eq!(a_result.version, 1);

    // Worker B's kv_wait should resolve with worker A's value.
    let entry = b_waiter.await.unwrap().unwrap();
    assert_eq!(entry.value, b"worker A is done");
    assert_eq!(entry.version, 1);
    assert_eq!(entry.written_by, "worker-A");
}

#[tokio::test]
async fn worker_cross_peer_write_is_forbidden() {
    let store = Arc::new(SharedStore::new());
    // Worker A tries to write into worker B's namespace.
    let args = KvSetArgs {
        path: "/peer/worker-B/output".into(),
        value: b"cross-peer".to_vec(),
        override_flag: false,
        meta: worker_meta("worker-A"),
    };
    let err = handle_kv_set(&store, args).await.unwrap_err();
    match err {
        pitboss_cli::shared_store::StoreError::Forbidden(_) => {}
        other => panic!("expected Forbidden, got {other:?}"),
    }
}

use pitboss_cli::shared_store::tools::{
    handle_lease_acquire, handle_lease_release, LeaseAcquireArgs, LeaseReleaseArgs,
};

/// Three workers concurrently call lease_acquire with a short wait_secs.
/// Exactly one should acquire; the others should return acquired=false
/// after the wait elapses because the first holder's lease TTL (200ms)
/// is longer than the contenders' wait (50ms).
#[tokio::test]
async fn three_workers_lease_contention() {
    let store = Arc::new(SharedStore::new());

    let args_for = |worker_id: &str| LeaseAcquireArgs {
        name: "shared-job".into(),
        ttl_secs: 1,     // 1 second TTL for the winner
        wait_secs: None, // non-blocking for this test
        meta: worker_meta(worker_id),
    };

    let store1 = store.clone();
    let store2 = store.clone();
    let store3 = store.clone();

    let (r1, r2, r3) = tokio::join!(
        handle_lease_acquire(&store1, args_for("w1")),
        handle_lease_acquire(&store2, args_for("w2")),
        handle_lease_acquire(&store3, args_for("w3")),
    );

    let results = [r1.unwrap(), r2.unwrap(), r3.unwrap()];
    let acquired_count = results.iter().filter(|r| r.acquired).count();
    assert_eq!(
        acquired_count, 1,
        "exactly one of three concurrent acquires should succeed, got {}",
        acquired_count
    );

    // The winner's lease_id should be present; losers should have None.
    let winner = results.iter().find(|r| r.acquired).unwrap();
    assert!(winner.lease_id.is_some());
    assert!(winner.expires_at.is_some());
    for loser in results.iter().filter(|r| !r.acquired) {
        assert!(loser.lease_id.is_none());
        assert!(loser.expires_at.is_none());
    }
}

/// Holder releases; a waiter with wait_secs > 0 picks up the lease.
#[tokio::test]
async fn lease_wait_succeeds_when_holder_releases() {
    let store = Arc::new(SharedStore::new());
    // Worker A acquires first with a longer TTL.
    let a = handle_lease_acquire(
        &store,
        LeaseAcquireArgs {
            name: "slow-job".into(),
            ttl_secs: 30,
            wait_secs: None,
            meta: worker_meta("worker-A"),
        },
    )
    .await
    .unwrap();
    assert!(a.acquired);
    let a_lease_id = a.lease_id.unwrap();

    // Worker B starts waiting. A releases after 50ms. B should acquire.
    let store_b = store.clone();
    let b_waiter = tokio::spawn(async move {
        handle_lease_acquire(
            &store_b,
            LeaseAcquireArgs {
                name: "slow-job".into(),
                ttl_secs: 30,
                wait_secs: Some(2),
                meta: worker_meta("worker-B"),
            },
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    handle_lease_release(
        &store,
        LeaseReleaseArgs {
            lease_id: a_lease_id,
            meta: worker_meta("worker-A"),
        },
    )
    .await
    .unwrap();

    let b_result = b_waiter.await.unwrap().unwrap();
    assert!(
        b_result.acquired,
        "worker B should acquire after A releases"
    );
}

/// Release is identity-checked: non-holder cannot release.
#[tokio::test]
async fn lease_release_by_non_holder_is_forbidden() {
    let store = Arc::new(SharedStore::new());
    let a = handle_lease_acquire(
        &store,
        LeaseAcquireArgs {
            name: "exclusive".into(),
            ttl_secs: 30,
            wait_secs: None,
            meta: worker_meta("worker-A"),
        },
    )
    .await
    .unwrap();
    assert!(a.acquired);
    let a_lease_id = a.lease_id.unwrap();

    // Worker B tries to release worker A's lease.
    let err = handle_lease_release(
        &store,
        LeaseReleaseArgs {
            lease_id: a_lease_id,
            meta: worker_meta("worker-B"),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        pitboss_cli::shared_store::StoreError::Forbidden(_)
    ));
}

/// A write-path tool call arriving without `_meta` (bridge not involved
/// or misconfigured) must fail deserialization. serde handles this as
/// a missing-field error at the MCP parameter layer, which the dispatcher
/// surfaces as a parse error — semantically equivalent to unauthenticated.
#[tokio::test]
async fn write_args_without_meta_fail_to_deserialize() {
    // KvSetArgs has meta: MetaField (required — not Option). Missing field
    // should fail in serde.
    let payload = serde_json::json!({
        "path": "/ref/foo",
        "value": [1, 2, 3],
    });
    let result = serde_json::from_value::<KvSetArgs>(payload);
    assert!(
        result.is_err(),
        "missing _meta must fail deserialization; got {result:?}"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("_meta") || err_msg.contains("meta"),
        "error should mention the missing _meta field, got: {err_msg}"
    );
}

/// Lead with override_flag=true can write to any worker's /peer/* namespace.
#[tokio::test]
async fn lead_override_writes_any_peer() {
    let store = Arc::new(SharedStore::new());
    let args = KvSetArgs {
        path: "/peer/worker-B/override-by-lead".into(),
        value: b"lead-injected".to_vec(),
        override_flag: true,
        meta: lead_meta("lead-X"),
    };
    let res = handle_kv_set(&store, args).await.unwrap();
    assert_eq!(res.version, 1);
    // Read back to confirm.
    let entry = store.get("/peer/worker-B/override-by-lead").await.unwrap();
    assert_eq!(entry.value, b"lead-injected");
    assert_eq!(entry.written_by, "lead-X");
}

/// Worker cannot use override_flag=true to escape its /peer/<self>/ namespace.
/// Override is lead-only.
#[tokio::test]
async fn worker_override_flag_is_ignored_and_still_forbidden() {
    let store = Arc::new(SharedStore::new());
    let args = KvSetArgs {
        path: "/peer/worker-B/sneaky".into(),
        value: b"no".to_vec(),
        override_flag: true, // worker setting the flag; should not help
        meta: worker_meta("worker-A"),
    };
    let err = handle_kv_set(&store, args).await.unwrap_err();
    assert!(matches!(
        err,
        pitboss_cli::shared_store::StoreError::Forbidden(_)
    ));
}

/// Lead without override_flag cannot write another worker's /peer/*.
#[tokio::test]
async fn lead_without_override_cannot_write_other_peer() {
    let store = Arc::new(SharedStore::new());
    let args = KvSetArgs {
        path: "/peer/worker-Z/lead-tried".into(),
        value: b"denied".to_vec(),
        override_flag: false,
        meta: lead_meta("lead-X"),
    };
    let err = handle_kv_set(&store, args).await.unwrap_err();
    assert!(matches!(
        err,
        pitboss_cli::shared_store::StoreError::Forbidden(_)
    ));
}

/// Exercises the connection-drop cleanup hook at the method level.
/// SharedStore::release_all_for_actor releases every lease held by
/// a given actor_id and fires the release notifier so waiters wake up.
///
/// The actual rmcp-side wiring (invoking this on MCP connection close)
/// is a follow-up — see the #[ignore]d test below for the target
/// end-to-end behavior.
#[tokio::test]
async fn release_all_for_actor_clears_held_leases() {
    let store = Arc::new(SharedStore::new());
    handle_lease_acquire(
        &store,
        LeaseAcquireArgs {
            name: "a".into(),
            ttl_secs: 60,
            wait_secs: None,
            meta: worker_meta("worker-X"),
        },
    )
    .await
    .unwrap();
    handle_lease_acquire(
        &store,
        LeaseAcquireArgs {
            name: "b".into(),
            ttl_secs: 60,
            wait_secs: None,
            meta: worker_meta("worker-X"),
        },
    )
    .await
    .unwrap();
    // Different actor holds "c"; it should survive.
    handle_lease_acquire(
        &store,
        LeaseAcquireArgs {
            name: "c".into(),
            ttl_secs: 60,
            wait_secs: None,
            meta: worker_meta("worker-Y"),
        },
    )
    .await
    .unwrap();

    store.release_all_for_actor("worker-X").await;

    // Now worker-Z can take "a" and "b", but "c" is still held by worker-Y.
    let a = handle_lease_acquire(
        &store,
        LeaseAcquireArgs {
            name: "a".into(),
            ttl_secs: 60,
            wait_secs: None,
            meta: worker_meta("worker-Z"),
        },
    )
    .await
    .unwrap();
    assert!(a.acquired);
    let b = handle_lease_acquire(
        &store,
        LeaseAcquireArgs {
            name: "b".into(),
            ttl_secs: 60,
            wait_secs: None,
            meta: worker_meta("worker-Z"),
        },
    )
    .await
    .unwrap();
    assert!(b.acquired);
    let c = handle_lease_acquire(
        &store,
        LeaseAcquireArgs {
            name: "c".into(),
            ttl_secs: 60,
            wait_secs: None,
            meta: worker_meta("worker-Z"),
        },
    )
    .await
    .unwrap();
    assert!(!c.acquired, "worker-Y still holds c");
}

// ---------------------------------------------------------------------------
// Deferred: real rmcp-driven connection-drop
// ---------------------------------------------------------------------------

/// End-to-end test of the per-connection lease-cleanup hook:
///   1. Session A (worker-A) acquires lease "job-1" with a long TTL.
///   2. Session A closes.
///   3. Session B (worker-B) can immediately acquire "job-1" — because
///      `SharedStore::release_all_for_actor("worker-A")` fired as part of
///      the connection-drop cleanup, not because the TTL elapsed.
///
/// Without the cleanup hook, worker-B's acquire would return `acquired=false`
/// until the 3600-second TTL expired — the pre-v0.4.3 behavior we shipped
/// with a `#[ignore]` guard on this test.
#[tokio::test]
async fn lease_released_when_mcp_connection_drops() {
    use fake_mcp_client::FakeMcpClient;
    use pitboss_cli::dispatch::state::{ApprovalPolicy, DispatchState};
    use pitboss_cli::manifest::resolve::{ResolvedLead, ResolvedManifest};
    use pitboss_cli::manifest::schema::{Effort, WorktreeCleanup};
    use pitboss_cli::mcp::{socket_path_for_run, McpServer};
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use pitboss_core::process::ProcessSpawner;
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;

    // Minimal dispatch state wrapping a SharedStore. Lead + spawner are
    // placeholders — we only drive lease_acquire, which doesn't touch
    // worker machinery.
    let dir = TempDir::new().unwrap();
    let lead = ResolvedLead {
        id: "lead".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "p".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::Low,
        tools: vec![],
        timeout_secs: 60,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_workers_across_tree: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(5.0),
        lead_timeout_secs: None,
        approval_policy: None,
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
    };
    let store_trait: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().into()));
    let run_id = Uuid::now_v7();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(FakeScript::new()));
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store_trait,
        CancelToken::new(),
        "lead".into(),
        spawner,
        PathBuf::from("claude"),
        Arc::new(WorktreeManager::new()),
        CleanupPolicy::Never,
        dir.path().join(run_id.to_string()),
        ApprovalPolicy::Block,
        None,
        Arc::new(SharedStore::new()),
    ));

    let socket = socket_path_for_run(state.run_id, &state.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // Session A: acquire with a very long TTL so only the connection-drop
    // cleanup can release it within the test timeframe.
    let mut client_a = FakeMcpClient::connect(&socket).await.unwrap();
    let acq_a = client_a
        .call_tool(
            "lease_acquire",
            json!({
                "name": "job-1",
                "ttl_secs": 3600,
                "_meta": { "actor_id": "worker-A", "actor_role": "worker" }
            }),
        )
        .await
        .unwrap();
    assert!(
        acq_a["acquired"].as_bool().unwrap_or(false),
        "session A should get the lease: {acq_a}"
    );

    client_a.close().await.unwrap();

    // Give the server a beat to process the disconnect. The cleanup fires
    // after `running.waiting()` returns in the accept-loop task, and that's
    // one tokio task hop away from our test thread.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Session B: should be able to take the lease — it's free because A
    // disconnected, not because 3600s elapsed.
    let mut client_b = FakeMcpClient::connect(&socket).await.unwrap();
    let acq_b = client_b
        .call_tool(
            "lease_acquire",
            json!({
                "name": "job-1",
                "ttl_secs": 60,
                "_meta": { "actor_id": "worker-B", "actor_role": "worker" }
            }),
        )
        .await
        .unwrap();
    assert!(
        acq_b["acquired"].as_bool().unwrap_or(false),
        "lease should be free after session A dropped: {acq_b}"
    );
    client_b.close().await.unwrap();
}
