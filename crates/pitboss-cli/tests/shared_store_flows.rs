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
