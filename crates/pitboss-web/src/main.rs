//! `pitboss-web` — web operational console for the Pitboss dispatcher.
//!
//! Single-binary HTTP server. Serves a SvelteKit SPA over the same
//! port and exposes a JSON API over the run filesystem (and, in
//! later phases, the per-run control socket). Bind to `127.0.0.1`
//! by default; remote-binding requires an explicit bearer token.
//!
//! Usage:
//!
//! ```text
//! pitboss-web                   # serve (default), listen on 127.0.0.1:7077
//! pitboss-web serve --port 8080 # serve, explicit subcommand
//! pitboss-web stop              # graceful SIGTERM to the running instance
//! ```
//!
//! The default-no-subcommand path is preserved verbatim from earlier
//! versions for backward compatibility — `--port`, `--bind`, `--token`,
//! `--runs-dir`, `--manifests-dir` continue to work without `serve`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod api;
mod assets;
mod control_bridge;
mod error;
mod insights;
mod pidfile;
mod runs_index;
mod state;

use state::AppState;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Pitboss web operational console",
    long_about = "Serves a browser UI for inspecting Pitboss runs and (in later phases) \
                  driving them. Reads runs from a configurable directory and serves an \
                  embedded SvelteKit SPA over a single port."
)]
struct Cli {
    /// Subcommand. Omit to run the server in the foreground (the
    /// pre-subcommand default).
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Serve flags also accepted at the top level so existing
    /// invocations like `pitboss-web --port 8080` continue to work
    /// without the explicit `serve` subcommand.
    #[command(flatten)]
    serve_args: ServeArgs,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the HTTP server in the foreground (the default action).
    Serve(ServeArgs),
    /// Send SIGTERM to the running pitboss-web instance, identified
    /// by the PID file at `$XDG_RUNTIME_DIR/pitboss/pitboss-web.pid`.
    /// Waits up to `--timeout-secs` for graceful shutdown.
    Stop(StopArgs),
}

#[derive(Args, Debug, Default, Clone)]
struct ServeArgs {
    /// Listen port. Default 7077.
    #[arg(long, default_value_t = 7077)]
    port: u16,

    /// Bind address. Default 127.0.0.1 (loopback only). Setting this to a
    /// non-loopback address requires `--token` to be set.
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,

    /// Bearer token required on the `Authorization: Bearer <token>` header
    /// for every API request. When unset and bound to loopback, requests
    /// are accepted without auth. Required when binding non-loopback.
    #[arg(long, env = "PITBOSS_WEB_TOKEN")]
    token: Option<String>,

    /// Override the runs directory. Falls back to `PITBOSS_RUNS_DIR`, then
    /// to `pitboss_cli::runs::runs_base_dir()`. The console reads run
    /// artifacts from this directory; `pitboss dispatch` must use the
    /// same directory for the console to see its runs.
    #[arg(long, env = "PITBOSS_RUNS_DIR")]
    runs_dir: Option<PathBuf>,

    /// Override the manifests workspace directory (Phase 4+). Falls back
    /// to `PITBOSS_MANIFESTS_DIR`, then `<data_dir>/pitboss/manifests`.
    /// Saves from the console editor are sandboxed inside this dir.
    #[arg(long, env = "PITBOSS_MANIFESTS_DIR")]
    manifests_dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct StopArgs {
    /// How long to wait for graceful shutdown before reporting failure.
    /// SIGTERM is non-blocking; this controls how long we poll for the
    /// PID to disappear.
    #[arg(long, default_value_t = 10)]
    timeout_secs: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        None => run_serve(cli.serve_args).await,
        Some(Cmd::Serve(args)) => run_serve(args).await,
        Some(Cmd::Stop(args)) => run_stop(args).await,
    }
}

async fn run_serve(args: ServeArgs) -> Result<()> {
    if !is_loopback(&args.bind) && args.token.is_none() {
        anyhow::bail!(
            "binding to non-loopback address {} requires --token (or PITBOSS_WEB_TOKEN)",
            args.bind
        );
    }

    let runs_dir = args
        .runs_dir
        .clone()
        .unwrap_or_else(pitboss_cli::runs::runs_base_dir);
    let manifests_dir = args
        .manifests_dir
        .clone()
        .unwrap_or_else(default_manifests_dir);

    tracing::info!(
        runs_dir = %runs_dir.display(),
        manifests_dir = %manifests_dir.display(),
        "pitboss-web starting"
    );

    let state = AppState::new(runs_dir, manifests_dir, args.token.clone());
    let app = api::router(state);

    let addr: SocketAddr = format!("{}:{}", args.bind, args.port)
        .parse()
        .with_context(|| format!("invalid bind address {}:{}", args.bind, args.port))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {}", addr))?;

    // Write PID file *after* the bind succeeds so a failed start
    // doesn't leave a stale PID pointing at us behind. Surface a
    // friendly error if another live pitboss-web is already running.
    let pid_path = match pidfile::write_self() {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            anyhow::bail!("{e}; run `pitboss-web stop` first or use a different port");
        }
        Err(e) => {
            // Non-fatal: a missing /run dir, read-only filesystem,
            // or container without writable runtime dir shouldn't
            // block the server from starting. `stop` will report
            // "no pidfile" in that case.
            tracing::warn!(error = %e, "failed to write pidfile; `pitboss-web stop` will not work");
            PathBuf::new()
        }
    };

    eprintln!("pitboss-web listening on http://{}/", addr);
    if !pid_path.as_os_str().is_empty() {
        eprintln!("pidfile: {}", pid_path.display());
    }
    if let Some(t) = &args.token {
        eprintln!("auth required: Authorization: Bearer {}", t);
    } else {
        eprintln!("no auth (loopback only)");
    }

    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    pidfile::remove_if_self();
    serve_result?;
    Ok(())
}

async fn run_stop(args: StopArgs) -> Result<()> {
    let path = pidfile::pidfile_path();
    let pid = pidfile::read_pid(&path).with_context(|| {
        format!(
            "no pitboss-web pidfile at {} — is it running?",
            path.display()
        )
    })?;

    if !pidfile::pid_alive(pid) {
        eprintln!(
            "pitboss-web pidfile points at pid {pid} which is no longer alive; cleaning up {}",
            path.display()
        );
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }

    pidfile::send_sigterm(pid)
        .with_context(|| format!("failed to send SIGTERM to pitboss-web pid {pid}"))?;
    eprintln!("sent SIGTERM to pitboss-web (pid {pid})");

    // Poll for the process to exit. Each iteration is short so we
    // observe shutdown promptly when it happens; the outer timeout
    // bounds the total wait.
    let deadline = std::time::Instant::now() + Duration::from_secs(args.timeout_secs);
    while std::time::Instant::now() < deadline {
        if !pidfile::pid_alive(pid) {
            eprintln!("pitboss-web (pid {pid}) exited");
            // The serving process should have removed its pidfile in
            // its shutdown branch; clean up here as a safety net.
            let _ = std::fs::remove_file(&path);
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    anyhow::bail!(
        "pitboss-web (pid {pid}) did not exit within {}s; check logs or rerun with --timeout-secs",
        args.timeout_secs
    )
}

/// Block until SIGTERM or SIGINT (Ctrl-C). Used as the
/// `with_graceful_shutdown` future so axum drains in-flight requests
/// before returning.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGTERM handler");
                return;
            }
        };
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::select! {
            _ = term.recv() => tracing::info!("SIGTERM received; draining"),
            _ = ctrl_c => tracing::info!("SIGINT received; draining"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("Ctrl-C received; draining");
    }
}

fn is_loopback(addr: &str) -> bool {
    addr == "127.0.0.1" || addr == "::1" || addr == "localhost"
}

/// Default manifests workspace dir. Uses platform conventions where
/// `dirs::data_dir()` resolves them: `~/.local/share` on Linux,
/// `~/Library/Application Support` on macOS.
fn default_manifests_dir() -> PathBuf {
    if let Some(d) = dirs::data_dir() {
        d.join("pitboss").join("manifests")
    } else {
        PathBuf::from("./pitboss-manifests")
    }
}
