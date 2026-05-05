//! Live verifier for the Phase C `StoreActivity` extension.
//!
//! Spawns `pitboss dispatch <manifest>` as a child, watches the
//! `$XDG_RUNTIME_DIR/pitboss/` socket directory for the run's new
//! `<run-id>.control.sock`, attaches a `FakeControlClient` to it, and
//! streams `StoreActivity` events while the dispatch runs. After the
//! child exits, asserts that at least one event surfaced
//! `message_ops > 0` AND `artifact_ops > 0` for some actor — i.e. that
//! the wire-protocol extension landed end-to-end on a real claude run,
//! not just in unit tests.
//!
//! Usage:
//!
//! ```bash
//! cargo build -p comm-activity-live-check
//! ./target/debug/comm-activity-live-check examples/communication-parent-child-demo.toml
//! ```
//!
//! Exit code 0 = pass. Anything else = failure with diagnostic on stderr.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use pitboss_cli::control::protocol::ControlEvent;
use tokio::process::Command;

use fake_control_client::FakeControlClient;

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const SOCKET_WAIT: Duration = Duration::from_secs(15);
const MAX_RUN_DURATION: Duration = Duration::from_secs(180);
const POST_EXIT_DRAIN: Duration = Duration::from_secs(2);

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let manifest = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "examples/communication-parent-child-demo.toml".into());
    let pitboss_bin =
        std::env::var("PITBOSS_BIN").unwrap_or_else(|_| "./target/debug/pitboss".into());

    eprintln!("== comm-activity-live-check ==");
    eprintln!("manifest: {manifest}");
    eprintln!("binary:   {pitboss_bin}");

    let socket_dir = pick_socket_dir().context("locate socket directory")?;
    eprintln!("sockets:  {}", socket_dir.display());
    let before = list_sockets(&socket_dir);

    eprintln!("\n-- spawning dispatch --");
    let mut child = Command::new(&pitboss_bin)
        .arg("dispatch")
        .arg(&manifest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn `{pitboss_bin} dispatch {manifest}`"))?;

    let new_socket = wait_new_socket(&socket_dir, &before, SOCKET_WAIT)
        .await
        .context("waiting for run's control.sock")?;
    eprintln!("socket:   {}", new_socket.display());

    let mut client = FakeControlClient::connect(&new_socket, env!("CARGO_PKG_VERSION"))
        .await
        .context("connect to control socket")?;
    eprintln!("connected.\n");

    let mut max_kv = 0u64;
    let mut max_lease = 0u64;
    let mut max_msg = 0u64;
    let mut max_art = 0u64;
    let mut events_seen = 0u64;

    let deadline = Instant::now() + MAX_RUN_DURATION;
    eprintln!("-- streaming control events --");
    'stream: while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            eprintln!("dispatch exited: {status}; draining for {POST_EXIT_DRAIN:?}.");
            let drain_until = Instant::now() + POST_EXIT_DRAIN;
            while Instant::now() < drain_until {
                match client.recv_timeout(Duration::from_millis(200)).await {
                    Ok(Some(ev)) => record(
                        ev,
                        &mut events_seen,
                        &mut max_kv,
                        &mut max_lease,
                        &mut max_msg,
                        &mut max_art,
                    ),
                    Ok(None) => {}
                    // Dispatcher closes the socket on clean shutdown — expected here.
                    Err(_) => break,
                }
            }
            break 'stream;
        }

        match client.recv_timeout(Duration::from_secs(1)).await {
            Ok(Some(ev)) => record(
                ev,
                &mut events_seen,
                &mut max_kv,
                &mut max_lease,
                &mut max_msg,
                &mut max_art,
            ),
            Ok(None) => {}
            Err(e) => {
                // Socket closed while dispatch is still running — surface as a
                // failure unless the child is on its way out.
                eprintln!("control socket closed: {e}");
                break 'stream;
            }
        }
    }

    let _ = child.wait().await;

    println!();
    println!("== results ==");
    println!("StoreActivity events observed: {events_seen}");
    println!("max kv_ops:        {max_kv}");
    println!("max lease_ops:     {max_lease}");
    println!("max message_ops:   {max_msg}");
    println!("max artifact_ops:  {max_art}");
    println!();

    if max_msg > 0 && max_art > 0 {
        println!("PASS: Phase C surfaced message_ops AND artifact_ops on the wire.");
        Ok(())
    } else {
        bail!(
            "FAIL: missing nonzero {} (saw kv_ops={max_kv} msg_ops={max_msg} art_ops={max_art} across {events_seen} StoreActivity events)",
            if max_msg == 0 && max_art == 0 {
                "message_ops/artifact_ops"
            } else if max_msg == 0 {
                "message_ops"
            } else {
                "artifact_ops"
            }
        )
    }
}

fn record(
    ev: ControlEvent,
    events: &mut u64,
    max_kv: &mut u64,
    max_lease: &mut u64,
    max_msg: &mut u64,
    max_art: &mut u64,
) {
    if let ControlEvent::StoreActivity { counters } = ev {
        *events += 1;
        for c in &counters {
            *max_kv = (*max_kv).max(c.kv_ops);
            *max_lease = (*max_lease).max(c.lease_ops);
            *max_msg = (*max_msg).max(c.message_ops);
            *max_art = (*max_art).max(c.artifact_ops);
        }
        eprintln!(
            "[#{events}] actors={} max-so-far kv={max_kv} lease={max_lease} msg={max_msg} art={max_art}",
            counters.len()
        );
    }
}

fn pick_socket_dir() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg).join("pitboss");
        std::fs::create_dir_all(&p).ok();
        if p.exists() {
            return Ok(p);
        }
    }
    bail!("XDG_RUNTIME_DIR not set / unwritable; this checker only supports XDG-mode runs")
}

fn list_sockets(dir: &Path) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    let Ok(read) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in read.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|s| s != "sock") {
            // catch *.control.sock — extension is "sock" only when filename ends with .sock
            if p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.ends_with(".control.sock"))
            {
                out.insert(p);
            }
            continue;
        }
        out.insert(p);
    }
    out
}

async fn wait_new_socket(
    dir: &Path,
    before: &HashSet<PathBuf>,
    timeout: Duration,
) -> Result<PathBuf> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for p in list_sockets(dir) {
            if !before.contains(&p) {
                // Wait for the socket to be bind-accepting before returning.
                if tokio::net::UnixStream::connect(&p).await.is_ok() {
                    return Ok(p);
                }
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    bail!(
        "no new control.sock appeared in {} within {:?}",
        dir.display(),
        timeout
    )
}
