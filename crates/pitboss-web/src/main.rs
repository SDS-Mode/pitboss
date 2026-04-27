//! `pitboss-web` — web operational console for the Pitboss dispatcher.
//!
//! Single-binary HTTP server. Serves a SvelteKit SPA over the same
//! port and exposes a JSON API over the run filesystem (and, in
//! later phases, the per-run control socket). Bind to `127.0.0.1`
//! by default; remote-binding requires an explicit bearer token.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod api;
mod assets;
mod control_bridge;
mod error;
mod insights;
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    if !is_loopback(&cli.bind) && cli.token.is_none() {
        anyhow::bail!(
            "binding to non-loopback address {} requires --token (or PITBOSS_WEB_TOKEN)",
            cli.bind
        );
    }

    let runs_dir = cli
        .runs_dir
        .clone()
        .unwrap_or_else(pitboss_cli::runs::runs_base_dir);
    let manifests_dir = cli
        .manifests_dir
        .clone()
        .unwrap_or_else(default_manifests_dir);

    tracing::info!(
        runs_dir = %runs_dir.display(),
        manifests_dir = %manifests_dir.display(),
        "pitboss-web starting"
    );

    let state = AppState::new(runs_dir, manifests_dir, cli.token.clone());
    let app = api::router(state);

    let addr: SocketAddr = format!("{}:{}", cli.bind, cli.port)
        .parse()
        .with_context(|| format!("invalid bind address {}:{}", cli.bind, cli.port))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {}", addr))?;

    eprintln!("pitboss-web listening on http://{}/", addr);
    if let Some(t) = &cli.token {
        eprintln!("auth required: Authorization: Bearer {}", t);
    } else {
        eprintln!("no auth (loopback only)");
    }

    axum::serve(listener, app).await?;
    Ok(())
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
