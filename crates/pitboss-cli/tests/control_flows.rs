//! Dispatcher-side control-socket integration tests. Uses
//! fake-control-client to drive the flow end-to-end.

use fake_control_client::FakeControlClient;
use pitboss_cli::control::control_socket_path;
use pitboss_cli::control::protocol::{ControlEvent, ControlOp};
use pitboss_cli::control::server::start_control_server;
use pitboss_cli::dispatch::state::{ApprovalPolicy, DispatchState, WorkerState};
use pitboss_cli::manifest::resolve::ResolvedManifest;
use pitboss_cli::manifest::schema::WorktreeCleanup;
use pitboss_core::process::{ProcessSpawner, TokioSpawner};
use pitboss_core::session::CancelToken;
use pitboss_core::store::{JsonFileStore, SessionStore};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
#[serial_test::serial(env)]
fn control_socket_path_uses_xdg_or_run_dir() {
    // Ensure the helper at least produces a valid path (regression guard for
    // Phase 1 wiring).
    std::env::remove_var("XDG_RUNTIME_DIR");
    let dir = TempDir::new().unwrap();
    let p = control_socket_path(Uuid::now_v7(), dir.path());
    assert!(p.starts_with(dir.path()));
    assert_eq!(p.file_name().unwrap(), "control.sock");
}

#[tokio::test]
async fn pause_op_writes_events_jsonl() {
    let dir = TempDir::new().unwrap();
    let (state, run_id, run_subdir) =
        make_control_test_state(&dir, ControlTestConfig::default()).await;
    let worker_token = CancelToken::new();
    state
        .root
        .workers
        .cancels
        .write()
        .await
        .insert("w-1".into(), worker_token);
    state.root.workers.states.write().await.insert(
        "w-1".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess".into()),
        },
    );

    let sock = dir.path().join("events-pause.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.4.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    let mut client = FakeControlClient::connect(&sock, "0.4.0").await.unwrap();
    client
        .send(&ControlOp::PauseWorker {
            task_id: "w-1".into(),
            mode: pitboss_cli::control::protocol::PauseMode::default(),
        })
        .await
        .unwrap();
    let ev = client
        .recv_timeout(std::time::Duration::from_secs(1))
        .await
        .unwrap()
        .expect("reply");
    assert!(matches!(ev, ControlEvent::OpAcked { .. }));

    // Assert events.jsonl was written.
    let events_path = run_subdir.join("tasks").join("w-1").join("events.jsonl");
    let contents = tokio::fs::read_to_string(&events_path).await.unwrap();
    assert!(contents.contains("\"kind\":\"pause\""));
}

use pitboss_cli::mcp::approval::ApprovalBridge;

#[tokio::test]
async fn control_event_carries_actor_path() {
    use pitboss_cli::control::protocol::{ControlEvent, EventEnvelope};
    use pitboss_cli::dispatch::actor::ActorPath;

    // Build an envelope wrapping an existing event with a deep actor path.
    let envelope = EventEnvelope {
        actor_path: ActorPath::new(["root", "S1", "W3"]),
        seq: 0,
        event: ControlEvent::Superseded,
    };

    // Serialized form must contain the actor_path field and its segments.
    let s = serde_json::to_string(&envelope).unwrap();
    assert!(
        s.contains("\"actor_path\""),
        "actor_path must be present: {s}"
    );
    assert!(s.contains("root"), "root segment must appear: {s}");
    assert!(s.contains("S1"), "S1 segment must appear: {s}");
    assert!(s.contains("W3"), "W3 segment must appear: {s}");

    // Round-trip must preserve the depth.
    let back: EventEnvelope = serde_json::from_str(&s).unwrap();
    assert_eq!(back.actor_path.depth(), 3);
}

#[tokio::test]
async fn sublead_spawned_event_emitted() {
    use pitboss_cli::control::protocol::ControlEvent;

    let event = ControlEvent::SubleadSpawned {
        sublead_id: "S1".into(),
        budget_usd: Some(5.0),
        lead_budget_usd: None,
        max_workers: Some(4),
        read_down: false,
    };
    let s = serde_json::to_string(&event).unwrap();
    // ControlEvent uses tag = "event", so the discriminator key is "event".
    assert!(
        s.contains("\"event\""),
        "discriminator key must be 'event': {s}"
    );
    assert!(
        s.contains("sublead_spawned"),
        "variant name must appear: {s}"
    );

    // Round-trip.
    let back: ControlEvent = serde_json::from_str(&s).unwrap();
    assert!(
        matches!(back, ControlEvent::SubleadSpawned { .. }),
        "round-trip must yield SubleadSpawned"
    );
}

#[tokio::test]
async fn sublead_terminated_event_roundtrips() {
    use pitboss_cli::control::protocol::ControlEvent;

    let event = ControlEvent::SubleadTerminated {
        sublead_id: "S1".into(),
        spent_usd: 2.50,
        unspent_usd: 2.50,
        outcome: pitboss_cli::control::protocol::TerminationOutcome::Success,
    };
    let s = serde_json::to_string(&event).unwrap();
    assert!(
        s.contains("sublead_terminated"),
        "variant name must appear: {s}"
    );
    assert!(s.contains("\"success\""), "outcome must appear: {s}");

    let back: ControlEvent = serde_json::from_str(&s).unwrap();
    match back {
        ControlEvent::SubleadTerminated {
            sublead_id,
            spent_usd,
            unspent_usd,
            outcome,
        } => {
            assert_eq!(sublead_id, "S1");
            assert!((spent_usd - 2.50).abs() < 1e-9);
            assert!((unspent_usd - 2.50).abs() < 1e-9);
            assert_eq!(
                outcome,
                pitboss_cli::control::protocol::TerminationOutcome::Success
            );
        }
        other => panic!("expected SubleadTerminated, got {other:?}"),
    }
}

#[tokio::test]
async fn event_envelope_empty_actor_path_omitted_on_wire() {
    use pitboss_cli::control::protocol::{ControlEvent, EventEnvelope};
    use pitboss_cli::dispatch::actor::ActorPath;

    // An envelope with an empty actor_path must serialize without the
    // actor_path key (backward-compat: v0.5 clients parse unmodified events).
    let envelope = EventEnvelope {
        actor_path: ActorPath::default(),
        seq: 0,
        event: ControlEvent::Superseded,
    };
    let s = serde_json::to_string(&envelope).unwrap();
    assert!(
        !s.contains("actor_path"),
        "empty actor_path must be omitted: {s}"
    );

    // And it must still round-trip correctly.
    let back: EventEnvelope = serde_json::from_str(&s).unwrap();
    assert!(back.actor_path.is_empty());
    assert!(matches!(back.event, ControlEvent::Superseded));
}

/// PR-B of #438: the live control wire now carries a `seq` field on
/// every envelope. The dispatcher's server emits monotonic per-connection
/// seqs starting at 1. This is the foundation for the transport-switching
/// algorithm spec'd in book/src/architecture/unified-envelope-api.md —
/// PR-C consumes it.
#[tokio::test]
async fn server_emits_monotonic_seqs_on_wire() {
    let dir = TempDir::new().unwrap();
    let (state, run_id, _run_subdir) =
        make_control_test_state(&dir, ControlTestConfig::default()).await;
    state.root.workers.states.write().await.insert(
        "w-seq".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess".into()),
        },
    );

    let sock = dir.path().join("events-seq.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.4.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    // Connect raw so we can inspect the hello envelope's seq directly,
    // bypassing FakeControlClient::connect which consumes the hello.
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r).lines();
    let client_hello = serde_json::to_string(&ControlOp::Hello {
        client_version: "0.4.0".into(),
        mode: pitboss_cli::control::protocol::ClientMode::Writer,
    })
    .unwrap()
        + "\n";
    w.write_all(client_hello.as_bytes()).await.unwrap();
    w.flush().await.unwrap();

    let hello_line = reader.next_line().await.unwrap().expect("hello");
    let hello_env: pitboss_cli::control::protocol::EventEnvelope =
        serde_json::from_str(&hello_line).unwrap();
    assert_eq!(hello_env.seq, 1, "first envelope is seq=1");
    assert!(matches!(hello_env.event, ControlEvent::Hello { .. }));

    // Send two ops back-to-back; their OpAcked / OpFailed replies must
    // carry monotonically increasing seqs.
    let list_op = serde_json::to_string(&ControlOp::ListWorkers).unwrap() + "\n";
    w.write_all(list_op.as_bytes()).await.unwrap();
    let cancel_op = serde_json::to_string(&ControlOp::CancelWorker {
        task_id: "w-seq".into(),
    })
    .unwrap()
        + "\n";
    w.write_all(cancel_op.as_bytes()).await.unwrap();
    w.flush().await.unwrap();

    // Collect three envelopes (workers snapshot, two op replies). The
    // exact order isn't pinned — the activity ticker may also fire — so
    // assert monotonicity rather than specific values.
    let mut seqs = Vec::new();
    for _ in 0..3 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), reader.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("envelope");
        let env: pitboss_cli::control::protocol::EventEnvelope =
            serde_json::from_str(&line).unwrap();
        seqs.push(env.seq);
    }
    for window in seqs.windows(2) {
        assert!(
            window[1] > window[0],
            "seqs must be strictly monotonic: saw {window:?} in {seqs:?}",
        );
    }
    assert!(
        *seqs.first().unwrap() >= 2,
        "post-hello seqs must be >= 2: {seqs:?}",
    );
}

/// PR-G of #259: when `[run].emit_event_stream = true`, the dispatcher
/// persists every non-Hello envelope to `<run-dir>/events.jsonl` as it
/// fires on the wire. Hello is skipped (handshake noise per the spec).
/// Seqs in the file match seqs on the wire (single counter for both).
#[tokio::test]
async fn emit_event_stream_writes_envelopes_to_events_jsonl() {
    let dir = TempDir::new().unwrap();
    let (state, run_id, run_subdir) = make_control_test_state(
        &dir,
        ControlTestConfig {
            emit_event_stream: true,
            ..Default::default()
        },
    )
    .await;
    state.root.workers.states.write().await.insert(
        "w-1".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess".into()),
        },
    );

    let sock = dir.path().join("events-stream.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.13.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    let mut client = FakeControlClient::connect(&sock, "0.13.0").await.unwrap();
    client.send(&ControlOp::ListWorkers).await.unwrap();
    // Wait for the OpAcked or WorkersSnapshot reply so the dispatcher
    // has flushed at least one persisted envelope.
    let _ = client
        .recv_timeout(std::time::Duration::from_secs(2))
        .await
        .unwrap();

    // Give the persistence write a beat to flush.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let events_path = run_subdir.join("events.jsonl");
    let contents = tokio::fs::read_to_string(&events_path)
        .await
        .expect("events.jsonl must be created when emit_event_stream=true");
    let lines: Vec<&str> = contents.lines().collect();
    assert!(
        !lines.is_empty(),
        "events.jsonl should have at least one persisted envelope",
    );
    // Spec: Hello is NOT archived. The hello envelope has seq=1 on the
    // wire, but persisted lines start with the second envelope.
    for line in &lines {
        assert!(
            !line.contains("\"event\":\"hello\""),
            "Hello envelope must not be archived: {line}",
        );
    }
    // Every line carries a seq field.
    for line in &lines {
        assert!(line.contains("\"seq\":"), "missing seq in {line}");
    }
}

#[tokio::test]
async fn emit_event_stream_off_does_not_create_events_jsonl() {
    let dir = TempDir::new().unwrap();
    let (state, run_id, run_subdir) =
        make_control_test_state(&dir, ControlTestConfig::default()).await;

    let sock = dir.path().join("events-off.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.13.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    let mut client = FakeControlClient::connect(&sock, "0.13.0").await.unwrap();
    client.send(&ControlOp::ListWorkers).await.unwrap();
    let _ = client
        .recv_timeout(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let events_path = run_subdir.join("events.jsonl");
    assert!(
        !events_path.exists(),
        "events.jsonl must not be created when emit_event_stream=false",
    );
}

/// PR-H of #259: headless dispatches (no control server, no client)
/// must still produce `events.jsonl` when `emit_event_stream = true`.
/// Persistence rides on a bus subscriber spawned in
/// `DispatchState::new` rather than on the per-connection pump path.
/// Without this, a hierarchical run with no operator attached would
/// silently lose its sub-lead lifecycle, worker-failure, and
/// approval-queue audit trail.
#[tokio::test]
async fn headless_broadcast_persists_events_jsonl_without_client() {
    let dir = TempDir::new().unwrap();
    let (state, _run_id, run_subdir) = make_control_test_state(
        &dir,
        ControlTestConfig {
            emit_event_stream: true,
            run_label: "lead".into(),
            ..Default::default()
        },
    )
    .await;

    // No control server. No client. Fire two broadcast events
    // directly on the root layer — the only path that exercises the
    // dispatcher-internal bus.
    state
        .root
        .broadcast_control_event(pitboss_cli::control::protocol::EventEnvelope {
            actor_path: pitboss_cli::dispatch::actor::ActorPath::default(),
            seq: 0,
            event: ControlEvent::WorkerFailed {
                task_id: "w-1".into(),
                parent_task_id: None,
                reason: pitboss_core::store::FailureReason::Unknown {
                    message: "test failure".into(),
                },
                terminate_reason: None,
            },
        })
        .await;
    state
        .root
        .broadcast_control_event(pitboss_cli::control::protocol::EventEnvelope {
            actor_path: pitboss_cli::dispatch::actor::ActorPath::default(),
            seq: 0,
            event: ControlEvent::SubleadSpawned {
                sublead_id: "sub-1".into(),
                budget_usd: Some(0.5),
                lead_budget_usd: Some(0.1),
                max_workers: Some(2),
                read_down: false,
            },
        })
        .await;

    // Wait for the persistence subscriber task to drain the bus and
    // flush both envelopes. 200 ms is conservative — broadcast send
    // is in-process and `event_log.persist` is one append per row.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let events_path = run_subdir.join("events.jsonl");
    let contents = tokio::fs::read_to_string(&events_path)
        .await
        .expect("events.jsonl must be created by the headless bus subscriber");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "expected two persisted envelopes, got: {contents}",
    );
    assert!(
        lines[0].contains("\"worker_failed\""),
        "first envelope payload: {}",
        lines[0],
    );
    assert!(
        lines[1].contains("\"sublead_spawned\""),
        "second envelope payload: {}",
        lines[1],
    );
    // Seqs come from the run-scoped EventLog and start at 1.
    assert!(lines[0].contains("\"seq\":1"), "first seq: {}", lines[0]);
    assert!(lines[1].contains("\"seq\":2"), "second seq: {}", lines[1]);
}

#[tokio::test]
async fn block_policy_queue_drains_on_tui_connect() {
    use std::time::Duration;

    let dir = TempDir::new().unwrap();
    let (state, run_id, _run_subdir) = make_control_test_state(
        &dir,
        ControlTestConfig {
            run_label: "lead".into(),
            ..Default::default()
        },
    )
    .await;

    // Kick off a blocking request on a background task (no TUI attached yet).
    let bridge = ApprovalBridge::new(state.clone());
    let req_handle = tokio::spawn(async move {
        bridge
            .request(
                "lead".into(),
                "spawn 3".into(),
                None,
                pitboss_cli::control::protocol::ApprovalKind::Action,
                Duration::from_secs(5),
                None,
                None,
            )
            .await
    });

    // Give the request a moment to queue.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(state.root.approvals.queue.lock().await.len(), 1);

    // Connect a TUI. The server drains the queue on connect.
    let sock = dir.path().join("block-drain.sock");
    let _h = pitboss_cli::control::server::start_control_server(
        sock.clone(),
        "0.4.0".into(),
        run_id.to_string(),
        "hierarchical".into(),
        state.clone(),
    )
    .await
    .unwrap();

    let mut client = fake_control_client::FakeControlClient::connect(&sock, "0.4.0")
        .await
        .unwrap();

    // Expect an ApprovalRequest event pushed from the drain.
    let ev = client
        .recv_timeout(Duration::from_secs(2))
        .await
        .unwrap()
        .expect("drain pushes approval_request");
    let request_id = match ev {
        ControlEvent::ApprovalRequest { request_id, .. } => request_id,
        other => panic!("expected ApprovalRequest, got {other:?}"),
    };

    // Respond.
    client
        .send(&ControlOp::Approve {
            request_id,
            approved: true,
            comment: None,
            edited_summary: None,
            reason: None,
        })
        .await
        .unwrap();

    // Await the original request's resolution.
    let resp = tokio::time::timeout(Duration::from_secs(3), req_handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(resp.approved);
}

#[tokio::test]
async fn auto_approve_policy_responds_without_tui() {
    use std::time::Duration;
    let dir = TempDir::new().unwrap();
    let (state, _run_id, _run_subdir) = make_control_test_state(
        &dir,
        ControlTestConfig {
            approval_policy: ApprovalPolicy::AutoApprove,
            run_label: "lead".into(),
            ..Default::default()
        },
    )
    .await;
    let bridge = ApprovalBridge::new(state);
    let resp = bridge
        .request(
            "lead".into(),
            "spawn".into(),
            None,
            pitboss_cli::control::protocol::ApprovalKind::Action,
            Duration::from_millis(200),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(resp.approved);
}

#[tokio::test]
async fn auto_reject_policy_responds_without_tui() {
    use std::time::Duration;
    let dir = TempDir::new().unwrap();
    let (state, _run_id, _run_subdir) = make_control_test_state(
        &dir,
        ControlTestConfig {
            approval_policy: ApprovalPolicy::AutoReject,
            run_label: "lead".into(),
            ..Default::default()
        },
    )
    .await;
    let bridge = ApprovalBridge::new(state);
    let resp = bridge
        .request(
            "lead".into(),
            "spawn".into(),
            None,
            pitboss_cli::control::protocol::ApprovalKind::Action,
            Duration::from_millis(200),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(!resp.approved);
    assert_eq!(
        resp.comment.as_deref(),
        Some("auto-rejected by default_approval_policy")
    );
}

#[tokio::test]
async fn propose_plan_end_to_end_unblocks_spawn_gate() {
    use pitboss_cli::control::protocol::{ApprovalKind, ApprovalPlanWire};
    use pitboss_cli::mcp::tools::{
        handle_propose_plan, handle_spawn_worker, ApprovalPlan, ProposePlanArgs, SpawnWorkerArgs,
    };
    use std::time::Duration;

    let dir = TempDir::new().unwrap();
    let (state, run_id, _run_subdir) = make_control_test_state(
        &dir,
        ControlTestConfig {
            require_plan_approval: true,
            run_label: "lead".into(),
            ..Default::default()
        },
    )
    .await;

    // Baseline: spawn_worker is gated.
    let err = handle_spawn_worker(
        &state,
        SpawnWorkerArgs {
            prompt: "early work".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
            worker_type: None,
            meta: None,
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("plan approval required"));

    // Boot the control server so an operator can attach.
    let sock = dir.path().join("plan-approval.sock");
    let _h = pitboss_cli::control::server::start_control_server(
        sock.clone(),
        "0.4.5".into(),
        run_id.to_string(),
        "hierarchical".into(),
        state.clone(),
    )
    .await
    .unwrap();
    let mut client = fake_control_client::FakeControlClient::connect(&sock, "0.4.5")
        .await
        .unwrap();

    // Drive the lead's propose_plan call on a background task.
    let state_for_plan = state.clone();
    let plan_handle = tokio::spawn(async move {
        handle_propose_plan(
            &state_for_plan,
            ProposePlanArgs {
                plan: ApprovalPlan {
                    summary: "phase-1 migration".into(),
                    rationale: Some("prep worktrees before fan-out".into()),
                    resources: vec!["3 worktrees off main".into()],
                    risks: vec![],
                    rollback: Some("drop worktrees; nothing committed".into()),
                },
                timeout_secs: Some(5),
                ..Default::default()
            },
        )
        .await
    });

    // Expect the TUI to receive an ApprovalRequest with kind=Plan.
    let ev = client
        .recv_timeout(Duration::from_secs(2))
        .await
        .unwrap()
        .expect("approval_request event");
    let (request_id, kind, plan) = match ev {
        ControlEvent::ApprovalRequest {
            request_id,
            kind,
            plan,
            ..
        } => (request_id, kind, plan),
        other => panic!("expected ApprovalRequest, got {other:?}"),
    };
    assert_eq!(kind, ApprovalKind::Plan);
    let plan = plan.expect("plan-kind requests must carry a structured plan");
    assert_eq!(
        plan,
        ApprovalPlanWire {
            summary: "phase-1 migration".into(),
            rationale: Some("prep worktrees before fan-out".into()),
            resources: vec!["3 worktrees off main".into()],
            risks: vec![],
            rollback: Some("drop worktrees; nothing committed".into()),
        }
    );

    // Operator approves.
    client
        .send(&ControlOp::Approve {
            request_id,
            approved: true,
            comment: None,
            edited_summary: None,
            reason: None,
        })
        .await
        .unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(3), plan_handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(resp.approved);
    assert!(state
        .root
        .approvals
        .plan_approved
        .load(std::sync::atomic::Ordering::Acquire));

    // Now spawn_worker no longer hits the plan-approval gate. It may
    // fail downstream (no git worktree set up) but the failure must not
    // be the plan-approval one.
    let res = handle_spawn_worker(
        &state,
        SpawnWorkerArgs {
            prompt: "post-approval work".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
            worker_type: None,
            meta: None,
        },
    )
    .await;
    if let Err(e) = &res {
        assert!(
            !e.to_string().contains("plan approval required"),
            "plan-approval gate still firing after approval: {e}"
        );
    }
}

/// PR-N of #438: the unified streaming consumer in `pitboss-core` sends
/// `Subscribe { since_seq }` right after Hello. The dispatcher acks the
/// op and continues normal event handling — so a subsequent ListWorkers
/// still round-trips. Pin the wire format + ack semantics so a future
/// server-side fast-forward implementation can drop in without
/// disturbing existing clients.
#[tokio::test]
async fn subscribe_op_acks_and_keeps_dispatcher_responsive() {
    let dir = TempDir::new().unwrap();
    let (state, run_id, _run_subdir) =
        make_control_test_state(&dir, ControlTestConfig::default()).await;

    let sock = dir.path().join("subscribe.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.4.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r).lines();

    // Hello.
    let client_hello = serde_json::to_string(&ControlOp::Hello {
        client_version: "pitboss-core/test".into(),
        mode: pitboss_cli::control::protocol::ClientMode::Writer,
    })
    .unwrap()
        + "\n";
    w.write_all(client_hello.as_bytes()).await.unwrap();
    w.flush().await.unwrap();
    let _hello = reader.next_line().await.unwrap().expect("server hello");

    // Subscribe with since_seq advisory.
    let subscribe = serde_json::to_string(&ControlOp::Subscribe { since_seq: 42 }).unwrap() + "\n";
    w.write_all(subscribe.as_bytes()).await.unwrap();
    w.flush().await.unwrap();

    // Read until we find the subscribe ack (the activity ticker may
    // also fire; tolerate intervening envelopes).
    let mut saw_ack = false;
    for _ in 0..6 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), reader.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("envelope");
        let env: pitboss_cli::control::protocol::EventEnvelope =
            serde_json::from_str(&line).unwrap();
        if let ControlEvent::OpAcked { op, .. } = &env.event {
            if op == "subscribe" {
                saw_ack = true;
                break;
            }
        }
    }
    assert!(
        saw_ack,
        "expected an OpAcked for 'subscribe' within 6 envelopes",
    );

    // Normal op handling still works after Subscribe.
    let list = serde_json::to_string(&ControlOp::ListWorkers).unwrap() + "\n";
    w.write_all(list.as_bytes()).await.unwrap();
    w.flush().await.unwrap();
    let mut saw_snapshot = false;
    for _ in 0..6 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), reader.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("envelope");
        let env: pitboss_cli::control::protocol::EventEnvelope =
            serde_json::from_str(&line).unwrap();
        if matches!(env.event, ControlEvent::WorkersSnapshot { .. }) {
            saw_snapshot = true;
            break;
        }
    }
    assert!(
        saw_snapshot,
        "Subscribe must not block subsequent op handling",
    );
}

// ----------------------------------------------------------------------
// Shared test scaffolding
// ----------------------------------------------------------------------

/// Config for `make_control_test_state`. Every field has a `Default`
/// matching the historical inline boilerplate; tests that drive a
/// specific code path override only the relevant field via struct-update
/// syntax (e.g. `ControlTestConfig { run_label: "lead".into(),
/// ..Default::default() }`).
struct ControlTestConfig {
    /// `[run].emit_event_stream` — activates `events.jsonl` persistence
    /// (PR-G/H of #259).
    emit_event_stream: bool,
    /// Drives both the manifest's `default_approval_policy` and
    /// `DispatchState::new`'s `default_approval_policy` arg. The two
    /// were always paired in practice, so a single knob keeps them in
    /// sync — diverging them was the latent footgun #536 flagged.
    approval_policy: ApprovalPolicy,
    /// `[run].require_plan_approval` — exercised by
    /// `propose_plan_end_to_end_unblocks_spawn_gate`.
    require_plan_approval: bool,
    /// 5th positional arg to `DispatchState::new` — the actor-role label
    /// for the lead. Empty in headless / writer-only tests; "lead" for
    /// tests that exercise actor-routed approval and broadcast paths.
    run_label: String,
}

impl Default for ControlTestConfig {
    fn default() -> Self {
        Self {
            emit_event_stream: false,
            approval_policy: ApprovalPolicy::Block,
            require_plan_approval: false,
            run_label: String::new(),
        }
    }
}

/// Build a minimal `DispatchState` + run id + run subdir from a config.
/// Replaces the per-test 80-line scaffolding block (29-line manifest
/// literal + 14-line `DispatchState::new` call) — see #536.
async fn make_control_test_state(
    dir: &TempDir,
    cfg: ControlTestConfig,
) -> (Arc<DispatchState>, Uuid, PathBuf) {
    let run_id = uuid::Uuid::now_v7();
    let run_subdir = dir.path().join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.unwrap();
    let manifest = ResolvedManifest {
        manifest_schema_version: 0,
        name: None,
        max_parallel_tasks: Some(4),
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: cfg.emit_event_stream,
        resource_sample_secs: 0,
        claude_setting_sources: None,
        tasks: vec![],
        lead: None,
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_budget_usd: None,
        lead_timeout_secs: None,
        default_approval_policy: Some(cfg.approval_policy),
        denial_termination_policy: None,
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: cfg.require_plan_approval,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        communication: Default::default(),
        lifecycle: None,
        worker_types: vec![],
        sublead_types: vec![],
        require_actor_type: false,
        untyped_actor_policy: Default::default(),
        agent_profiles: ::std::collections::HashMap::new(),
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    let wt_mgr = Arc::new(WorktreeManager::new());
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        cfg.run_label,
        spawner,
        PathBuf::from("/bin/true"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir.clone(),
        cfg.approval_policy,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));
    (state, run_id, run_subdir)
}

// ----------------------------------------------------------------------
// PR-P of #438 — subscriber-mode client flows.
// ----------------------------------------------------------------------

/// A second client attaching in subscriber mode must not displace the
/// connected writer: the dispatcher's `control_writer` slot stays bound
/// to the first client, no `Superseded` envelope is emitted, and the
/// writer keeps serving op replies.
#[tokio::test]
async fn subscriber_hello_does_not_supersede_writer() {
    let dir = TempDir::new().unwrap();
    let (state, run_id, _run_subdir) =
        make_control_test_state(&dir, ControlTestConfig::default()).await;

    let sock = dir.path().join("subscriber-coexist.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.4.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    // Writer attaches first.
    let mut writer = FakeControlClient::connect(&sock, "writer/0.0.0")
        .await
        .unwrap();

    // Subscriber attaches second — historically this would have
    // displaced the writer.
    let mut subscriber = FakeControlClient::connect_subscriber(&sock, "pitboss-core/0.14.0")
        .await
        .unwrap();

    // The writer must NOT receive a Superseded within a reasonable
    // window. Drain any incidental traffic (e.g. activity ticks) and
    // assert no Superseded landed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(800);
    while std::time::Instant::now() < deadline {
        match writer
            .recv_timeout(std::time::Duration::from_millis(150))
            .await
            .unwrap()
        {
            Some(ev) => {
                assert!(
                    !matches!(ev, ControlEvent::Superseded),
                    "subscriber attach must not displace writer; got {ev:?}",
                );
            }
            None => break,
        }
    }

    // Writer still serves op replies after the subscriber attached.
    writer.send(&ControlOp::ListWorkers).await.unwrap();
    let mut saw_snapshot = false;
    for _ in 0..6 {
        let ev = writer
            .recv_timeout(std::time::Duration::from_secs(2))
            .await
            .unwrap()
            .expect("envelope");
        if matches!(ev, ControlEvent::WorkersSnapshot { .. }) {
            saw_snapshot = true;
            break;
        }
    }
    assert!(saw_snapshot, "writer must keep serving ops");

    // Subscriber connection is healthy — drain one envelope so a
    // dangling task doesn't leak when the server handle drops.
    let _ = subscriber
        .recv_timeout(std::time::Duration::from_millis(50))
        .await;
}

/// Subscriber connections are read-only: any writer op must come back
/// as a typed `OpFailed` rather than mutating dispatcher state.
#[tokio::test]
async fn subscriber_writer_op_returns_op_failed() {
    let dir = TempDir::new().unwrap();
    let (state, run_id, _run_subdir) =
        make_control_test_state(&dir, ControlTestConfig::default()).await;

    // Install a worker so a misrouted CancelWorker has something to find.
    // The cancel must NOT fire — we rely on the workers.cancels token
    // staying un-cancelled to prove the op was rejected before
    // dispatch_op ran.
    let worker_token = CancelToken::new();
    state
        .root
        .workers
        .cancels
        .write()
        .await
        .insert("w-1".into(), worker_token.clone());
    state.root.workers.states.write().await.insert(
        "w-1".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess".into()),
        },
    );

    let sock = dir.path().join("subscriber-readonly.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.4.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    let mut sub = FakeControlClient::connect_subscriber(&sock, "pitboss-core/0.14.0")
        .await
        .unwrap();

    sub.send(&ControlOp::CancelWorker {
        task_id: "w-1".into(),
    })
    .await
    .unwrap();

    // Skim envelopes until we hit OpFailed for cancel_worker. Tolerate
    // intervening activity ticks.
    let mut saw_failure = false;
    for _ in 0..6 {
        let ev = sub
            .recv_timeout(std::time::Duration::from_secs(2))
            .await
            .unwrap()
            .expect("envelope");
        if let ControlEvent::OpFailed { op, error, .. } = &ev {
            if op == "cancel_worker" {
                assert!(
                    error.contains("subscriber"),
                    "error must explain subscriber-mode rejection: {error:?}",
                );
                saw_failure = true;
                break;
            }
        }
    }
    assert!(
        saw_failure,
        "writer op must return OpFailed in subscriber mode"
    );
    assert!(
        !worker_token.is_terminated(),
        "subscriber-mode op must NOT terminate the worker token",
    );
    assert!(
        !worker_token.is_draining(),
        "subscriber-mode op must NOT drain the worker token either",
    );
}

/// Multiple subscriber clients can coexist: every connection's
/// bus-bridge subscribes to the run's broadcast bus, so a single
/// `broadcast_control_event` fan-outs to all of them.
#[tokio::test]
async fn multiple_subscribers_coexist() {
    let dir = TempDir::new().unwrap();
    let (state, run_id, _run_subdir) =
        make_control_test_state(&dir, ControlTestConfig::default()).await;

    let sock = dir.path().join("multi-subscriber.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.4.0".into(),
        run_id.to_string(),
        "flat".into(),
        state.clone(),
    )
    .await
    .unwrap();

    let mut subs = Vec::with_capacity(3);
    for _ in 0..3 {
        subs.push(
            FakeControlClient::connect_subscriber(&sock, "pitboss-core/0.14.0")
                .await
                .unwrap(),
        );
    }

    // Wait briefly for every bus-bridge subscriber to attach before we
    // publish — `events_tx.subscribe()` is spawned on each connection's
    // task, so a publish too early can miss a slow subscriber. The 100ms
    // here is generous for an in-process broadcast bus.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Publish a recognizable envelope onto the broadcast bus. Using
    // RunFinished because it's a leaf event with no state dependencies
    // beyond the wire shape — perfect for fan-out assertions.
    let envelope = pitboss_cli::control::protocol::EventEnvelope {
        actor_path: pitboss_cli::dispatch::actor::ActorPath::default(),
        seq: 0, // overwritten by broadcast_control_event
        event: ControlEvent::RunFinished {
            summary: pitboss_cli::control::protocol::RunFinishedSummary {
                tasks_total: 7,
                tasks_failed: 1,
            },
        },
    };
    state.root.broadcast_control_event(envelope).await;

    for (idx, sub) in subs.iter_mut().enumerate() {
        let mut saw = false;
        for _ in 0..6 {
            let ev = sub
                .recv_timeout(std::time::Duration::from_secs(2))
                .await
                .unwrap()
                .expect("envelope");
            if let ControlEvent::RunFinished { summary } = &ev {
                assert_eq!(summary.tasks_total, 7);
                assert_eq!(summary.tasks_failed, 1);
                saw = true;
                break;
            }
        }
        assert!(saw, "subscriber {idx} did not see the broadcast event");
    }
}

/// Pre-PR-P clients send `Hello` without a `mode` field. The
/// dispatcher must continue to install them as the writer (the
/// historical behavior) — verified by observing that a second
/// no-mode-field client displaces the first with `Superseded`.
#[tokio::test]
async fn legacy_hello_without_mode_defaults_to_writer() {
    let dir = TempDir::new().unwrap();
    let (state, run_id, _run_subdir) =
        make_control_test_state(&dir, ControlTestConfig::default()).await;

    let sock = dir.path().join("legacy-hello.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.4.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    // First client: bare legacy Hello (no `mode` field).
    let stream1 = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (r1, mut w1) = stream1.into_split();
    let mut reader1 = BufReader::new(r1).lines();
    w1.write_all(b"{\"op\":\"hello\",\"client_version\":\"0.4.0\"}\n")
        .await
        .unwrap();
    w1.flush().await.unwrap();
    let line = reader1.next_line().await.unwrap().expect("server hello");
    let env: pitboss_cli::control::protocol::EventEnvelope = serde_json::from_str(&line).unwrap();
    assert!(matches!(env.event, ControlEvent::Hello { .. }));

    // Second client: same legacy Hello shape. If the first connection
    // had been classified as a subscriber, no Superseded would fire.
    let stream2 = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (_r2, mut w2) = stream2.into_split();
    w2.write_all(b"{\"op\":\"hello\",\"client_version\":\"0.4.0\"}\n")
        .await
        .unwrap();
    w2.flush().await.unwrap();

    // First client should now receive Superseded.
    let mut saw_superseded = false;
    for _ in 0..6 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), reader1.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("envelope");
        let env: pitboss_cli::control::protocol::EventEnvelope =
            serde_json::from_str(&line).unwrap();
        if matches!(env.event, ControlEvent::Superseded) {
            saw_superseded = true;
            break;
        }
    }
    assert!(
        saw_superseded,
        "legacy Hello (no mode field) must still take the writer slot",
    );
}

/// PR-Q of #438: writer-mode op replies (`OpAcked`, `OpFailed`,
/// `WorkersSnapshot`, etc.) are routed via `events_tx` so every
/// connected client — writer AND every subscriber — sees them. This
/// pins the cross-connection visibility the web SSE bridge depends on
/// after its read side switched to the unified API.
#[tokio::test]
async fn op_replies_broadcast_to_subscribers() {
    let dir = TempDir::new().unwrap();
    let (state, run_id, _run_subdir) =
        make_control_test_state(&dir, ControlTestConfig::default()).await;

    // Install a worker so ListWorkers has a non-empty snapshot to return.
    state.root.workers.states.write().await.insert(
        "w-1".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess".into()),
        },
    );
    state
        .root
        .workers
        .prompts
        .write()
        .await
        .insert("w-1".into(), "investigate failure".into());

    let sock = dir.path().join("op-reply-broadcast.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.4.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    // Writer attaches first.
    let mut writer = FakeControlClient::connect(&sock, "writer/0.0.0")
        .await
        .unwrap();
    // Subscriber attaches second; PR-P guarantees it does not displace
    // the writer.
    let mut subscriber = FakeControlClient::connect_subscriber(&sock, "pitboss-core/0.14.0")
        .await
        .unwrap();

    // Writer sends ListWorkers. The reply is broadcast on the bus, so
    // both clients should see it.
    writer.send(&ControlOp::ListWorkers).await.unwrap();

    let mut writer_saw = false;
    for _ in 0..6 {
        let ev = writer
            .recv_timeout(std::time::Duration::from_secs(2))
            .await
            .unwrap()
            .expect("writer envelope");
        if let ControlEvent::WorkersSnapshot { workers } = &ev {
            assert!(
                workers.iter().any(|w| w.task_id == "w-1"),
                "writer snapshot must include w-1: {workers:?}",
            );
            writer_saw = true;
            break;
        }
    }
    assert!(writer_saw, "writer must see its own ListWorkers reply");

    let mut subscriber_saw = false;
    for _ in 0..6 {
        let ev = subscriber
            .recv_timeout(std::time::Duration::from_secs(2))
            .await
            .unwrap()
            .expect("subscriber envelope");
        if let ControlEvent::WorkersSnapshot { workers } = &ev {
            assert!(
                workers.iter().any(|w| w.task_id == "w-1"),
                "subscriber snapshot must include w-1: {workers:?}",
            );
            subscriber_saw = true;
            break;
        }
    }
    assert!(
        subscriber_saw,
        "subscriber must see the writer's ListWorkers reply via broadcast bus",
    );
}
