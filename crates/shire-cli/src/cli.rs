use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "shire",
    version,
    about = "Headless dispatcher for parallel Claude Code agents"
)]
pub struct Cli {
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[arg(short, long, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Parse, resolve and validate a manifest. Prints report and exits.
    Validate { manifest: PathBuf },
    /// Execute a manifest.
    Dispatch {
        manifest: PathBuf,
        /// Override run_dir from the manifest / default.
        #[arg(long)]
        run_dir: Option<PathBuf>,
        /// Print the resolved claude spawn commands and exit.
        #[arg(long)]
        dry_run: bool,
    },
    /// Re-run a prior dispatch, reusing claude_session_id for each task.
    Resume {
        /// Run id (full UUID or unique prefix).
        run_id: String,
        /// Override run_dir, same semantics as Dispatch.
        #[arg(long)]
        run_dir: Option<PathBuf>,
    },
    /// Compare two prior runs side-by-side.
    Diff {
        /// First run id (prefix OK).
        run_a: String,
        /// Second run id (prefix OK).
        run_b: String,
        /// Emit machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Print version information.
    Version,
    /// Internal: proxy stdio <-> unix-socket for the lead Hobbit's MCP client.
    /// Launched automatically by the `--mcp-config` file that shire generates
    /// for hierarchical runs. Not intended for direct human use.
    McpBridge {
        /// Path to the shire MCP unix socket to bridge stdio to.
        socket: PathBuf,
    },
}
