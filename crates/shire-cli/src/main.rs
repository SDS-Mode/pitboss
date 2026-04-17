mod cli;
mod diff;
mod dispatch;
mod manifest;
mod tui_table;

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
        Command::Dispatch {
            manifest,
            run_dir,
            dry_run,
        } => {
            run_dispatch(&manifest, run_dir, dry_run);
        }
        Command::Resume { run_id, run_dir } => {
            run_resume(&run_id, run_dir);
        }
        Command::Diff { run_a, run_b, json } => {
            run_diff(&run_a, &run_b, json);
        }
    }
}

fn init_tracing(verbose: u8, quiet: bool) {
    use tracing_subscriber::{fmt, EnvFilter};
    let level = match (quiet, verbose) {
        (true, _) => "warn",
        (false, 0) => "info",
        (false, 1) => "debug",
        (false, _) => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("shire={level},mosaic_core={level}")));
    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn run_validate(manifest: &std::path::Path) -> Result<()> {
    let env_mp = parse_env_max_parallel();
    let r = manifest::load_manifest(manifest, env_mp)?;
    println!(
        "OK — {} tasks, max_parallel={}",
        r.tasks.len(),
        r.max_parallel
    );
    Ok(())
}

fn run_dispatch(
    manifest: &std::path::Path,
    run_dir_override: Option<std::path::PathBuf>,
    dry_run: bool,
) -> ! {
    let env_mp = parse_env_max_parallel();
    let manifest_text = match std::fs::read_to_string(manifest) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("read manifest: {e}");
            std::process::exit(2);
        }
    };
    let resolved = match manifest::load_manifest(manifest, env_mp) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("validation failed: {e:#}");
            std::process::exit(2);
        }
    };
    let claude_bin = std::env::var_os("SHIRE_CLAUDE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("claude"));

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("runtime: {e}");
            std::process::exit(2);
        }
    };

    let code = rt.block_on(async move {
        let claude_version = if dry_run {
            None
        } else {
            match dispatch::probe_claude(&claude_bin).await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("{e}");
                    return 2;
                }
            }
        };
        match dispatch::run_dispatch_inner(
            resolved,
            manifest_text,
            manifest.to_path_buf(),
            claude_bin,
            claude_version,
            run_dir_override,
            dry_run,
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("dispatch: {e:#}");
                1
            }
        }
    });
    std::process::exit(code);
}

fn parse_env_max_parallel() -> Option<u32> {
    std::env::var("ANTHROPIC_MAX_CONCURRENT")
        .ok()
        .and_then(|s| s.parse().ok())
}

fn run_diff(run_a: &str, run_b: &str, json: bool) -> ! {
    let dir_a = match diff::resolve_run(run_a) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("shire diff: run A: {e:#}");
            std::process::exit(1);
        }
    };
    let dir_b = match diff::resolve_run(run_b) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("shire diff: run B: {e:#}");
            std::process::exit(1);
        }
    };

    let summary_a = match diff::load_summary(&dir_a) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("shire diff: load run A: {e:#}");
            std::process::exit(1);
        }
    };
    let summary_b = match diff::load_summary(&dir_b) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("shire diff: load run B: {e:#}");
            std::process::exit(1);
        }
    };

    let models_a = diff::load_model_map(&dir_a);
    let models_b = diff::load_model_map(&dir_b);

    let report = diff::build_report(&summary_a, &models_a, &summary_b, &models_b);

    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("shire diff: JSON serialization: {e}");
                std::process::exit(1);
            }
        }
    } else {
        print!("{}", diff::render_human(&report));
    }

    std::process::exit(0);
}

/// Resolve a run id (full UUID or unique prefix) to an absolute run directory.
///
/// Searches `base_runs_dir` for subdirectory names whose string representation
/// starts with `run_id_prefix`. Returns an error if zero or more than one match.
fn resolve_run_dir(
    base_runs_dir: &std::path::Path,
    run_id_prefix: &str,
) -> Result<std::path::PathBuf> {
    let entries = match std::fs::read_dir(base_runs_dir) {
        Ok(e) => e,
        Err(e) => {
            anyhow::bail!(
                "cannot read runs directory {}: {e}",
                base_runs_dir.display()
            );
        }
    };

    let mut matches: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if entry.path().is_dir() && name_str.starts_with(run_id_prefix) {
            matches.push(entry.path());
        }
    }

    match matches.len() {
        0 => anyhow::bail!(
            "no run found matching prefix '{}' in {}",
            run_id_prefix,
            base_runs_dir.display()
        ),
        1 => Ok(matches.remove(0)),
        n => anyhow::bail!(
            "{n} runs match prefix '{}' — be more specific",
            run_id_prefix
        ),
    }
}

fn run_resume(run_id_prefix: &str, run_dir_override: Option<std::path::PathBuf>) -> ! {
    // Determine the base runs directory (same default as dispatch).
    let base_runs_dir = run_dir_override.clone().unwrap_or_else(|| {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".local/share/shire/runs")
    });

    let run_subdir = match resolve_run_dir(&base_runs_dir, run_id_prefix) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("shire resume: {e:#}");
            std::process::exit(2);
        }
    };

    let resolved = match dispatch::build_resume_manifest(&run_subdir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("shire resume: {e:#}");
            std::process::exit(2);
        }
    };

    let claude_bin = std::env::var_os("SHIRE_CLAUDE_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("claude"));

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("runtime: {e}");
            std::process::exit(2);
        }
    };

    let code = rt.block_on(async move {
        let claude_version = match dispatch::probe_claude(&claude_bin).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        };
        // Use the prior run's run_dir so artifacts land alongside the original.
        // run_dir_override replaces both the base and the resolved manifest's run_dir.
        let effective_run_dir = run_dir_override.unwrap_or_else(|| resolved.run_dir.clone());
        match dispatch::run_dispatch_inner(
            resolved,
            String::new(),
            std::path::PathBuf::new(),
            claude_bin,
            claude_version,
            Some(effective_run_dir),
            false,
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("dispatch: {e:#}");
                1
            }
        }
    });
    std::process::exit(code);
}
