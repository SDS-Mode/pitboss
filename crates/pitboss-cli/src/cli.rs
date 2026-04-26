use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
#[clap(rename_all = "snake_case")]
pub enum ActorRoleArg {
    /// Legacy v0.3-v0.5 flat-lead role. Kept for compat with
    /// `lead_spawn_args`' mcp-config emitter, which writes `--actor-role lead`.
    Lead,
    /// Depth-2 root lead. Accepted by the mcp-bridge subcommand so the
    /// server-side role gate can enforce depth-2 invariants (e.g. only
    /// RootLead may call `spawn_sublead`).
    RootLead,
    /// Depth-2 sub-tree lead. Required so `build_sublead_mcp_config`'s
    /// `--actor-role sublead` passes clap parsing; without this variant the
    /// mcp-bridge subprocess rejects argv at startup and the sub-lead's
    /// claude reports `pitboss: failed` on every MCP call. Silent depth-2
    /// break since v0.6.
    Sublead,
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
    /// Run a dispatch inside a container, assembling the docker/podman invocation
    /// from the manifest's `[container]` section. Replaces the current process
    /// with the container run (exec-style). Requires `[container]` in the manifest.
    ContainerDispatch {
        manifest: PathBuf,
        /// Override run_dir from the manifest / default.
        #[arg(long)]
        run_dir: Option<PathBuf>,
        /// Print the container run command and exit without executing.
        #[arg(long)]
        dry_run: bool,
        /// Override container runtime detection ("docker" or "podman").
        #[arg(long)]
        runtime: Option<String>,
    },
    /// Print a snapshot of all task records for a prior run.
    /// Reads summary.jsonl (in-flight) or summary.json (finalized).
    Status {
        /// Run id (full UUID or unique prefix).
        run_id: String,
        /// Override run_dir, same semantics as Dispatch.
        #[arg(long)]
        run_dir: Option<PathBuf>,
        /// Emit machine-readable JSON array instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Print shell completion script for the given shell (bash, zsh, fish,
    /// elvish, powershell) to stdout.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Sweep orphaned run directories.
    ///
    /// A run is *orphaned* when its dispatcher exited uncleanly
    /// (`SIGKILL`, OOM, segfault, host crash) and never finalized
    /// `summary.json`. By default, prune matches runs in the `Stale`
    /// state and synthesizes a Cancelled `summary.json` reflecting
    /// whatever partial state landed in `summary.jsonl`. Pass
    /// `--remove` to delete the run directory entirely instead.
    ///
    /// Defaults to dry-run; `--apply` commits the action.
    /// `--older-than 24h` filters by age so a fresh `kill -KILL` two
    /// minutes ago doesn't get swept while you're still investigating.
    Prune {
        /// Without this, prune only reports what would happen.
        #[arg(long)]
        apply: bool,
        /// Remove the run directory entirely (and the leftover XDG
        /// socket file) instead of synthesizing a Cancelled
        /// `summary.json`.
        #[arg(long)]
        remove: bool,
        /// Only prune runs older than this. Accepts `60s`, `30m`,
        /// `4h`, `1d`, or a bare number of seconds.
        #[arg(long, value_name = "DURATION", value_parser = crate::prune::parse_duration)]
        older_than: Option<std::time::Duration>,
        /// Also include runs in the `Aborted` state (no `summary.json`,
        /// no `summary.jsonl` records). Off by default — Aborted runs
        /// might be the active output of a still-spinning-up dispatcher.
        #[arg(long)]
        include_aborted: bool,
        /// Override the runs base directory. Defaults to
        /// `~/.local/share/pitboss/runs`.
        #[arg(long, value_name = "PATH")]
        runs_dir: Option<PathBuf>,
    },
    /// Emit a starter manifest TOML to stdout or a named file.
    ///
    /// Two templates: `simple` (one [lead] driving a flat worker pool) and
    /// `full` (coordinator + sub-leads + workers + commented optional
    /// sections). Both are valid v0.9 manifests once you fill in the
    /// placeholder paths and prompt. Use `pitboss validate` to verify
    /// your edits.
    ///
    /// The complete reference (every field, no defaults hidden) lives at
    /// `docs/manifest-reference.toml`; produce it with
    /// `pitboss schema --format=example`.
    Init {
        /// Output path. If omitted, prints to stdout.
        output: Option<PathBuf>,
        /// Template to emit. `simple` is the default 80% case.
        #[arg(short, long, value_enum, default_value_t = InitTemplateArg::Simple)]
        template: InitTemplateArg,
        /// Overwrite the output file if it already exists.
        #[arg(short, long)]
        force: bool,
    },
    /// Emit machine-readable views of the manifest schema.
    ///
    /// Supported formats:
    ///   * `--format=map` — markdown field map, checked in at
    ///     `docs/manifest-map.md`.
    ///   * `--format=example` — complete reference TOML with every field
    ///     present (placeholders), checked in at
    ///     `docs/manifest-reference.toml`.
    ///
    /// The `--check <path>` mode regenerates and diffs against an existing
    /// file — used in CI to catch drift.
    Schema {
        /// Output format. `map` (default) and `example` are implemented;
        /// `n8n-form` is roadmapped.
        #[arg(long, value_enum, default_value_t = SchemaFormat::Map)]
        format: SchemaFormat,
        /// If set, compare the generator output against the file at this
        /// path and exit non-zero if they differ. Otherwise prints to stdout.
        #[arg(long, value_name = "PATH")]
        check: Option<PathBuf>,
    },
}

/// Output formats supported by `pitboss schema`.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub enum SchemaFormat {
    /// Markdown manifest map (one row per field, with file:line refs).
    Map,
    /// Complete reference TOML — every field present as `key = placeholder`.
    Example,
}

/// Starter templates supported by `pitboss init`.
///
/// Mirrors [`crate::manifest::init_template::InitTemplate`] — kept separate so
/// the clap surface doesn't depend on `clap::ValueEnum` leaking into the
/// `manifest` module.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub enum InitTemplateArg {
    /// One [lead] driving a flat worker pool. Two levels deep.
    Simple,
    /// Coordinator + sub-leads + workers + commented optional sections.
    Full,
}

impl From<InitTemplateArg> for crate::manifest::init_template::InitTemplate {
    fn from(arg: InitTemplateArg) -> Self {
        match arg {
            InitTemplateArg::Simple => Self::Simple,
            InitTemplateArg::Full => Self::Full,
        }
    }
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
