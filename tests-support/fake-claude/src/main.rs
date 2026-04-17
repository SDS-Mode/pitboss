//! Scripted fake-claude binary for integration tests.
//!
//! Reads a JSONL script from MOSAIC_FAKE_SCRIPT, executing each action in order:
//!   {"stdout": "..."} — writes line to stdout
//!   {"stderr": "..."} — writes line to stderr
//!   {"sleep_ms": N}   — sleeps for N milliseconds
//!
//! After the script, exits with MOSAIC_FAKE_EXIT_CODE (default 0).
//! If MOSAIC_FAKE_HOLD=1, blocks indefinitely after the script (for Ctrl-C tests).
//! Special-cases --version to print "fake-claude 0.0.0".

use std::io::{self, BufRead, Write};
use std::thread;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Handle --version flag
    if args.iter().any(|a| a == "--version") {
        println!("fake-claude 0.0.0");
        return;
    }

    let exit_code: i32 = std::env::var("MOSAIC_FAKE_EXIT_CODE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let hold = std::env::var("MOSAIC_FAKE_HOLD")
        .map(|v| v.trim() == "1")
        .unwrap_or(false);

    // Execute the script if provided
    if let Ok(script_path) = std::env::var("MOSAIC_FAKE_SCRIPT") {
        let file = std::fs::File::open(&script_path)
            .unwrap_or_else(|e| panic!("fake-claude: cannot open script {script_path:?}: {e}"));
        let reader = io::BufReader::new(file);

        let stdout = io::stdout();
        let stderr = io::stderr();

        for (line_no, line) in reader.lines().enumerate() {
            let line =
                line.unwrap_or_else(|e| panic!("fake-claude: read error at line {line_no}: {e}"));
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let action: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
                panic!("fake-claude: invalid JSON at line {line_no}: {e}\n  line: {line}")
            });

            if let Some(text) = action.get("stdout").and_then(|v| v.as_str()) {
                let mut out = stdout.lock();
                writeln!(out, "{text}").unwrap();
                out.flush().unwrap();
            } else if let Some(text) = action.get("stderr").and_then(|v| v.as_str()) {
                let mut err = stderr.lock();
                writeln!(err, "{text}").unwrap();
                err.flush().unwrap();
            } else if let Some(ms) = action.get("sleep_ms").and_then(|v| v.as_u64()) {
                thread::sleep(Duration::from_millis(ms));
            } else {
                eprintln!("fake-claude: unknown action at line {line_no}: {line}");
            }
        }
    }

    if hold {
        // Block indefinitely — wait for SIGINT/SIGTERM from the test harness
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }

    std::process::exit(exit_code);
}
