//! Integration test for the control-socket → SSE bridge happy path.
//!
//! Spins up a fake dispatcher (UnixListener) at a temp path that mimics
//! the per-run `<run_id>/control.sock` layout, then drives the bridge
//! with subscribe → reader → assert-on-receive. No real `pitboss
//! dispatch` needed.
//!
//! The fake dispatcher only implements what the bridge cares about:
//! - Accept ONE connection.
//! - Read the client's `Hello` line (and discard it; verifying we sent
//!   one is sufficient).
//! - Write a fixed sequence of events as line-delimited JSON.
//! - Hold the connection open until the bridge drops it.

use std::time::Duration;

use pitboss_cli::control::protocol::{ControlEvent, ControlOp, EventEnvelope};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::time::timeout;

// Re-import the bridge from our own crate. Note: this requires
// `control_bridge` to be a `pub mod` in lib.rs OR a `pub(crate)` mod
// reachable via a tests-only re-export. We expose it via a tests-only
// shim to keep the binary surface clean.
//
// Bridge is normally a private module of the bin crate; for this
// integration test we hard-link it via #[path]. Cargo treats `tests/`
// as separate compilation units that can pull in any module by path.
#[path = "../src/control_bridge.rs"]
mod control_bridge;

use control_bridge::{BridgeError, ControlBridge};

#[tokio::test]
async fn bridge_subscribe_returns_dispatcher_events() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_dir = tmp.path().to_path_buf();
    let run_id = "01950000-0000-7000-8000-000000000001";
    let run_dir = runs_dir.join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();
    let socket_path = run_dir.join("control.sock");

    let listener = UnixListener::bind(&socket_path).unwrap();

    // Spawn the fake dispatcher.
    let server_handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read).lines();

        // Read the Hello sent by the bridge — assert non-empty.
        let hello_line = reader
            .next_line()
            .await
            .expect("read hello")
            .expect("hello present");
        assert!(
            hello_line.contains("\"op\":\"hello\""),
            "missing hello op: {hello_line}"
        );

        // Write back a Hello event (pre-v0.6 bare ControlEvent shape).
        let hello_event = ControlEvent::Hello {
            server_version: "test-0.0.1".into(),
            run_id: "01950000-0000-7000-8000-000000000001".into(),
            run_kind: "flat".into(),
            workers: vec!["w-1".into()],
            policy_rules: vec![],
        };
        let mut line = serde_json::to_string(&hello_event).unwrap();
        line.push('\n');
        write.write_all(line.as_bytes()).await.unwrap();

        // Then a WorkerFailed event for variety.
        let worker_failed = ControlEvent::WorkerFailed {
            task_id: "w-1".into(),
            parent_task_id: None,
            reason: pitboss_core::store::FailureReason::AuthFailure,
        };
        let mut line2 = serde_json::to_string(&worker_failed).unwrap();
        line2.push('\n');
        write.write_all(line2.as_bytes()).await.unwrap();

        // Hold the connection until the bridge drops.
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    // Drive the bridge from the client side.
    let bridge = ControlBridge::new(runs_dir);
    let mut rx = bridge.subscribe(run_id).await.expect("subscribe");

    // First event should be the Hello.
    let envelope: EventEnvelope = timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("hello timeout")
        .expect("hello recv");
    match envelope.event {
        ControlEvent::Hello { server_version, .. } => {
            assert_eq!(server_version, "test-0.0.1");
        }
        other => panic!("expected Hello, got {other:?}"),
    }

    // Second event: WorkerFailed.
    let envelope: EventEnvelope = timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("failure timeout")
        .expect("failure recv");
    match envelope.event {
        ControlEvent::WorkerFailed { task_id, .. } => {
            assert_eq!(task_id, "w-1");
        }
        other => panic!("expected WorkerFailed, got {other:?}"),
    }

    drop(rx);
    drop(bridge);
    let _ = server_handle.await;
}

#[tokio::test]
async fn bridge_returns_not_found_when_no_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let bridge = ControlBridge::new(tmp.path().to_path_buf());
    let err = bridge
        .subscribe("nonexistent-run-id-12345")
        .await
        .expect_err("should fail");
    assert!(matches!(err, BridgeError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn bridge_subscribe_shares_connection_for_multiple_subscribers() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_dir = tmp.path().to_path_buf();
    let run_id = "01950000-0000-7000-8000-000000000002";
    let run_dir = runs_dir.join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();
    let socket_path = run_dir.join("control.sock");

    let listener = UnixListener::bind(&socket_path).unwrap();

    // Track how many connections the dispatcher accepts. Should be 1
    // even though we subscribe twice.
    let server_handle = tokio::spawn(async move {
        let mut accepted = 0u32;
        loop {
            tokio::select! {
                accept = listener.accept() => {
                    if let Ok((stream, _)) = accept {
                        accepted += 1;
                        let (_read, mut write) = stream.into_split();
                        // Single broadcast event so both subscribers see something.
                        let ev = ControlEvent::Hello {
                            server_version: "shared".into(),
                            run_id: "shared".into(),
                            run_kind: "flat".into(),
                            workers: vec![],
                            policy_rules: vec![],
                        };
                        let mut line = serde_json::to_string(&ev).unwrap();
                        line.push('\n');
                        let _ = write.write_all(line.as_bytes()).await;
                        // Hold the connection.
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(3)) => break accepted,
            }
        }
    });

    let bridge = ControlBridge::new(runs_dir);
    let mut rx1 = bridge.subscribe(run_id).await.expect("subscribe 1");
    let mut rx2 = bridge.subscribe(run_id).await.expect("subscribe 2");

    let e1 = timeout(Duration::from_secs(2), rx1.recv())
        .await
        .expect("rx1")
        .expect("rx1 recv");
    let e2 = timeout(Duration::from_secs(2), rx2.recv())
        .await
        .expect("rx2")
        .expect("rx2 recv");

    assert!(matches!(e1.event, ControlEvent::Hello { .. }));
    assert!(matches!(e2.event, ControlEvent::Hello { .. }));

    drop(rx1);
    drop(rx2);
    drop(bridge);

    let accepted = server_handle.await.unwrap();
    assert_eq!(
        accepted, 1,
        "bridge must reuse connection across subscribers"
    );
}

#[tokio::test]
async fn bridge_send_op_round_trips_through_dispatcher() {
    // End-to-end: subscribe to the bridge, send an op via send_op, the
    // fake dispatcher echoes an OpAcked, the bridge's reader_loop
    // delivers the OpAcked envelope to the subscriber. This exercises
    // the full Phase 3 control-write path without any side-channel
    // assertions on raw socket bytes (which were flaky under tokio's
    // current_thread test runtime — the broadcast receiver path is the
    // one the SPA actually relies on).
    let tmp = tempfile::tempdir().unwrap();
    let runs_dir = tmp.path().to_path_buf();
    let run_id = "01950000-0000-7000-8000-000000000003";
    let run_dir = runs_dir.join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();
    let socket_path = run_dir.join("control.sock");

    let listener = UnixListener::bind(&socket_path).unwrap();

    let server_handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read).lines();
        // Read until we see a cancel_worker, then ack it. Dispatcher
        // implementations always respond to a recognised op with either
        // OpAcked or OpFailed; we only need the happy path here.
        while let Ok(Some(line)) = reader.next_line().await {
            if line.contains("\"op\":\"cancel_worker\"") {
                let ack = ControlEvent::OpAcked {
                    op: "cancel_worker".into(),
                    task_id: Some("w-1".into()),
                };
                let mut s = serde_json::to_string(&ack).unwrap();
                s.push('\n');
                let _ = write.write_all(s.as_bytes()).await;
                break;
            }
        }
        // Hold the connection long enough for the bridge's reader to
        // drain the ack into the broadcast channel before we EOF.
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let bridge = ControlBridge::new(runs_dir);
    // Subscribe FIRST so the dispatcher's OpAcked is visible to the
    // receiver — tokio broadcast only delivers messages sent after a
    // receiver is created.
    let mut rx = bridge.subscribe(run_id).await.expect("subscribe");

    bridge
        .send_op(
            run_id,
            &ControlOp::CancelWorker {
                task_id: "w-1".into(),
            },
        )
        .await
        .expect("send_op");

    let envelope = timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("recv timeout")
        .expect("recv chan");
    match envelope.event {
        ControlEvent::OpAcked { op, task_id } => {
            assert_eq!(op, "cancel_worker");
            assert_eq!(task_id.as_deref(), Some("w-1"));
        }
        other => panic!("expected OpAcked, got {other:?}"),
    }

    drop(rx);
    drop(bridge);
    let _ = server_handle.await;
}

#[tokio::test]
async fn bridge_send_op_rejects_client_hello() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_dir = tmp.path().to_path_buf();
    let run_id = "01950000-0000-7000-8000-000000000004";
    let run_dir = runs_dir.join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();

    // Bind a listener so the path exists but never accept — Rejected
    // must fire on the up-front variant check before any IO.
    let _listener = UnixListener::bind(run_dir.join("control.sock")).unwrap();

    let bridge = ControlBridge::new(runs_dir);
    let err = bridge
        .send_op(
            run_id,
            &ControlOp::Hello {
                client_version: "spoof/0.0.0".into(),
            },
        )
        .await
        .expect_err("hello must be rejected");
    assert!(matches!(err, BridgeError::Rejected(_)), "got {err:?}");
}
