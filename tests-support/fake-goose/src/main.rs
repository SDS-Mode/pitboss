use std::io::{BufRead, BufReader, Write};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

fn main() {
    if let Err(e) = run() {
        eprintln!("fake-goose: {e:#}");
        std::process::exit(5);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version") {
        println!("fake-goose 0.0.0");
        return Ok(());
    }

    if let Ok(script_path) = std::env::var("PITBOSS_FAKE_SCRIPT") {
        let file = std::fs::File::open(&script_path)
            .with_context(|| format!("cannot open script {script_path:?}"))?;
        let reader = BufReader::new(file);
        execute_script(reader)?;
    }

    if std::env::var("PITBOSS_FAKE_HOLD")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
    {
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }

    let exit_code: i32 = std::env::var("PITBOSS_FAKE_EXIT_CODE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    std::process::exit(exit_code);
}

fn execute_script<R: BufRead>(reader: R) -> Result<()> {
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();

    for (idx, line) in reader.lines().enumerate() {
        let line_no = idx + 1;
        let line = line.with_context(|| format!("read error at line {line_no}"))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let action: Value = serde_json::from_str(line)
            .with_context(|| format!("invalid JSON at line {line_no}: {line}"))?;

        if let Some(text) = action.get("stdout").and_then(|v| v.as_str()) {
            let mut out = stdout.lock();
            writeln!(out, "{text}")?;
            out.flush()?;
        } else if let Some(text) = action.get("stderr").and_then(|v| v.as_str()) {
            let mut err = stderr.lock();
            writeln!(err, "{text}")?;
            err.flush()?;
        } else if let Some(ms) = action.get("sleep_ms").and_then(|v| v.as_u64()) {
            thread::sleep(Duration::from_millis(ms));
        } else {
            anyhow::bail!("unknown action at line {line_no}: {line}");
        }
    }

    Ok(())
}
