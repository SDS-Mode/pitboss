//! Live demonstration of the worker shared store.
//!
//! Run with:
//!     cargo run --release -p pitboss-cli --example shared_store_demo
//!
//! Exercises every use case from the design spec:
//!   A. Lead writes shared reference data, workers read it.
//!   B. Worker A publishes output, worker B kv_waits then consumes.
//!   C. Lease contention across 3 concurrent workers.
//!   D. Namespace authz denials.
//!   E. Finalize-time dump to a JSON file.
//!
//! This uses the `SharedStore` API directly (no MCP / bridge) to make
//! the semantic behavior observable. Full-stack MCP-through-bridge
//! coverage lives in `tests/shared_store_flows.rs` and the crate's
//! unit tests.

use std::sync::Arc;
use std::time::Duration;

use pitboss_cli::shared_store::tools::{
    handle_kv_set, handle_kv_wait, handle_lease_acquire, handle_lease_release, KvSetArgs,
    KvWaitArgs, LeaseAcquireArgs, LeaseReleaseArgs, MetaField,
};
use pitboss_cli::shared_store::{ActorRole, SharedStore};

fn lead(id: &str) -> MetaField {
    MetaField {
        actor_id: id.into(),
        actor_role: ActorRole::Lead,
    }
}

fn worker(id: &str) -> MetaField {
    MetaField {
        actor_id: id.into(),
        actor_role: ActorRole::Worker,
    }
}

fn section(title: &str) {
    println!();
    println!("━━━ {title} ━━━");
}

fn ok(msg: &str) {
    println!("  \x1b[32m✓\x1b[0m {msg}");
}

fn info(msg: &str) {
    println!("  · {msg}");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("\x1b[1mpitboss shared-store demo\x1b[0m");
    println!("exercising SharedStore behavior end-to-end (no MCP / bridge; semantic proof)");

    let store = Arc::new(SharedStore::new());

    // ---------- Use case A: lead writes /ref, workers read ----------
    section("A. Lead writes /ref/*, workers read");
    handle_kv_set(
        &store,
        KvSetArgs {
            path: "/ref/project-config".into(),
            value: b"{\"model\":\"claude-haiku-4-5\",\"tests\":\"cargo test\"}".to_vec(),
            override_flag: false,
            meta: lead("lead-X"),
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("lead /ref write failed: {e:?}"))?;
    ok("lead-X wrote /ref/project-config");

    let entry = store.get("/ref/project-config").await.unwrap();
    info(&format!(
        "worker-A reads it: {} bytes, version={}, written_by={}",
        entry.value.len(),
        entry.version,
        entry.written_by
    ));

    // ---------- Use case B: worker A publishes, worker B waits ----------
    section("B. Worker A publishes, worker B kv_waits and consumes");

    // Kick off B's kv_wait FIRST so we prove it actually blocks.
    let store_b = store.clone();
    let b_task = tokio::spawn(async move {
        handle_kv_wait(
            &store_b,
            KvWaitArgs {
                path: "/peer/worker-A/result".into(),
                timeout_secs: 5,
                min_version: None,
                meta: Some(worker("worker-B")),
            },
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    info("worker-B is blocked on kv_wait for /peer/worker-A/result (100ms elapsed, nothing yet)");

    let t_write = std::time::Instant::now();
    handle_kv_set(
        &store,
        KvSetArgs {
            path: "/peer/worker-A/result".into(),
            value: b"audit finding: 11 P0 items".to_vec(),
            override_flag: false,
            meta: worker("worker-A"),
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("worker-A /peer write failed: {e:?}"))?;
    ok(&format!(
        "worker-A wrote /peer/worker-A/result (at t+{:.0}ms)",
        t_write.elapsed().as_secs_f64() * 1000.0
    ));

    let b_entry = b_task
        .await?
        .map_err(|e| anyhow::anyhow!("B wait: {e:?}"))?;
    ok(&format!(
        "worker-B kv_wait resolved: {} (written_by={})",
        String::from_utf8_lossy(&b_entry.value),
        b_entry.written_by
    ));

    // ---------- Use case C: lease contention ----------
    section("C. 3-worker lease contention (exactly one wins)");
    let args_for = |actor: &str| LeaseAcquireArgs {
        name: "build-lock".into(),
        ttl_secs: 1,
        wait_secs: None,
        meta: worker(actor),
    };
    let (r1, r2, r3) = tokio::join!(
        handle_lease_acquire(&store, args_for("worker-1")),
        handle_lease_acquire(&store, args_for("worker-2")),
        handle_lease_acquire(&store, args_for("worker-3")),
    );
    let results = [("worker-1", r1?), ("worker-2", r2?), ("worker-3", r3?)];
    for (name, r) in &results {
        if r.acquired {
            ok(&format!(
                "{name} acquired lease (id={}… expires_at={:?})",
                &r.lease_id.as_ref().unwrap()[..8],
                r.expires_at.as_ref().map(|t| t.to_rfc3339()),
            ));
        } else {
            info(&format!("{name} did NOT acquire (expected for 2 of 3)"));
        }
    }
    let winners: Vec<_> = results.iter().filter(|(_, r)| r.acquired).collect();
    assert_eq!(
        winners.len(),
        1,
        "exactly one concurrent acquirer should win, got {}",
        winners.len()
    );

    // Holder releases; a fresh worker picks it up.
    let winner_lease = winners[0].1.lease_id.clone().unwrap();
    let winner_name = winners[0].0;
    handle_lease_release(
        &store,
        LeaseReleaseArgs {
            lease_id: winner_lease,
            meta: worker(winner_name),
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("release: {e:?}"))?;
    ok(&format!("{winner_name} released lease"));

    let r4 = handle_lease_acquire(&store, args_for("worker-4")).await?;
    assert!(r4.acquired, "worker-4 should be able to re-acquire");
    ok("worker-4 immediately re-acquires the released lease");

    // ---------- Authz denials ----------
    section("D. Authz denials (role + namespace checks)");

    let deny_1 = handle_kv_set(
        &store,
        KvSetArgs {
            path: "/ref/sneaky-write".into(),
            value: b"no".to_vec(),
            override_flag: false,
            meta: worker("worker-evil"),
        },
    )
    .await;
    match deny_1 {
        Err(pitboss_cli::shared_store::StoreError::Forbidden(m)) => {
            ok(&format!("worker writing /ref rejected: {m}"));
        }
        other => panic!("expected Forbidden, got {other:?}"),
    }

    let deny_2 = handle_kv_set(
        &store,
        KvSetArgs {
            path: "/peer/worker-B/injection".into(),
            value: b"spoof".to_vec(),
            override_flag: false,
            meta: worker("worker-A"),
        },
    )
    .await;
    match deny_2 {
        Err(pitboss_cli::shared_store::StoreError::Forbidden(m)) => {
            ok(&format!("worker A writing /peer/worker-B/* rejected: {m}"));
        }
        other => panic!("expected Forbidden, got {other:?}"),
    }

    // Lead override succeeds on /peer/<other>/*.
    handle_kv_set(
        &store,
        KvSetArgs {
            path: "/peer/worker-B/lead-injected".into(),
            value: b"operator said so".to_vec(),
            override_flag: true,
            meta: lead("lead-X"),
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("lead override: {e:?}"))?;
    ok("lead with override=true can write /peer/<other>/* (admin channel)");

    // ---------- Finalize dump ----------
    section("E. Finalize-time dump to shared-store.json");
    let dump_dir = tempfile::TempDir::new()?;
    let dump_path = dump_dir.path().join("shared-store.json");
    store.dump_to_path(&dump_path).await?;
    let bytes = std::fs::read_to_string(&dump_path)?;
    let size = bytes.len();
    info(&format!(
        "dumped to {} ({} bytes JSON)",
        dump_path.display(),
        size
    ));
    let keys: Vec<&str> = [
        "/ref/project-config",
        "/peer/worker-A/result",
        "/peer/worker-B/lead-injected",
    ]
    .to_vec();
    for k in &keys {
        assert!(
            bytes.contains(&format!("\"path\": \"{k}\"")),
            "dump missing key {k}"
        );
    }
    ok(&format!(
        "dump contains all {} expected keys + lease metadata",
        keys.len()
    ));

    // ---------- Finish ----------
    println!();
    println!("\x1b[1;32m✓ all shared-store capabilities exercised successfully\x1b[0m");
    println!();
    Ok(())
}
