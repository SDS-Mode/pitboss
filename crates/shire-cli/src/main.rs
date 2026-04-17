mod cli;
mod dispatch;
mod manifest;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command};

fn main() -> Result<()> {
    let args = Cli::parse();
    init_tracing(args.verbose, args.quiet);

    match args.command {
        Command::Version => {
            println!("shire {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Validate { manifest } => run_validate(&manifest),
        Command::Dispatch { manifest, run_dir, dry_run } => {
            run_dispatch(&manifest, run_dir, dry_run)
        }
    }
}

fn init_tracing(verbose: u8, quiet: bool) {
    use tracing_subscriber::{fmt, EnvFilter};
    let level = match (quiet, verbose) {
        (true, _)   => "warn",
        (false, 0)  => "info",
        (false, 1)  => "debug",
        (false, _)  => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("shire={level},mosaic_core={level}")));
    fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
}

fn run_validate(manifest: &std::path::Path) -> Result<()> {
    let env_mp = parse_env_max_parallel();
    let r = manifest::load_manifest(manifest, env_mp)?;
    println!("OK — {} tasks, max_parallel={}", r.tasks.len(), r.max_parallel);
    Ok(())
}

fn run_dispatch(
    manifest: &std::path::Path,
    _run_dir_override: Option<std::path::PathBuf>,
    _dry_run: bool,
) -> Result<()> {
    let env_mp = parse_env_max_parallel();
    let _resolved = manifest::load_manifest(manifest, env_mp)?;
    anyhow::bail!("dispatch not yet implemented — Task 30+");
}

fn parse_env_max_parallel() -> Option<u32> {
    std::env::var("ANTHROPIC_MAX_CONCURRENT").ok().and_then(|s| s.parse().ok())
}
