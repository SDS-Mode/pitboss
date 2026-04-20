use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum ActorRoleArg {
    Lead,
    Worker,
}

#[derive(Debug, Parser)]
#[command(
    name = "pitboss",
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
    /// Tail a specific worker's output live (`docker logs -f` shape).
    /// Resolves a run id (full UUID or unique prefix) + task id against
    /// the run directory, then streams the worker's stdout.log with
    /// pitboss's event formatter. Exits on Ctrl-C or when the worker's
    /// record lands in `summary.jsonl`.
    Attach {
        /// Run id (full UUID or unique prefix).
        run_id: String,
        /// Task id within the run (lead id or worker task_id).
        task_id: String,
        /// Emit raw stream-json instead of the formatted event stream.
        /// Useful for piping through `jq` or capturing to a file.
        #[arg(long)]
        raw: bool,
        /// Seed with the last N historical lines before following.
        #[arg(long, default_value_t = 20)]
        lines: usize,
    },
    /// Print version information.
    Version,
    /// Print the bundled AGENTS.md reference document to stdout. Useful
    /// for agents orchestrating pitboss from environments without repo
    /// access (installed binary, container, CI runner). Content is
    /// compiled in at build time and matches the running binary's version.
    AgentsMd,
    /// Internal: proxy stdio <-> unix-socket for a claude subprocess's MCP client.
    /// Launched automatically by the `--mcp-config` file that pitboss generates
    /// per-actor. Not intended for direct human use.
    McpBridge {
        /// Path to the pitboss MCP unix socket to bridge stdio to.
        socket: PathBuf,
        /// Identity of the actor driving this bridge (lead id or worker id).
        /// Injected into every outbound MCP tool call's `_meta` field so the
        /// dispatcher can enforce namespace authz.
        #[arg(long)]
        actor_id: String,
        /// Role of the actor: `lead` or `worker`.
        #[arg(long, value_enum)]
        actor_role: ActorRoleArg,
    },
    /// Print shell completion script for the given shell (bash, zsh, fish,
    /// elvish, powershell) to stdout.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_bash_contains_pitboss_name() {
        // Smoke test: generate bash completions and confirm the output
        // references the binary name. We're not validating the script
        // content, just that the subcommand plumbing is wired correctly.
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(clap_complete::Shell::Bash, &mut cmd, "pitboss", &mut buf);
        let s = String::from_utf8(buf).unwrap();
        assert!(
            s.contains("pitboss"),
            "output should reference the binary name"
        );
        assert!(
            s.contains("complete"),
            "output should look like a completion script"
        );
    }
}
