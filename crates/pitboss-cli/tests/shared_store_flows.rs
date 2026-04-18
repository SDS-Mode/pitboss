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
